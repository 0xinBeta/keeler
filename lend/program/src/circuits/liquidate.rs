//! `liquidate`: the keeper, from its own note of the debt token, repays `x`
//! of a position past the liquidation threshold at the public bound, pays a
//! buffer of `floor(x x penalty / 1e4)` into the reserve (with no shares),
//! and takes `floor((x + buffer) x 1e12 / bound)` of collateral, capped at
//! the position's. Unless the position was above the max limit, it may not be
//! left healthier than the threshold. The circuit does not prove `x` is the
//! largest: the keeper computes it with `core::lend::liquidate`. No owner
//! signature; the rest of the collateral stays in the position.
use spr_program::{
    circuit::{
        CheckedTransaction, ConfidentialTransaction, DataUtxo, TokenUtxos, UniqueDataUtxo, Utxo,
        UtxoTrait,
    },
    input, require, CircuitError, ClientInputs, Field, PublicInputs, ShieldedAddress, TxContext,
    ZkProgram,
};

use super::{
    assert_mint, assert_note,
    math::{
        accrue, add, debt_of, refresh, repay_shares, sub, to_u64, BPS, PRICE_SCALE,
        TUSD_POOL,
    },
    reserve_address,
    vault::{
        div_floor,
        above_max_limit, assert_first_nullifier, assert_position, assert_seed,
        at_or_past_threshold, is_liquidatable, value, vault_rules, POSITION_BLINDING_DOMAIN,
    },
    PositionState, ReserveState,
};

pub const LIQUIDATE_POSITION_SLOT: usize = 0;
pub const LIQUIDATE_RESERVE_SLOT: usize = 1;
/// The liquidator's note after paying `x + buffer`.
pub const LIQUIDATE_CHANGE_SLOT: usize = 2;
/// The collateral, to the liquidator.
pub const LIQUIDATE_PAYOUT_SLOT: usize = 3;

#[input]
#[derive(ZkProgram)]
pub struct Liquidate {
    pub private: LiquidatePrivateInputs,
    pub public: LiquidatePublicInputs,
}

#[input]
#[derive(ClientInputs)]
pub struct LiquidatePrivateInputs {
    /// Its blinding seed must be `Poseidon(LPOS, position blinding)`.
    pub tx_context: TxContext,
    pub position_utxo: Utxo,
    pub position_state: PositionState,
    pub reserve_utxo: Utxo,
    pub reserve_state: ReserveState,
    /// The liquidator's own note of the debt token; it receives the change
    /// and the collateral.
    pub funds: Utxo,
    /// Debt repaid, `x`.
    pub repay: u64,
    pub salt: u128,
}

/// Ten fields.
#[input]
#[derive(PublicInputs)]
pub struct LiquidatePublicInputs {
    pub pool_owner: ShieldedAddress,
    /// The position owner's user PDA with the owner's viewing key, which the
    /// new position is encrypted to.
    pub user_owner: ShieldedAddress,
    pub debt_hi: u128,
    pub debt_lo: u128,
    pub coll_hi: u128,
    pub coll_lo: u128,
    pub vault: u64,
    pub now: u64,
    /// The collateral price of the check and the payout; the program requires
    /// the oracle's `price + confidence` to be at most this.
    pub bound: u64,
    pub first_nullifier: Field,
}

impl Liquidate {
    pub fn execute(&self) -> Result<CheckedTransaction, CircuitError> {
        let p = &self.private;
        let public = &self.public;
        assert_seed(&p.tx_context, &p.position_utxo, POSITION_BLINDING_DOMAIN)?;
        let rules = vault_rules(public.vault);
        let v = rules.params;
        let user = public.user_owner.owner();
        let pool = public.pool_owner.owner();

        let mut position = DataUtxo::new_mut(&p.position_utxo, &p.position_state)?;
        assert_position(&position, user, public.coll_hi, public.coll_lo, public.vault);

        let mut funds = TokenUtxos::new_mut(core::slice::from_ref(&p.funds))?.with_zero_output();
        let liquidator = funds.owner();
        let debt_mint = funds.asset();
        assert_mint(&debt_mint, public.debt_hi, public.debt_lo);

        let mut reserve = UniqueDataUtxo::new_mut(
            &p.reserve_utxo,
            &reserve_address(pool, &debt_mint)?,
            &p.reserve_state,
        )?;
        assert_note(&reserve, pool, &debt_mint);
        accrue(&mut reserve, TUSD_POOL.fee_bps, public.now);

        // Past the threshold at the bound.
        let collateral = position.amount()?;
        let debt = debt_of(position.debt_shares, reserve.borrow_price);
        let worth = value(collateral, public.bound);
        require(is_liquidatable(debt, worth, &v));
        let absorb = above_max_limit(debt, worth, &v);

        // x, the buffer, and the payout at the bound, capped at the collateral.
        let x = p.repay;
        require(x != 0 && x <= debt);
        let buffer = to_u64(div_floor::<78, 14>(
            Field::from(x),
            Field::from(v.penalty_bps),
            Field::from(BPS),
        ));
        let paid_in = add(x, buffer);
        // A zero bound is refused by the division.
        let formula = to_u64(div_floor::<104, 64>(
            Field::from(paid_in),
            Field::from(PRICE_SCALE),
            Field::from(public.bound),
        ));
        let payout = if formula > collateral {
            collateral
        } else {
            formula
        };

        // Not past the threshold afterwards, unless above the max limit.
        let left = sub(debt, x);
        let worth_after = value(sub(collateral, payout), public.bound);
        require(absorb || at_or_past_threshold(left, worth_after, &v));

        // Debt shares follow the repay rule for x (x <= debt: no change).
        let (shares, _) = repay_shares(position.debt_shares, x, reserve.borrow_price);
        let removed = sub(position.debt_shares, shares);
        reserve.debt_shares = sub(reserve.debt_shares, removed);
        position.debt_shares = shares;
        funds.transfer(&mut reserve, &paid_in)?;
        let mut collateral_payout =
            TokenUtxos::new_init(&liquidator, &position.asset()).with_zero_output();
        position.transfer(&mut collateral_payout, &payout)?;

        refresh(&mut reserve, &TUSD_POOL, p.salt);
        let checked = ConfidentialTransaction::new(&p.tx_context, public)
            .with(position)
            .with(reserve)
            .with(funds)
            .with(collateral_payout)
            .check()?;
        assert_first_nullifier(&checked, public.first_nullifier);
        Ok(checked)
    }
}
