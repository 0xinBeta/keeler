//! `init_reserve`, `intent` and `cancel_intent`: build the transaction (which
//! runs the circuit natively, so a bad request fails here), then
//! [`Prepared::prove`] proves both sides and returns the instruction.
use anyhow::Result;
use solana_address::Address;
use solana_instruction::{AccountMeta, Instruction};
use spr_client::SprIndexer;
use spr_keypair::{ShieldedKeypair, X25519Pubkey};
use spr_program::{CompressedProof, ProgramProofInputs, ProgramTransaction, TransactionOptions};
use spr_transaction::{
    instructions::transact::SppProofInputs, Mint, SppProofOutputUtxo, WalletUtxo,
};

use keeler_lend::circuits::{
    CancelIntentClient, CancelIntentPrivateInputsClient, CancelIntentPublicInputsClient,
    InitReserveClient, InitReservePrivateInputsClient, InitReservePublicInputsClient, IntentClient,
    IntentPrivateInputsClient, IntentPublicInputsClient, IntentStateClient,
};

use crate::{
    decode, err, identity, instruction, mint_halves, pool_pda,
    proof::{self, CircuitId},
    prove_transact, tag, user_pda, CancelIntentArgs, InitReserveArgs, IntentArgs, IntentState,
};

fn wallet_address(wallet: &ShieldedKeypair) -> Result<Address> {
    wallet
        .shielded_address()
        .map_err(err)?
        .solana_address()
        .map_err(err)
}

pub(crate) type Args = Box<dyn FnOnce(CompressedProof) -> Result<Vec<u8>>>;

/// A built transaction, ready to prove.
pub struct Prepared {
    circuit: CircuitId,
    ix_tag: u8,
    args: Args,
    head: Vec<AccountMeta>,
    payer: Address,
    pdas: Vec<Address>,
    /// The encrypted SPP inputs.
    pub spp: SppProofInputs,
    pub proof_inputs: ProgramProofInputs,
    /// Every output with its blinding, in slot order.
    pub outputs: Vec<SppProofOutputUtxo>,
    /// The first input's nullifier, which the output blindings derive from.
    pub first_nullifier: [u8; 32],
}

impl Prepared {
    pub(crate) fn new(
        transaction: ProgramTransaction,
        sender: &ShieldedKeypair,
        circuit: CircuitId,
        ix_tag: u8,
        args: Args,
        head: Vec<AccountMeta>,
        pdas: Vec<Address>,
    ) -> Result<Self> {
        let finalized = transaction.finalized;
        let outputs = finalized.output_utxos().to_vec();
        let first_nullifier = finalized.first_nullifier().map_err(err)?;
        let key = sender
            .get_transaction_viewing_key(&first_nullifier)
            .map_err(err)?;
        let spp = finalized.encrypt_with_viewing_key(&key).map_err(err)?;
        Ok(Self {
            circuit,
            ix_tag,
            args,
            payer: spp.payer,
            head,
            pdas,
            spp,
            proof_inputs: transaction.proof_inputs,
            outputs,
            first_nullifier,
        })
    }

    /// Proves the SPP side through `indexer` (`wallet` completes its own
    /// inputs) and the lend circuit, and builds the instruction.
    pub fn prove(
        self,
        indexer: &SprIndexer,
        tree: Address,
        wallet: &ShieldedKeypair,
    ) -> Result<Instruction> {
        let (spp, owners) = prove_transact(indexer, self.spp, wallet)?;
        let proof = proof::prove(self.circuit, &self.proof_inputs)?;
        instruction(
            self.ix_tag,
            (self.args)(proof)?,
            self.head,
            self.payer,
            tree,
            &spp,
            &owners,
            &self.pdas,
        )
    }
}

fn options(tree_id: u16) -> TransactionOptions {
    TransactionOptions {
        blinding_seed: None,
        output_tree_id: Some(tree_id),
    }
}

