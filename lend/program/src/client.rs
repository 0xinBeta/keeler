//! What a client needs to check a note it did not build: the blinding of an
//! output slot, a state's data hash, and the output hash SPP appends.
use borsh::BorshDeserialize;
use solana_address::Address;
use spr_keypair::ShieldedAddress;
use spr_program::hasher::{data_hash, unique_data_hash, DataHasher};
use spr_transaction::{
    asset::Mint,
    utxo::{derive_output_blinding_seed, derive_transact_output_blinding, SppProofOutputUtxo},
};

use crate::circuits::{IntentState, PositionState, ReceiptState, ReserveState};

/// The note kinds, for [`note_data_hash`] and the browser exports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum NoteKind {
    Reserve = 1,
    Receipt = 2,
    Position = 3,
    Intent = 4,
}

impl NoteKind {
    pub fn from_u8(kind: u8) -> Option<Self> {
        Some(match kind {
            1 => Self::Reserve,
            2 => Self::Receipt,
            3 => Self::Position,
            4 => Self::Intent,
            _ => return None,
        })
    }
}

pub type ClientError = String;

fn err(e: impl core::fmt::Debug) -> ClientError {
    format!("{e:?}")
}

/// The blinding of output `slot` of the transaction whose first nullifier is
/// `first_nullifier` and whose private blinding seed is `blinding_seed`.
pub fn output_blinding(
    first_nullifier: &[u8; 32],
    blinding_seed: &[u8; 32],
    slot: u32,
) -> Result<[u8; 32], ClientError> {
    let seed = derive_output_blinding_seed(first_nullifier, blinding_seed).map_err(err)?;
    derive_transact_output_blinding(first_nullifier, &seed, slot).map_err(err)
}

fn state_hash<T: BorshDeserialize + DataHasher>(mut bytes: &[u8]) -> Result<[u8; 32], ClientError> {
    let state = T::deserialize(&mut bytes).map_err(err)?;
    if !bytes.is_empty() {
        return Err("trailing bytes after the note state".into());
    }
    state.hash().map_err(err)
}

/// The data hash of a Borsh-encoded note state. The reserve's commits its
/// address too: `reserve_address` is required for it and refused otherwise.
pub fn note_data_hash(
    kind: NoteKind,
    state: &[u8],
    reserve_address: Option<&[u8; 32]>,
) -> Result<[u8; 32], ClientError> {
    let h = match kind {
        NoteKind::Reserve => state_hash::<ReserveState>(state)?,
        NoteKind::Receipt => state_hash::<ReceiptState>(state)?,
        NoteKind::Position => state_hash::<PositionState>(state)?,
        NoteKind::Intent => state_hash::<IntentState>(state)?,
    };
    match (kind, reserve_address) {
        (NoteKind::Reserve, Some(address)) => unique_data_hash(address, &h).map_err(err),
        (NoteKind::Reserve, None) => Err("the reserve's hash needs its address".into()),
        (_, None) => data_hash(&h).map_err(err),
        (_, Some(_)) => Err("only the reserve has an address".into()),
    }
}

/// The hash SPP appends for an output: owner, mint, amount, data hash (none
/// for a plain token note) and blinding, in tree `tree_id`.
pub fn output_hash(
    owner: ShieldedAddress,
    mint: &[u8; 32],
    amount: u64,
    data_hash: Option<[u8; 32]>,
    blinding: [u8; 32],
    tree_id: u16,
) -> Result<[u8; 32], ClientError> {
    SppProofOutputUtxo {
        // The asset id is a payload field only; the hash commits the mint.
        asset: Mint::new(Address::new_from_array(*mint), 0),
        amount,
        blinding,
        owner_address: Some(owner),
        data_hash,
        ..Default::default()
    }
    .hash(tree_id)
    .map_err(err)
}
