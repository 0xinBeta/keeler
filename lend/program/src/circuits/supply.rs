use spr_program::{
    circuit::{
        CheckedTransaction, ConfidentialTransaction, DataUtxo, UniqueDataUtxo, Utxo, UtxoTrait,
    },
    input, poseidon, require, require_eq, CircuitError, ClientInputs, Field, PublicInputs,
    ShieldedAddress, TxContext, ZkProgram,
};

use super::{
    assert_intent, assert_mint, assert_note,
    math::{accrue, add, refresh, shares_for_supply, TUSD_POOL},
    purpose, reserve_address, IntentState, ReceiptState, ReserveState,
};
use crate::pool;

/// Output slot of the reserve.
pub const SUPPLY_RESERVE_SLOT: usize = 0;
/// Output slot of the receipt.
pub const SUPPLY_RECEIPT_SLOT: usize = 1;
/// Domain of the blinding seed `Poseidon(LSUP, intent blinding)`.
pub const SUPPLY_BLINDING_DOMAIN: u32 = u32::from_be_bytes(*b"LSUP");
/// The tUSD pool's smallest supply: 1 tUSD (6 decimals).
pub const TUSD_MIN_SUPPLY: u64 = 1_000_000;

/// Fills a supply intent: accrues the reserve to `now`, moves the whole
/// intent into it, and mints the maker a receipt for
/// `floor(amount x 1e12 / supply_price)` shares.
#[input]
#[derive(ZkProgram)]
pub struct Supply {
    pub private: SupplyPrivateInputs,
    pub public: SupplyPublicInputs,
}

#[input]
#[derive(ClientInputs)]
pub struct SupplyPrivateInputs {
    /// Its blinding seed must be `Poseidon(LSUP, intent blinding)`.
    pub tx_context: TxContext,
    pub intent_utxo: Utxo,
    pub intent_state: IntentState,
    pub reserve_utxo: Utxo,
    pub reserve_state: ReserveState,
    /// The keeper's fresh salt.
    pub salt: u128,
}

#[input]
#[derive(PublicInputs)]
pub struct SupplyPublicInputs {
    /// The pool PDA `[POOL_SEED, mint]` with the pool viewing key.
    pub pool_owner: ShieldedAddress,
    /// The maker's user PDA with the maker's viewing key, which the receipt is
    /// encrypted to.
    pub user_owner: ShieldedAddress,
    pub mint_hi: u128,
    pub mint_lo: u128,
    pub now: u64,
    /// The intent's expiry; the program requires the clock not past it.
    pub expiry: u64,
    /// The intent's nullifier, which the program reads from the transact.
    pub first_nullifier: Field,
}

impl Supply {
    pub fn execute(&self) -> Result<CheckedTransaction, CircuitError> {
        let p = &self.private;
        let public = &self.public;
        // The maker holds the intent opening: with the published first
        // nullifier (the intent's) it derives every output's blinding.
        require_eq(
            p.tx_context.blinding_seed,
            poseidon(&[Field::from(SUPPLY_BLINDING_DOMAIN), p.intent_utxo.blinding])?,
        );

        let mut intent = DataUtxo::new_mut(&p.intent_utxo, &p.intent_state)?.close();
        require_eq(intent.owner(), *public.user_owner.owner());
        assert_intent(&intent, purpose::SUPPLY, pool::TUSD, public.now);
        require_eq(intent.expiry, public.expiry);
        let asset = intent.asset();
        assert_mint(&asset, public.mint_hi, public.mint_lo);

        let pool_owner = public.pool_owner.owner();
        let mut reserve = UniqueDataUtxo::new_mut(
            &p.reserve_utxo,
            &reserve_address(pool_owner, &asset)?,
            &p.reserve_state,
        )?;
        assert_note(&reserve, pool_owner, &asset);

        // shortcut: one pool (tUSD); select params and minimum by pool id when a
        // second pool is added.
        accrue(&mut reserve, TUSD_POOL.fee_bps, public.now);
        let amount = intent.amount()?;
        require(amount >= TUSD_MIN_SUPPLY);
        let shares = shares_for_supply(amount, reserve.supply_price);
        reserve.supply_shares = add(reserve.supply_shares, shares);
        intent.transfer(&mut reserve, &amount)?;
        refresh(&mut reserve, &TUSD_POOL, p.salt);

        let mut receipt =
            DataUtxo::<ReceiptState>::new_init(public.user_owner.owner()).with_asset(&asset)?;
        receipt.owner = intent.maker;
        receipt.pool = pool::TUSD;
        receipt.supply_shares = shares;

        let checked = ConfidentialTransaction::new(&p.tx_context, public)
            .with(intent)
            .with(reserve)
            .with(receipt)
            .check()?;
        require_eq(checked.first_nullifier, public.first_nullifier);
        Ok(checked)
    }
}
