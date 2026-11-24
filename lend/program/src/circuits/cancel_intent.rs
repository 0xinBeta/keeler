use spr_program::{
    circuit::{CheckedTransaction, ConfidentialTransaction, DataUtxo, TokenUtxos, Utxo, UtxoTrait},
    input, require_eq, CircuitError, ClientInputs, Field, PublicInputs, ShieldedAddress, TxContext,
    ZkProgram,
};

use super::IntentState;

/// Output slot of the refund.
pub const CANCEL_REFUND_SLOT: usize = 0;

/// Spends an intent and returns all of it to its maker. The program allows it
/// only after the public `expiry`, which must be the intent's.
#[input]
#[derive(ZkProgram)]
pub struct CancelIntent {
    pub private: CancelIntentPrivateInputs,
    pub public: CancelIntentPublicInputs,
}

#[input]
#[derive(ClientInputs)]
pub struct CancelIntentPrivateInputs {
    pub tx_context: TxContext,
    pub intent_utxo: Utxo,
    pub intent_state: IntentState,
}

#[input]
#[derive(PublicInputs)]
pub struct CancelIntentPublicInputs {
    /// The user PDA `[USER_SEED, signer]` with the intent's viewing key.
    pub user_owner: ShieldedAddress,
    /// The signer's owner identity: only the maker cancels.
    pub maker_identity: Field,
    pub expiry: u64,
}

impl CancelIntent {
    pub fn execute(&self) -> Result<CheckedTransaction, CircuitError> {
        let p = &self.private;
        let public = &self.public;
        let mut intent = DataUtxo::new_mut(&p.intent_utxo, &p.intent_state)?.close();
        require_eq(intent.owner(), *public.user_owner.owner());
        let maker = intent.maker;
        require_eq(maker.key().identity()?, public.maker_identity);
        require_eq(intent.expiry, public.expiry);

        let asset = intent.asset();
        let mut refund = TokenUtxos::new_init(&maker, &asset);
        let all = intent.amount()?;
        intent.transfer(&mut refund, &all)?;

        ConfidentialTransaction::new(&p.tx_context, public)
            .with(refund)
            .with(intent)
            .check()
    }
}
