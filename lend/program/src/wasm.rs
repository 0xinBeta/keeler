//! Browser exports beside the generated `<circuit>Transaction` and
//! `<state>DataUtxo` ones: recompute an output's blinding and hash to check a
//! note the keeper built.
use spr_keypair::ShieldedAddress;
use spr_program::__private::wasm_bindgen::{self, prelude::*};

use crate::client::{self, NoteKind};

fn array<const N: usize>(bytes: &[u8], what: &str) -> Result<[u8; N], JsError> {
    bytes
        .try_into()
        .map_err(|_| JsError::new(&format!("{what}: expected {N} bytes, got {}", bytes.len())))
}

fn js(e: client::ClientError) -> JsError {
    JsError::new(&e)
}

/// The blinding of output `slot`, from the transaction's first nullifier and
/// its private blinding seed.
#[wasm_bindgen(js_name = outputBlinding, wasm_bindgen = wasm_bindgen)]
pub fn output_blinding(
    first_nullifier: &[u8],
    blinding_seed: &[u8],
    slot: u32,
) -> Result<Vec<u8>, JsError> {
    let nullifier = array::<32>(first_nullifier, "first nullifier")?;
    let seed = array::<32>(blinding_seed, "blinding seed")?;
    Ok(client::output_blinding(&nullifier, &seed, slot)
        .map_err(js)?
        .to_vec())
}

/// The output hash of a note: `kind` 0 is a plain token note (no state),
/// 1-4 are [`NoteKind`] with its Borsh-encoded `state`; the reserve also
/// takes its 32-byte address.
#[allow(clippy::too_many_arguments)]
#[wasm_bindgen(js_name = noteOutputHash, wasm_bindgen = wasm_bindgen)]
pub fn note_output_hash(
    owner: &[u8],
    mint: &[u8],
    amount: u64,
    kind: u8,
    state: &[u8],
    reserve_address: Option<Vec<u8>>,
    blinding: &[u8],
    tree_id: u16,
) -> Result<Vec<u8>, JsError> {
    let owner = ShieldedAddress::from_bytes(&array(owner, "owner")?)
        .map_err(|e| JsError::new(&format!("owner: {e:?}")))?;
    let mint = array::<32>(mint, "mint")?;
    let data_hash = if kind == 0 {
        None
    } else {
        let kind = NoteKind::from_u8(kind).ok_or_else(|| JsError::new("unknown note kind"))?;
        let address = reserve_address
            .map(|a| array::<32>(&a, "reserve address"))
            .transpose()?;
        Some(client::note_data_hash(kind, state, address.as_ref()).map_err(js)?)
    };
    let blinding = array::<32>(blinding, "blinding")?;
    Ok(
        client::output_hash(owner, &mint, amount, data_hash, blinding, tree_id)
            .map_err(js)?
            .to_vec(),
    )
}
