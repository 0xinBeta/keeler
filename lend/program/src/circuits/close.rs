//! `close`: the exit that needs nobody's help. The owner, from the browser,
//! spends a position with no debt shares and takes all its collateral.
use spr_program::{
    circuit::{CheckedTransaction, ConfidentialTransaction, DataUtxo, TokenUtxos, Utxo, UtxoTrait},
    input, require_eq, CircuitError, ClientInputs, Field, PublicInputs, ShieldedAddress, TxContext,
    ZkProgram,
};

use super::{
    vault::{assert_position, vault_rules},
    PositionState,
};

/// The collateral, to the owner.
pub const CLOSE_PAYOUT_SLOT: usize = 0;

#[input]
#[derive(ZkProgram)]
pub struct Close {
    pub private: ClosePrivateInputs,
    pub public: ClosePublicInputs,
}

#[input]
#[derive(ClientInputs)]
pub struct ClosePrivateInputs {
    /// The owner builds it, so it knows the blinding seed: no seed rule.
    pub tx_context: TxContext,
    pub position_utxo: Utxo,
    pub position_state: PositionState,
}

/// Five fields.
#[input]
#[derive(PublicInputs)]
pub struct ClosePublicInputs {
    /// The signer's user PDA (any viewing key: the check is on the owner).
    pub user_owner: ShieldedAddress,
    /// The signer's owner identity: only the owner closes.
    pub owner_identity: Field,
    pub coll_hi: u128,
    pub coll_lo: u128,
    pub vault: u64,
}

impl Close {
    pub fn execute(&self) -> Result<CheckedTransaction, CircuitError> {
        let p = &self.private;
        let public = &self.public;
        let _ = vault_rules(public.vault);
        let mut position = DataUtxo::new_mut(&p.position_utxo, &p.position_state)?.close();
        assert_position(
            &position,
            public.user_owner.owner(),
            public.coll_hi,
            public.coll_lo,
            public.vault,
        );
        let owner = position.owner;
        require_eq(owner.key().identity()?, public.owner_identity);
        require_eq(position.debt_shares, 0u64);

        let mut payout = TokenUtxos::new_init(&owner, &position.asset()).with_zero_output();
        let all = position.amount()?;
        position.transfer(&mut payout, &all)?;
        ConfidentialTransaction::new(&p.tx_context, public)
            .with(position)
            .with(payout)
            .check()
    }
}
