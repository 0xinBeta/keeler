//! Client SDK for Keeler Lend: PDAs, note decoding, output hashes, Xark
//! proofs and the instruction builders.
pub mod builders;
pub mod proof;
pub mod vault;

use anyhow::{ensure, Result};
use solana_address::Address;
use solana_instruction::{AccountMeta, Instruction};
use spr_client::{ProofAuthority, SprIndexer, TransferInput};
use spr_interface::{
    instruction::instruction_data::transact::TransactIxData, SHIELDED_POOL_PROGRAM_ID,
};
use spr_keypair::ShieldedKeypair;
use spr_program::{instruction::nullifier_pda_accounts, DataUtxo, Field, ProgramOwner};
use spr_transaction::{instructions::transact::SppProofInputs, WalletUtxo};

/// The program's wire types and helpers.
pub use keeler_lend::{
    circuits::{purpose, reserve_address, IntentState, PositionState, ReceiptState, ReserveState},
    client::{note_data_hash, output_blinding, output_hash, NoteKind},
    instructions::{
        cancel_intent::CancelIntentArgs, init_reserve::InitReserveArgs, intent::IntentArgs,
        redeem::RedeemArgs, supply::SupplyArgs,
    },
    tag, ADMIN, ID, KEEPER, POOL_SEED, USER_SEED,
};

pub(crate) fn err(e: impl core::fmt::Debug) -> anyhow::Error {
    anyhow::anyhow!("{e:?}")
}

/// A program PDA that owns notes: its owner (the zero nullifier key) and the
/// canonical bump the instructions take.
pub struct Pda {
    pub owner: ProgramOwner,
    pub bump: u8,
}

impl Pda {
    fn new(seed: &[u8], key: &[u8; 32]) -> Self {
        Self {
            owner: ProgramOwner::find(&[seed, key], &ID),
            bump: Address::find_program_address(&[seed, key], &ID).1,
        }
    }

    pub fn address(&self) -> Address {
        *self.owner.pda()
    }
}

/// `[POOL_SEED, mint]`: owns the mint's reserve.
pub fn pool_pda(mint: &Address) -> Pda {
    Pda::new(POOL_SEED, mint.as_array())
}

/// `[USER_SEED, wallet]`: owns the wallet's intents, receipts and positions.
pub fn user_pda(wallet: &Address) -> Pda {
    Pda::new(USER_SEED, wallet.as_array())
}

/// A wallet's owner identity as a circuit field: what the program computes
/// from a signer.
pub fn identity(wallet: &Address) -> Result<Field> {
    let bytes = spr_hasher::primitives::solana_owner_identity(wallet.as_array()).map_err(err)?;
    spr_program::from_bytes(&bytes).map_err(err)
}

/// The mint as the circuits' two big-endian halves.
pub fn mint_halves(mint: &Address) -> (u128, u128) {
    let bytes = mint.to_bytes();
    let (hi, lo) = bytes.split_at(16);
    let half = |b: &[u8]| u128::from_be_bytes(b.try_into().expect("16 bytes"));
    (half(hi), half(lo))
}

/// Decodes a note's state, checking it against the note's data hash.
pub fn decode<T>(utxo: &WalletUtxo) -> Result<T>
where
    DataUtxo<T>: TryFrom<WalletUtxo>,
    <DataUtxo<T> as TryFrom<WalletUtxo>>::Error: core::fmt::Debug,
{
    Ok(DataUtxo::<T>::try_from(utxo.clone()).map_err(err)?.data)
}

/// Inputs a PDA owns complete under the zero nullifier key, the rest under
/// the wallet.
pub struct Authority<'a>(pub &'a ShieldedKeypair);

impl ProofAuthority for Authority<'_> {
    fn complete_inputs(&self, inputs: &mut [TransferInput]) -> Result<(), spr_client::ClientError> {
        for input in inputs.iter_mut() {
            if input.nullifier_secret.is_some() {
                continue;
            }
            let input = core::slice::from_mut(input);
            if ProgramOwner::nullifier_key()
                .complete_inputs(input)
                .is_err()
            {
                self.0.complete_inputs(input)?;
            }
        }
        Ok(())
    }
}

/// Proves the SPP side through `indexer` and checks it covers the same
/// transaction as the program proof.
pub fn prove_transact(
    indexer: &SprIndexer,
    spp: SppProofInputs,
    wallet: &ShieldedKeypair,
) -> Result<(TransactIxData, Vec<Address>)> {
    let owners = spp.owner_signer_pubkeys().map_err(err)?;
    let hash = spp.padding_independent_private_tx_hash().map_err(err)?;
    let ix = indexer
        .prove_transact(spp, &Authority(wallet))
        .map_err(err)?;
    ensure!(
        ix.private_tx_hash == hash,
        "the SPP proof and the lend proof cover different transactions"
    );
    Ok((ix, owners))
}

/// `[tag] ++ args ++ transact`, with `head` then the SPP transact accounts.
/// The PDAs among the owner signers go in as non-signers: the program signs
/// for them.
#[allow(clippy::too_many_arguments)]
pub fn instruction(
    ix_tag: u8,
    args: Vec<u8>,
    head: Vec<AccountMeta>,
    payer: Address,
    tree: Address,
    spp: &TransactIxData,
    owners: &[Address],
    pdas: &[Address],
) -> Result<Instruction> {
    let mut accounts = head;
    accounts.extend([
        AccountMeta::new(payer, true),
        AccountMeta::new(tree, false),
        AccountMeta::new_readonly(Address::new_from_array(SHIELDED_POOL_PROGRAM_ID), false),
        AccountMeta::new_readonly(Address::default(), false),
    ]);
    accounts.extend(
        spp.tree_contexts
            .iter()
            .map(|_| AccountMeta::new(tree, false)),
    );
    accounts.extend(nullifier_pda_accounts(
        &tree,
        spp.inputs.iter().map(|input| &input.nullifier_hash),
    ));
    for owner in owners {
        accounts.push(AccountMeta::new_readonly(*owner, !pdas.contains(owner)));
    }
    let mut data = vec![ix_tag];
    data.extend(args);
    data.extend(wincode::serialize(spp).map_err(err)?);
    Ok(Instruction {
        program_id: ID,
        accounts,
        data,
    })
}
