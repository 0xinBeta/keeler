//! The builders, natively (no network, no proofs): the refusals the circuits
//! make, and every output's blinding and hash recomputed from its contents
//! and the first nullifier.
mod refusal;
#[path = "../../../.tools/privacy-ring-examples/test-utils/utxo.rs"]
mod utxo;

use keeler_lend::circuits::{CancelIntent, InitReserve, Intent};
use keeler_lend_sdk::proof::CircuitId;
use refusal::{field, identity_of, refused, set, Compiled};

use keeler_lend_sdk::{
    builders::{self, Prepared},
    decode, note_data_hash, output_blinding, output_hash, pool_pda, purpose, user_pda, IntentState,
    NoteKind, ReserveState, ADMIN,
};
use solana_address::Address;
use solana_keypair::Keypair;
use spr_keypair::ShieldedKeypair;
use spr_program::DataUtxo;
use spr_transaction::{Data, Mint, Utxo, WalletUtxo};

const TREE_ID: u16 = 0;
const MINT: Mint = Mint::new(Address::new_from_array([3; 32]), 2);
const SOURCE: u64 = 1_000;

fn keypair(seed: u8) -> ShieldedKeypair {
    ShieldedKeypair::from_keypair(&Keypair::new_from_array([seed; 32])).unwrap()
}

fn note(owner: &ShieldedKeypair, mint: Mint, amount: u64) -> WalletUtxo {
    utxo::wallet_utxo(
        Utxo {
            owner: owner.signing_pubkey(),
            asset: mint,
            amount,
            blinding: [5; 32],
            ring_program_id: None,
            data: Data::default(),
        },
        &owner.nullifier_key,
        TREE_ID,
        0,
    )
}

fn intent_with(
    user: &ShieldedKeypair,
    mint: &Address,
    amount: u64,
    purpose: u8,
) -> anyhow::Result<Prepared> {
    let keeper = keypair(1);
    builders::intent(
        user,
        note(user, MINT, SOURCE),
        mint,
        amount,
        purpose,
        7,
        2_000_000_000,
        keeper.viewing_pubkey(),
        TREE_ID,
    )
}

/// Every output's blinding and hash recompute; returns the data outputs.
fn check_outputs(p: &Prepared) -> Vec<usize> {
    let mut data = Vec::new();
    for (slot, out) in p.outputs.iter().enumerate() {
        let Some(owner) = out.owner_address else {
            continue;
        };
        let blinding =
            output_blinding(&p.first_nullifier, &p.spp.blinding_seed, slot as u32).unwrap();
        assert_eq!(blinding, out.blinding, "slot {slot} blinding");
        let hash = output_hash(
            owner,
            out.asset.asset.as_array(),
            out.amount,
            out.data_hash,
            blinding,
            TREE_ID,
        )
        .unwrap();
        assert_eq!(hash, out.hash(TREE_ID).unwrap(), "slot {slot} hash");
        if out.data_hash.is_some() {
            data.push(slot);
        }
    }
    data
}

#[test]
fn intent_outputs_recompute_and_decode() {
    let user = keypair(7);
    let p = intent_with(&user, &MINT.asset, 400, purpose::SUPPLY).unwrap();
    let data = check_outputs(&p);
    assert_eq!(data, vec![keeler_lend::circuits::INTENT_SLOT]);
    let out = &p.outputs[data[0]];
    assert_eq!(out.amount, 400);
    assert_eq!(
        p.outputs[keeler_lend::circuits::INTENT_CHANGE_SLOT].amount,
        SOURCE - 400
    );
    let owner = user_pda(&user.shielded_address().unwrap().solana_address().unwrap()).owner;
    let intent = owner.input(out, TREE_ID, 1).unwrap();
    let state: IntentState = decode(&intent).unwrap();
    assert_eq!(
        (state.purpose, state.target, state.expiry),
        (purpose::SUPPLY, 7, 2_000_000_000)
    );
    let bytes = borsh::to_vec(&state).unwrap();
    assert_eq!(
        Some(note_data_hash(NoteKind::Intent, &bytes, None).unwrap()),
        out.data_hash
    );
    // The wrong kind, or an address on a plain note, does not match or is refused.
    assert_eq!(
        note_data_hash(NoteKind::Intent, &bytes, Some(&[0; 32])).unwrap_err(),
        "only the reserve has an address"
    );
}

