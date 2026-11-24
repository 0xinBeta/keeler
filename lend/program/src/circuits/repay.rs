//! `repay`: a repay intent pays down the maker's position. No price, never
//! refused for being small: a repay covering the whole debt clears the
//! shares and the rest comes back as change.
use spr_program::{
    circuit::{
        CheckedTransaction, ConfidentialTransaction, DataUtxo, TokenUtxos, UniqueDataUtxo, Utxo,
        UtxoTrait,
    },
    input, require, require_eq, CircuitError, ClientInputs, Field, PublicInputs, ShieldedAddress,
    TxContext, ZkProgram,
};

use super::{
    assert_intent, assert_mint, assert_note,
    math::{accrue, refresh, repay_shares, sub, TUSD_POOL},
    purpose, reserve_address,
    vault::{
        assert_first_nullifier, assert_position, assert_seed, vault_rules, POSITION_BLINDING_DOMAIN,
    },
    IntentState, PositionState, ReserveState,
};

pub const REPAY_POSITION_SLOT: usize = 0;
pub const REPAY_RESERVE_SLOT: usize = 1;
/// What the intent paid beyond the debt, to the maker (a zero note if none).
pub const REPAY_CHANGE_SLOT: usize = 2;

#[input]
#[derive(ZkProgram)]
pub struct Repay {
    pub private: RepayPrivateInputs,
    pub public: RepayPublicInputs,
}

#[input]
#[derive(ClientInputs)]
pub struct RepayPrivateInputs {
    /// Its blinding seed must be `Poseidon(LPOS, position blinding)`.
    pub tx_context: TxContext,
    pub position_utxo: Utxo,
    pub position_state: PositionState,
    pub reserve_utxo: Utxo,
    pub reserve_state: ReserveState,
    pub intent_utxo: Utxo,
    pub intent_state: IntentState,
    /// The maker's wallet address: the change is encrypted to its viewing
    /// key. Its owner must be the intent's maker.
    pub maker_address: ShieldedAddress,
    pub salt: u128,
}

/// Ten fields.
#[input]
#[derive(PublicInputs)]
pub struct RepayPublicInputs {
    pub pool_owner: ShieldedAddress,
    /// The maker's user PDA with the maker's viewing key: owns the intent and
    /// the position.
    pub user_owner: ShieldedAddress,
    pub debt_hi: u128,
    pub debt_lo: u128,
    pub coll_hi: u128,
    pub coll_lo: u128,
    pub vault: u64,
    pub now: u64,
    /// The intent's expiry; the program requires the clock not past it.
    pub expiry: u64,
    pub first_nullifier: Field,
}

impl Repay {
    pub fn execute(&self) -> Result<CheckedTransaction, CircuitError> {
        let p = &self.private;
        let public = &self.public;
        assert_seed(&p.tx_context, &p.position_utxo, POSITION_BLINDING_DOMAIN)?;
        let _ = vault_rules(public.vault);
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

        let mut intent = DataUtxo::new_mut(&p.intent_utxo, &p.intent_state)?.close();
        require_eq(intent.owner(), *user);
        let debt_mint = intent.asset();
        assert_mint(&debt_mint, public.debt_hi, public.debt_lo);
        assert_intent(&intent, purpose::REPAY, position.position_id, public.now);
        require_eq(intent.expiry, public.expiry);
        let maker = intent.maker;
        // Only the maker's own position, and the change back to the maker.
        require_eq(position.owner, maker);
        require_eq(*p.maker_address.owner(), maker);

        let mut reserve = UniqueDataUtxo::new_mut(
            &p.reserve_utxo,
            &reserve_address(pool, &debt_mint)?,
            &p.reserve_state,
        )?;
        assert_note(&reserve, pool, &debt_mint);
        accrue(&mut reserve, TUSD_POOL.fee_bps, public.now);

        let amount = intent.amount()?;
        require(amount != 0);
        let mut change = TokenUtxos::new_init(&maker, &debt_mint).with_zero_output();
        intent.transfer(&mut change, &amount)?;
        let (left, back) = repay_shares(position.debt_shares, amount, reserve.borrow_price);
        let removed = sub(position.debt_shares, left);
        reserve.debt_shares = sub(reserve.debt_shares, removed);
        position.debt_shares = left;
        let paid = sub(amount, back);
        change.transfer(&mut reserve, &paid)?;

        refresh(&mut reserve, &TUSD_POOL, p.salt);
        let checked = ConfidentialTransaction::new(&p.tx_context, public)
            .with(position)
            .with(reserve)
            .with(intent)
            .with(change)
            .check()?;
        assert_first_nullifier(&checked, public.first_nullifier);
        Ok(checked)
    }
}
