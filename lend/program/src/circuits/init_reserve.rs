use spr_program::{
    circuit::{CheckedTransaction, ConfidentialTransaction, UniqueDataUtxo},
    input, Asset, CircuitError, ClientInputs, PublicInputs, ShieldedAddress, TxContext, ZkProgram,
};

use super::{
    assert_mint,
    math::{refresh_rate, PRICE_SCALE, TUSD_POOL},
    reserve_seed, ReserveState,
};

/// Creates the mint's reserve at its unique address: no shares, both prices
/// at 1e12, the curve's rate at zero utilisation, `last_update = now`.
#[input]
#[derive(ZkProgram)]
pub struct InitReserve {
    pub private: InitReservePrivateInputs,
    pub public: InitReservePublicInputs,
}

#[input]
#[derive(ClientInputs)]
pub struct InitReservePrivateInputs {
    pub tx_context: TxContext,
    pub mint: Asset,
    /// The keeper's fresh salt.
    pub salt: u128,
}

#[input]
#[derive(PublicInputs)]
pub struct InitReservePublicInputs {
    /// The pool PDA `[POOL_SEED, mint]` with the keeper's viewing key.
    pub pool_owner: ShieldedAddress,
    pub mint_hi: u128,
    pub mint_lo: u128,
    pub now: u64,
}

impl InitReserve {
    pub fn execute(&self) -> Result<CheckedTransaction, CircuitError> {
        let p = &self.private;
        let public = &self.public;
        assert_mint(&p.mint, public.mint_hi, public.mint_lo);
        let mut reserve = UniqueDataUtxo::<ReserveState>::new_init(
            public.pool_owner.owner(),
            &reserve_seed(&p.mint)?,
        )?
        .with_asset(&p.mint)?;
        reserve.supply_price = PRICE_SCALE;
        reserve.borrow_price = PRICE_SCALE;
        reserve.last_update = public.now;
        // shortcut: one pool (tUSD); select the curve by pool id when a second
        // pool is added.
        refresh_rate(&mut reserve, &TUSD_POOL);
        reserve.salt = p.salt;
        ConfidentialTransaction::new(&p.tx_context, public)
            .with(reserve)
            .check()
    }
}