#[test]
fn intent_refuses_wrong_asset_purpose_and_amount() {
    let user = keypair(7);
    refused(
        intent_with(
            &user,
            &Address::new_from_array([4; 32]),
            400,
            purpose::SUPPLY,
        ),
        "circuits/notes.rs",
        "require_eq(hi, mint_hi);",
    );
    for bad in [0, 4, u8::MAX] {
        refused(
            intent_with(&user, &MINT.asset, 400, bad),
            "circuits/intent.rs",
            "require(purpose::is_valid(p.purpose));",
        );
    }
    refused(
        intent_with(&user, &MINT.asset, 0, purpose::SUPPLY),
        "circuits/intent.rs",
        "require(p.amount != 0);",
    );
    refused(
        intent_with(&user, &MINT.asset, SOURCE + 1, purpose::SUPPLY),
        "utxo/balance.rs",
        "crate::require_eq(high, u64::from(borrow));",
    );
    // A note someone else owns.
    let other = keypair(9);
    let keeper = keypair(1);
    let stolen = builders::intent(
        &user,
        note(&other, MINT, SOURCE),
        &MINT.asset,
        400,
        purpose::SUPPLY,
        7,
        2_000_000_000,
        keeper.viewing_pubkey(),
        TREE_ID,
    );
    refused(
        stolen,
        "circuits/intent.rs",
        "require_eq(maker.key().identity()?, public.maker_identity);",
    );
}

#[test]
fn only_the_maker_cancels() {
    let user = keypair(7);
    let keeper = keypair(1);
    let p = intent_with(&user, &MINT.asset, 400, purpose::DEPOSIT).unwrap();
    let out = &p.outputs[keeler_lend::circuits::INTENT_SLOT];
    let owner = user_pda(&user.shielded_address().unwrap().solana_address().unwrap()).owner;
    let intent = owner.input(out, TREE_ID, 1).unwrap();

    let cancel =
        builders::cancel_intent(&user, intent.clone(), keeper.viewing_pubkey(), TREE_ID).unwrap();
    check_outputs(&cancel);
    let refund = &cancel.outputs[keeler_lend::circuits::CANCEL_REFUND_SLOT];
    assert_eq!(
        (refund.amount, refund.owner_address),
        (400, Some(user.shielded_address().unwrap()))
    );

    let other = keypair(9);
    refused(
        builders::cancel_intent(&other, intent.clone(), keeper.viewing_pubkey(), TREE_ID),
        "circuits/cancel_intent.rs",
        "require_eq(intent.owner(), *public.user_owner.owner());",
    );
    // The owner check is on the PDA, not the viewing key, which only says
    // whom new notes are encrypted to: any key cancels for the maker.
    assert!(builders::cancel_intent(&user, intent, other.viewing_pubkey(), TREE_ID).is_ok());
}

// The compiled circuits: valid inputs with one witness changed.

const IN_EXECUTE: &str = "execute → require";
const IN_EXECUTE_EQ: &str = "execute → require_eq";
/// `{amount,purpose,target,expiry}`: u64, u8, u64, u64 from bit 0.
const INTENT_PACKED: &str = "program.private.{amount,purpose,target,expiry}";

#[test]
fn compiled_intent_refusals() {
    let user = keypair(7);
    let p = intent_with(&user, &MINT.asset, 400, purpose::SUPPLY).unwrap();
    let c = Compiled {
        id: CircuitId::Intent,
        inputs: &p.proof_inputs,
        execute: Intent::execute,
    };
    // Another maker's identity: only the identity check.
    c.refuses(
        set("program.public.maker_identity", identity_of(&keypair(9))),
        "circuits/intent.rs",
        "require_eq(maker.key().identity()?, public.maker_identity);",
        IN_EXECUTE_EQ,
    );
    c.refuses(
        field(&p.proof_inputs, INTENT_PACKED, 64, 8, 4),
        "circuits/intent.rs",
        "require(purpose::is_valid(p.purpose));",
        IN_EXECUTE,
    );
    c.refuses(
        field(&p.proof_inputs, INTENT_PACKED, 0, 64, 0),
        "circuits/intent.rs",
        "require(p.amount != 0);",
        "→ check program.private.{amount,purpose,target,expiry}",
    );
}

