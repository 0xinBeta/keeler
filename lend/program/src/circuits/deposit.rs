//! `deposit`: an intent's collateral opens a position (`DepositOpen`) or is
//! added to one (`DepositAdd`). A data input cannot be optional (SPIKE.md
//! item 3), so each case is its own circuit. No reserve and no price: the
//! keeper builds it and nothing refuses a deposit of at least the minimum.
use spr_program::{
    circuit::{CheckedTransaction, ConfidentialTransaction, DataUtxo, Utxo, UtxoTrait},
    input, require, require_eq, CircuitError, ClientInputs, Field, PublicInputs, ShieldedAddress,
    TxContext, ZkProgram,
};

use super::{
    assert_intent, assert_mint,
    math::add,
    purpose,
    vault::{
        assert_first_nullifier, assert_position, assert_seed, vault_rules, DEPOSIT_BLINDING_DOMAIN,
        POSITION_BLINDING_DOMAIN,
    },
    IntentState, PositionState,
};

/// Output slot of the position (both circuits).
pub const DEPOSIT_POSITION_SLOT: usize = 0;

/// Opens position `intent.target` for the intent's maker with all of the
/// intent's collateral and no debt.
#[input]
#[derive(ZkProgram)]
pub struct DepositOpen {
    pub private: DepositOpenPrivateInputs,
    pub public: DepositOpenPublicInputs,
}

#[input]
#[derive(ClientInputs)]
pub struct DepositOpenPrivateInputs {
    /// Its blinding seed must be `Poseidon(LDEP, intent blinding)`.
    pub tx_context: TxContext,
    pub intent_utxo: Utxo,
    pub intent_state: IntentState,
}

/// Seven fields, for both circuits.
#[input]
#[derive(PublicInputs)]
pub struct DepositOpenPublicInputs {
    /// The maker's user PDA with the maker's viewing key: owns the intent and
    /// the position, which is encrypted to that key.
    pub user_owner: ShieldedAddress,
    pub coll_hi: u128,
    pub coll_lo: u128,
    pub vault: u64,
    pub now: u64,
    /// The intent's expiry; the program requires the clock not past it.
    pub expiry: u64,
    /// The first input's nullifier (the intent's for `DepositOpen`, the
    /// position's for `DepositAdd`), which the program reads from the
    /// transact.
    pub first_nullifier: Field,
}

/// Spends a deposit intent made for this vault's collateral; returns it
/// closed and the intent's maker.
fn deposit_intent(
    intent_utxo: &Utxo,
    intent_state: &IntentState,
    public_owner: &ShieldedAddress,
    coll_hi: u128,
    coll_lo: u128,
    now: u64,
    expiry: u64,
) -> Result<DataUtxo<IntentState>, CircuitError> {
    let intent = DataUtxo::new_mut(intent_utxo, intent_state)?.close();
    require_eq(intent.owner(), *public_owner.owner());
    assert_mint(&intent.asset(), coll_hi, coll_lo);
    require_eq(intent.purpose, purpose::DEPOSIT);
    require(now <= intent.expiry);
    require_eq(intent.expiry, expiry);
    Ok(intent)
}

impl DepositOpen {
    pub fn execute(&self) -> Result<CheckedTransaction, CircuitError> {
        let p = &self.private;
        let public = &self.public;
        assert_seed(&p.tx_context, &p.intent_utxo, DEPOSIT_BLINDING_DOMAIN)?;
        let rules = vault_rules(public.vault);
        let mut intent = deposit_intent(
            &p.intent_utxo,
            &p.intent_state,
            &public.user_owner,
            public.coll_hi,
            public.coll_lo,
            public.now,
            public.expiry,
        )?;
        let asset = intent.asset();
        let mut position =
            DataUtxo::<PositionState>::new_init(public.user_owner.owner()).with_asset(&asset)?;
        let amount = intent.amount()?;
        require(amount >= rules.min_collateral);
        intent.transfer(&mut position, &amount)?;
        position.owner = intent.maker;
        position.vault = public.vault;
        position.position_id = intent.target;
        position.debt_shares = 0;

        let checked = ConfidentialTransaction::new(&p.tx_context, public)
            .with(intent)
            .with(position)
            .check()?;
        assert_first_nullifier(&checked, public.first_nullifier);
        Ok(checked)
    }
}

/// Adds all of a deposit intent to the maker's position `intent.target`.
#[input]
#[derive(ZkProgram)]
pub struct DepositAdd {
    pub private: DepositAddPrivateInputs,
    pub public: DepositOpenPublicInputs,
}

#[input]
#[derive(ClientInputs)]
pub struct DepositAddPrivateInputs {
    /// Its blinding seed must be `Poseidon(LPOS, position blinding)`.
    pub tx_context: TxContext,
    pub position_utxo: Utxo,
    pub position_state: PositionState,
    pub intent_utxo: Utxo,
    pub intent_state: IntentState,
}

impl DepositAdd {
    pub fn execute(&self) -> Result<CheckedTransaction, CircuitError> {
        let p = &self.private;
        let public = &self.public;
        assert_seed(&p.tx_context, &p.position_utxo, POSITION_BLINDING_DOMAIN)?;
        let rules = vault_rules(public.vault);
        let user = public.user_owner.owner();
        let mut position = DataUtxo::new_mut(&p.position_utxo, &p.position_state)?;
        assert_position(
            &position,
            user,
            public.coll_hi,
            public.coll_lo,
            public.vault,
        );
        let mut intent = deposit_intent(
            &p.intent_utxo,
            &p.intent_state,
            &public.user_owner,
            public.coll_hi,
            public.coll_lo,
            public.now,
            public.expiry,
        )?;
        // Credited only to the maker's own position, the one the intent names.
        require_eq(position.owner, intent.maker);
        assert_intent(&intent, purpose::DEPOSIT, position.position_id, public.now);
        let amount = intent.amount()?;
        let total = add(position.amount()?, amount);
        require(total >= rules.min_collateral);
        intent.transfer(&mut position, &amount)?;

        let checked = ConfidentialTransaction::new(&p.tx_context, public)
            .with(position)
            .with(intent)
            .check()?;
        assert_first_nullifier(&checked, public.first_nullifier);
        Ok(checked)
    }
}
