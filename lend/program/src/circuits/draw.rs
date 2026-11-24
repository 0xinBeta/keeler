//! `draw`: borrow, withdraw collateral, or both, from one position. The
//! keeper proves it, the owner signs. The output count is fixed (both
//! payouts are always real notes, zero or not), so an observer cannot tell a
//! borrow from a withdrawal.
use spr_program::{
    circuit::{
        CheckedTransaction, ConfidentialTransaction, DataUtxo, TokenUtxos, UniqueDataUtxo, Utxo,
        UtxoTrait,
    },
    input, require, require_eq, Asset, CircuitError, ClientInputs, Field, PublicInputs,
    ShieldedAddress, TxContext, ZkProgram,
};

use super::{
    assert_mint, assert_note,
    math::{self, accrue, add, borrow_shares, debt_of, refresh, TUSD_POOL},
    reserve_address,
    vault::{
        assert_first_nullifier, assert_position, assert_seed, can_draw, value, vault_rules,
        zero_or_at_least, POSITION_BLINDING_DOMAIN,
    },
    PositionState, ReserveState,
};

pub const DRAW_POSITION_SLOT: usize = 0;
pub const DRAW_RESERVE_SLOT: usize = 1;
/// The borrowed debt token, to the owner.
pub const DRAW_DEBT_SLOT: usize = 2;
/// The withdrawn collateral, to the owner.
pub const DRAW_COLLATERAL_SLOT: usize = 3;

#[input]
#[derive(ZkProgram)]
pub struct Draw {
    pub private: DrawPrivateInputs,
    pub public: DrawPublicInputs,
}

#[input]
#[derive(ClientInputs)]
pub struct DrawPrivateInputs {
    /// Its blinding seed must be `Poseidon(LPOS, position blinding)`.
    pub tx_context: TxContext,
    pub position_utxo: Utxo,
    pub position_state: PositionState,
    pub reserve_utxo: Utxo,
    pub reserve_state: ReserveState,
    pub debt_mint: Asset,
    /// The owner's wallet address: the payouts are encrypted to its viewing
    /// key. Its owner must be the position's.
    pub owner_address: ShieldedAddress,
    /// Debt token to borrow, may be zero.
    pub borrow: u64,
    /// Collateral to withdraw, may be zero.
    pub withdraw: u64,
    /// The keeper's fresh salt.
    pub salt: u128,
}

/// Eleven fields, the most a proof takes.
#[input]
#[derive(PublicInputs)]
pub struct DrawPublicInputs {
    /// The pool PDA `[POOL_SEED, debt mint]` with the pool viewing key.
    pub pool_owner: ShieldedAddress,
    /// The owner's user PDA with the owner's viewing key.
    pub user_owner: ShieldedAddress,
    /// The signer's owner identity: only the owner draws.
    pub owner_identity: Field,
    pub debt_hi: u128,
    pub debt_lo: u128,
    pub coll_hi: u128,
    pub coll_lo: u128,
    pub vault: u64,
    pub now: u64,
    /// The collateral price the health check uses (debt base units per
    /// collateral base unit x 1e12); the program requires the oracle's
    /// `price - confidence` to be at least this.
    pub bound: u64,
    pub first_nullifier: Field,
}

impl Draw {
    pub fn execute(&self) -> Result<CheckedTransaction, CircuitError> {
        let p = &self.private;
        let public = &self.public;
        assert_seed(&p.tx_context, &p.position_utxo, POSITION_BLINDING_DOMAIN)?;
        let rules = vault_rules(public.vault);
        let user = public.user_owner.owner();
        let pool = public.pool_owner.owner();

        let mut position = DataUtxo::new_mut(&p.position_utxo, &p.position_state)?;
        assert_position(
            &position,
            user,
            public.coll_hi,
            public.coll_lo,
            public.vault,
        );
        let owner = position.owner;
        require_eq(owner.key().identity()?, public.owner_identity);
        require_eq(*p.owner_address.owner(), owner);

        assert_mint(&p.debt_mint, public.debt_hi, public.debt_lo);
        let mut reserve = UniqueDataUtxo::new_mut(
            &p.reserve_utxo,
            &reserve_address(pool, &p.debt_mint)?,
            &p.reserve_state,
        )?;
        assert_note(&reserve, pool, &p.debt_mint);
        accrue(&mut reserve, TUSD_POOL.fee_bps, public.now);

        // Borrow: ceil(amount x 1e12 / borrow price) debt shares, out of idle
        // liquidity, and never past 95% utilisation.
        let borrowing = p.borrow != 0;
        require(p.borrow <= reserve.amount()?);
        let added = borrow_shares(p.borrow, reserve.borrow_price);
        reserve.debt_shares = add(reserve.debt_shares, added);
        position.debt_shares = add(position.debt_shares, added);
        let over_cap = (Field::from(math::total_debt(&reserve)) * Field::from(math::BPS)).gt::<80>(
            Field::from(math::total_supply(&reserve)) * Field::from(math::MAX_BORROW_UTIL_BPS),
        );
        require(!(borrowing && over_cap));
        let mut debt_payout = TokenUtxos::new_init(&owner, &p.debt_mint).with_zero_output();
        reserve.transfer(&mut debt_payout, &p.borrow)?;

        // Withdraw.
        let collateral_mint = position.asset();
        let mut collateral_payout =
            TokenUtxos::new_init(&owner, &collateral_mint).with_zero_output();
        position.transfer(&mut collateral_payout, &p.withdraw)?;

        // Afterwards: the minimums, and health at the public bound.
        let collateral = position.amount()?;
        let debt = debt_of(position.debt_shares, reserve.borrow_price);
        require(zero_or_at_least(collateral, rules.min_collateral));
        require(zero_or_at_least(debt, rules.min_debt));
        require(can_draw(
            debt,
            value(collateral, public.bound),
            &rules.params,
        ));

        refresh(&mut reserve, &TUSD_POOL, p.salt);
        let checked = ConfidentialTransaction::new(&p.tx_context, public)
            .with(position)
            .with(reserve)
            .with(debt_payout)
            .with(collateral_payout)
            .check()?;
        assert_first_nullifier(&checked, public.first_nullifier);
        Ok(checked)
    }
}
