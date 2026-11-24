use spr_program::{
    circuit::{
        CheckedTransaction, ConfidentialTransaction, DataUtxo, TokenUtxos, UniqueDataUtxo, Utxo,
        UtxoTrait,
    },
    input, poseidon, require, require_eq, CircuitError, ClientInputs, Field, PublicInputs,
    ShieldedAddress, TxContext, ZkProgram,
};

use super::{
    assert_mint, assert_note,
    math::{accrue, refresh, shares_to_burn, sub, TUSD_POOL},
    reserve_address, ReceiptState, ReserveState,
};
use crate::pool;

/// Output slot of the receipt for the shares left (zero shares after a full
/// redeem, so every redeem has the same outputs).
pub const REDEEM_RECEIPT_SLOT: usize = 0;
/// Output slot of the reserve.
pub const REDEEM_RESERVE_SLOT: usize = 1;
/// Output slot of the payout to the owner.
pub const REDEEM_PAYOUT_SLOT: usize = 2;
/// Domain of the blinding seed `Poseidon(LRDM, receipt blinding)`.
pub const REDEEM_BLINDING_DOMAIN: u32 = u32::from_be_bytes(*b"LRDM");

/// Pays `amount` of the reserve to a receipt's owner, who signs: accrues the
/// reserve to `now` and burns `ceil(amount x 1e12 / supply_price)` of the
/// receipt's shares.
#[input]
#[derive(ZkProgram)]
pub struct Redeem {
    pub private: RedeemPrivateInputs,
    pub public: RedeemPublicInputs,
}

#[input]
#[derive(ClientInputs)]
pub struct RedeemPrivateInputs {
    /// Its blinding seed must be `Poseidon(LRDM, receipt blinding)`.
    pub tx_context: TxContext,
    pub receipt_utxo: Utxo,
    pub receipt_state: ReceiptState,
    pub reserve_utxo: Utxo,
    pub reserve_state: ReserveState,
    pub amount: u64,
    /// The keeper's fresh salt.
    pub salt: u128,
    /// The owner's shielded address, whose viewing key the payout is
    /// encrypted to. The circuit does not read it: the payout's owner is the
    /// receipt's.
    pub owner_address: ShieldedAddress,
}

#[input]
#[derive(PublicInputs)]
pub struct RedeemPublicInputs {
    /// The pool PDA `[POOL_SEED, mint]` with the pool viewing key.
    pub pool_owner: ShieldedAddress,
    /// The signer's user PDA with the owner's viewing key, which the new
    /// receipt is encrypted to.
    pub user_owner: ShieldedAddress,
    /// The signer's owner identity: only the receipt's owner redeems.
    pub owner_identity: Field,
    pub mint_hi: u128,
    pub mint_lo: u128,
    pub now: u64,
    /// The receipt's nullifier, which the program reads from the transact.
    pub first_nullifier: Field,
}

impl Redeem {
    pub fn execute(&self) -> Result<CheckedTransaction, CircuitError> {
        let p = &self.private;
        let public = &self.public;
        // The owner holds the receipt opening: with the published first
        // nullifier (the receipt's) it derives every output's blinding.
        require_eq(
            p.tx_context.blinding_seed,
            poseidon(&[
                Field::from(REDEEM_BLINDING_DOMAIN),
                p.receipt_utxo.blinding,
            ])?,
        );

        let mut receipt = DataUtxo::new_mut(&p.receipt_utxo, &p.receipt_state)?;
        let owner = receipt.owner;
        require_eq(receipt.owner(), *public.user_owner.owner());
        require_eq(owner.key().identity()?, public.owner_identity);
        require_eq(receipt.pool, pool::TUSD);
        let asset = receipt.asset();
        assert_mint(&asset, public.mint_hi, public.mint_lo);

        let pool_owner = public.pool_owner.owner();
        let mut reserve = UniqueDataUtxo::new_mut(
            &p.reserve_utxo,
            &reserve_address(pool_owner, &asset)?,
            &p.reserve_state,
        )?;
        assert_note(&reserve, pool_owner, &asset);

        // shortcut: one pool (tUSD); select params by pool id when a second
        // pool is added.
        accrue(&mut reserve, TUSD_POOL.fee_bps, public.now);
        require(p.amount != 0);
        let burned = shares_to_burn(p.amount, reserve.supply_price);
        receipt.supply_shares = sub(receipt.supply_shares, burned);
        reserve.supply_shares = sub(reserve.supply_shares, burned);
        // Refused past the reserve's balance: only idle liquidity pays out.
        let mut payout = TokenUtxos::new_init(&owner, &asset);
        reserve.transfer(&mut payout, &p.amount)?;
        refresh(&mut reserve, &TUSD_POOL, p.salt);

        let checked = ConfidentialTransaction::new(&p.tx_context, public)
            .with(receipt)
            .with(reserve)
            .with(payout)
            .check()?;
        require_eq(checked.first_nullifier, public.first_nullifier);
        Ok(checked)
    }
}