#[test]
fn compiled_cancel_intent_refusals() {
    let user = keypair(7);
    let keeper = keypair(1);
    let p = intent_with(&user, &MINT.asset, 400, purpose::DEPOSIT).unwrap();
    let owner = user_pda(&user.shielded_address().unwrap().solana_address().unwrap()).owner;
    let intent = owner
        .input(&p.outputs[keeler_lend::circuits::INTENT_SLOT], TREE_ID, 1)
        .unwrap();
    let cancel = builders::cancel_intent(&user, intent, keeper.viewing_pubkey(), TREE_ID).unwrap();
    let c = Compiled {
        id: CircuitId::CancelIntent,
        inputs: &cancel.proof_inputs,
        execute: CancelIntent::execute,
    };
    c.refuses(
        set("program.public.maker_identity", identity_of(&keypair(9))),
        "circuits/cancel_intent.rs",
        "require_eq(maker.key().identity()?, public.maker_identity);",
        IN_EXECUTE_EQ,
    );
    // The program checks the clock against the public expiry, so it must be
    // the intent's.
    c.refuses(
        set("program.public.expiry", 1_000_000_000u64),
        "circuits/cancel_intent.rs",
        "require_eq(intent.expiry, public.expiry);",
        IN_EXECUTE_EQ,
    );
}

#[test]
fn compiled_init_reserve_refusals() {
    let p = builders::init_reserve(&keypair(1), ADMIN, MINT, 1_800_000_000, 42, TREE_ID).unwrap();
    let c = Compiled {
        id: CircuitId::InitReserve,
        inputs: &p.proof_inputs,
        execute: InitReserve::execute,
    };
    // A reserve of one mint published as another's.
    c.refuses(
        set("program.public.mint_hi", 5u64),
        "circuits/notes.rs",
        "require_eq(hi, mint_hi);",
        "assert_mint → require_eq",
    );
}

#[test]
fn init_reserve_output_recomputes_with_its_address() {
    let keeper = keypair(1);
    let p = builders::init_reserve(&keeper, ADMIN, MINT, 1_800_000_000, 42, TREE_ID).unwrap();
    let data = check_outputs(&p);
    assert_eq!(data.len(), 1);
    let out = &p.outputs[data[0]];
    let reserve = pool_pda(&MINT.asset).owner.input(out, TREE_ID, 1).unwrap();
    let decoded = DataUtxo::<ReserveState>::try_from(reserve).unwrap();
    let state = &decoded.data;
    assert_eq!(
        (
            state.supply_price,
            state.borrow_price,
            state.last_update,
            state.salt
        ),
        (1_000_000_000_000, 1_000_000_000_000, 1_800_000_000, 42)
    );
    assert_eq!((state.supply_shares, state.debt_shares), (0, 0));
    let address = decoded.address.expect("the reserve has an address");
    let bytes = borsh::to_vec(state).unwrap();
    assert_eq!(
        Some(note_data_hash(NoteKind::Reserve, &bytes, Some(&address)).unwrap()),
        out.data_hash
    );
    assert_eq!(
        note_data_hash(NoteKind::Reserve, &bytes, None).unwrap_err(),
        "the reserve's hash needs its address"
    );
    // The address depends only on the mint: a second init proves the same one.
    let again = builders::init_reserve(&keeper, ADMIN, MINT, 1_800_000_100, 43, TREE_ID).unwrap();
    let out2 = &again.outputs[data[0]];
    let decoded2 = DataUtxo::<ReserveState>::try_from(
        pool_pda(&MINT.asset).owner.input(out2, TREE_ID, 2).unwrap(),
    )
    .unwrap();
    assert_eq!(decoded2.address, Some(address));
}