/// The reserve of `mint`, created by the keeper (payer) and signed by the
/// admin, encrypted to the keeper. `now` must be within the program's window
/// of the cluster clock when it lands.
pub fn init_reserve(
    keeper: &ShieldedKeypair,
    admin: Address,
    mint: Mint,
    now: u64,
    salt: u128,
    tree_id: u16,
) -> Result<Prepared> {
    let keeper_key = wallet_address(keeper)?;
    let pool = pool_pda(&mint.asset);
    let viewing = keeper.viewing_pubkey();
    let (mint_hi, mint_lo) = mint_halves(&mint.asset);
    let transaction = InitReserveClient {
        private: InitReservePrivateInputsClient {
            tx_context: options(tree_id),
            mint,
            salt,
        },
        public: InitReservePublicInputsClient {
            pool_owner: pool.owner.address(viewing).map_err(err)?,
            mint_hi,
            mint_lo,
            now,
        },
    }
    .prepare(&keeper.shielded_address().map_err(err)?, keeper_key)
    .map_err(err)?;
    let (mint_bytes, bump) = (mint.asset.to_bytes(), pool.bump);
    let args: Args = Box::new(move |proof| {
        wincode::serialize(&InitReserveArgs {
            proof,
            mint: mint_bytes,
            pool_viewing: *viewing.as_bytes(),
            pool_bump: bump,
            now,
        })
        .map_err(err)
    });
    let head = vec![
        AccountMeta::new(keeper_key, true),
        AccountMeta::new_readonly(admin, true),
    ];
    Prepared::new(
        transaction,
        keeper,
        CircuitId::InitReserve,
        tag::INIT_RESERVE,
        args,
        head,
        vec![pool.address()],
    )
}

/// Moves `amount` of `source` (the user's note of `mint`) into an intent for
/// `purpose` / `target` until `expiry`, owned by the user PDA under
/// `viewing` (the keeper's, so it can fill it). The user pays.
#[allow(clippy::too_many_arguments)]
pub fn intent(
    user: &ShieldedKeypair,
    source: WalletUtxo,
    mint: &Address,
    amount: u64,
    purpose: u8,
    target: u64,
    expiry: u64,
    viewing: X25519Pubkey,
    tree_id: u16,
) -> Result<Prepared> {
    let user_key = wallet_address(user)?;
    let pda = user_pda(&user_key);
    let (mint_hi, mint_lo) = mint_halves(mint);
    let transaction = IntentClient {
        private: IntentPrivateInputsClient {
            tx_context: options(tree_id),
            source,
            amount,
            purpose,
            target,
            expiry,
        },
        public: IntentPublicInputsClient {
            user_owner: pda.owner.address(viewing).map_err(err)?,
            maker_identity: identity(&user_key)?,
            mint_hi,
            mint_lo,
        },
    }
    .prepare(&user.shielded_address().map_err(err)?, user_key)
    .map_err(err)?;
    let (mint_bytes, bump) = (mint.to_bytes(), pda.bump);
    let args: Args = Box::new(move |proof| {
        wincode::serialize(&IntentArgs {
            proof,
            mint: mint_bytes,
            viewing: *viewing.as_bytes(),
            user_bump: bump,
        })
        .map_err(err)
    });
    let head = vec![AccountMeta::new(user_key, true)];
    Prepared::new(
        transaction,
        user,
        CircuitId::Intent,
        tag::INTENT,
        args,
        head,
        vec![pda.address()],
    )
}

/// Returns an expired intent (a note the user PDA owns under `viewing`) to
/// its maker, `user`, who pays.
pub fn cancel_intent(
    user: &ShieldedKeypair,
    intent: WalletUtxo,
    viewing: X25519Pubkey,
    tree_id: u16,
) -> Result<Prepared> {
    let user_key = wallet_address(user)?;
    let pda = user_pda(&user_key);
    let state: IntentState = decode(&intent)?;
    let expiry = state.expiry;
    let transaction = CancelIntentClient {
        private: CancelIntentPrivateInputsClient {
            tx_context: options(tree_id),
            intent_utxo: intent,
            intent_state: IntentStateClient {
                maker: state.maker,
                purpose: state.purpose,
                target: state.target,
                expiry,
            },
        },
        public: CancelIntentPublicInputsClient {
            user_owner: pda.owner.address(viewing).map_err(err)?,
            maker_identity: identity(&user_key)?,
            expiry,
        },
    }
    .prepare(&user.shielded_address().map_err(err)?, user_key)
    .map_err(err)?;
    let bump = pda.bump;
    let args: Args = Box::new(move |proof| {
        wincode::serialize(&CancelIntentArgs {
            proof,
            viewing: *viewing.as_bytes(),
            user_bump: bump,
            expiry,
        })
        .map_err(err)
    });
    let head = vec![AccountMeta::new(user_key, true)];
    Prepared::new(
        transaction,
        user,
        CircuitId::CancelIntent,
        tag::CANCEL_INTENT,
        args,
        head,
        vec![pda.address()],
    )
}
