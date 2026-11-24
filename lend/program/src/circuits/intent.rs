use spr_program::{
    circuit::{CheckedTransaction, ConfidentialTransaction, DataUtxo, TokenUtxos, Utxo, UtxoTrait},
    input, require, require_eq, CircuitError, ClientInputs, Field, PublicInputs, ShieldedAddress,
    TxContext, ZkProgram,
};

use super::{assert_mint, purpose, IntentState};

/// Output slot of the change.
pub const INTENT_CHANGE_SLOT: usize = 0;
/// Output slot of the intent note.
pub const INTENT_SLOT: usize = 1;

/// Moves `amount` of the maker's note into an intent owned by the maker's
/// user PDA, committing the maker, purpose, target and expiry. The rest comes
/// back as change.
#[input]
#[derive(ZkProgram)]
pub struct Intent {
    pub private: IntentPrivateInputs,
    pub public: IntentPublicInputs,
}

#[input]
#[derive(ClientInputs)]
pub struct IntentPrivateInputs {
    pub tx_context: TxContext,
    /// The maker's note; its owner is the maker.
    pub source: Utxo,
    pub amount: u64,
    pub purpose: u8,
    pub target: u64,
    pub expiry: u64,
}

#[input]
#[derive(PublicInputs)]
pub struct IntentPublicInputs {
    /// The user PDA `[USER_SEED, maker wallet]` with the keeper's viewing key.
    pub user_owner: ShieldedAddress,
    /// The signer's owner identity: the maker must be the signer.
    pub maker_identity: Field,
    pub mint_hi: u128,
    pub mint_lo: u128,
}

impl Intent {
    pub fn execute(&self) -> Result<CheckedTransaction, CircuitError> {
        let p = &self.private;
        let public = &self.public;
        let mut source = TokenUtxos::new_mut(core::slice::from_ref(&p.source))?;
        let maker = source.owner();
        require_eq(maker.key().identity()?, public.maker_identity);
        let asset = source.asset();
        assert_mint(&asset, public.mint_hi, public.mint_lo);
        require(purpose::is_valid(p.purpose));
        require(p.amount != 0);

        let mut intent =
            DataUtxo::<IntentState>::new_init(public.user_owner.owner()).with_asset(&asset)?;
        source.transfer(&mut intent, &p.amount)?;
        intent.maker = maker;
        intent.purpose = p.purpose;
        intent.target = p.target;
        intent.expiry = p.expiry;

        ConfidentialTransaction::new(&p.tx_context, public)
            .with(source)
            .with(intent)
            .check()
    }
}
