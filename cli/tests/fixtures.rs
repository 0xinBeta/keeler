//! Golden make and cancel instructions for the web client's encoder, built
//! by the swap SDK's own instruction builders from fixed inputs. The proofs
//! and the transact data are fixed byte patterns, not valid proofs: the
//! fixture pins the encoding and the account list only.
//!
//! Checks `web/tests/fixtures/{make,cancel}-ix.json`; regenerate with
//! `KEELER_WRITE_FIXTURES=1 cargo test -p keeler --test fixtures`.
use std::path::PathBuf;

use serde_json::{json, Value};
use solana_address::Address;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_signer::Signer;
use spr_interface::instruction::instruction_data::transact::{
    CircuitId, InputUtxo, MessageData, OwnerTag, TransactIxData, TransactOutput, TransactProof,
    TreeContext,
};
use swap_sdk::{
    instructions::{cancel::Cancel, make::Make},
    order_authority_bump, order_authority_pda, CancelProof, MakeProof,
};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Two inputs (two nullifier PDAs), one output with a ciphertext and one
/// without, one message.
fn transact() -> TransactIxData {
    TransactIxData {
        expiry_unix_ts: 1_763_900_030,
        tx_viewing_pk: [6; 33],
        salt: [7; 16],
        interface_transfers: vec![],
        data_hash: None,
        ring_data_hash: None,
        outputs: vec![
            TransactOutput {
                utxo_hash: [8; 32],
                owner_tag: OwnerTag::Inline([9; 32]),
                data: Some(vec![10, 11, 12]),
            },
            TransactOutput {
                utxo_hash: [13; 32],
                owner_tag: OwnerTag::Account(1),
                data: None,
            },
        ],
        messages: vec![MessageData {
            view_tag: [14; 32],
            data: vec![15; 64],
        }],
        private_tx_hash: [16; 32],
        circuit: CircuitId::ConfidentialSigner(2, 2, 0),
        proof: TransactProof {
            a: [17; 32],
            b: [18; 128],
            c: [19; 32],
        },
        inputs: vec![
            InputUtxo {
                nullifier_hash: [20; 32],
                tree_index: 0,
            },
            InputUtxo {
                nullifier_hash: [21; 32],
                tree_index: 0,
            },
        ],
        tree_contexts: vec![TreeContext {
            utxo_tree_root_index: 3,
            nullifier_tree_root_index: 4,
        }],
    }
}

fn transact_json(t: &TransactIxData) -> Value {
    json!({
        "expiry_unix_ts": t.expiry_unix_ts,
        "tx_viewing_pk": hex(&t.tx_viewing_pk),
        "salt": hex(&t.salt),
        "interface_transfers": [],
        "data_hash": null,
        "ring_data_hash": null,
        "outputs": [
            {"utxo_hash": hex(&[8; 32]), "owner_tag": {"Inline": hex(&[9; 32])}, "data": hex(&[10, 11, 12])},
            {"utxo_hash": hex(&[13; 32]), "owner_tag": {"Account": 1}, "data": null},
        ],
        "messages": [{"view_tag": hex(&[14; 32]), "data": hex(&[15; 64])}],
        "private_tx_hash": hex(&t.private_tx_hash),
        "circuit": {"ConfidentialSigner": [2, 2, 0]},
        "proof": {"a": hex(&t.proof.a), "b": hex(&t.proof.b), "c": hex(&t.proof.c)},
        "inputs": [
            {"nullifier_hash": hex(&[20; 32]), "tree_index": 0},
            {"nullifier_hash": hex(&[21; 32]), "tree_index": 0},
        ],
        "tree_contexts": [{"utxo_tree_root_index": 3, "nullifier_tree_root_index": 4}],
        "wincode_hex": hex(&t.serialize().unwrap()),
    })
}

fn proof_json(a: &[u8], b: &[u8], c: &[u8]) -> Value {
    json!({"a": hex(a), "b": hex(b), "c": hex(c)})
}

fn instruction_json(ix: &Instruction) -> Value {
    json!({
        "program_id": ix.program_id.to_string(),
        "data_hex": hex(&ix.data),
        "accounts": ix.accounts.iter().map(|a| json!({
            "pubkey": a.pubkey.to_string(),
            "is_signer": a.is_signer,
            "is_writable": a.is_writable,
        })).collect::<Vec<_>>(),
    })
}

/// The maker's fee payer: `Keypair::new_from_array([1; 32])`.
fn maker() -> Address {
    Keypair::new_from_array([1; 32]).pubkey()
}

fn check(name: &str, got: Value) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../web/tests/fixtures")
        .join(name);
    let text = serde_json::to_string_pretty(&got).unwrap() + "\n";
    if std::env::var_os("KEELER_WRITE_FIXTURES").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}; regenerate the fixtures", path.display()));
    assert_eq!(text, want, "{name} changed; regenerate if that is intended");
}

const ABOUT: &str = "Built by cli/tests/fixtures.rs with the swap SDK's instruction builder. \
Proofs and transact data are fixed byte patterns, not valid proofs. data_hex is the tag byte, \
then wincode of the instruction struct; the transact part equals inputs.transact.wincode_hex. \
Regenerate: KEELER_WRITE_FIXTURES=1 cargo test -p keeler --test fixtures";

#[test]
fn make_instruction() {
    let (payer, tree) = (maker(), spr_interface::pda::tree(0));
    let proof = MakeProof {
        a: [3; 32],
        b: [4; 64],
        c: [5; 32],
    };
    let ix = Make {
        payer,
        viewing_pubkey: [2; 33],
        tree,
        make_proof: proof,
        spp_proof: transact(),
    }
    .instruction()
    .unwrap();
    check(
        "make-ix.json",
        json!({
            "about": ABOUT,
            "inputs": {
                "payer": payer.to_string(),
                "payer_keypair_seed": hex(&[1; 32]),
                "tree": tree.to_string(),
                "viewing_pubkey": hex(&[2; 33]),
                "make_proof": proof_json(&proof.a, &proof.b, &proof.c),
                "transact": transact_json(&transact()),
            },
            "derived": {
                "authority_bump": order_authority_bump(&payer),
                "order_authority": order_authority_pda(&payer).to_string(),
            },
            "instruction": instruction_json(&ix),
        }),
    );
}

#[test]
fn cancel_instruction() {
    let (maker, tree) = (maker(), spr_interface::pda::tree(0));
    let proof = CancelProof {
        a: [3; 32],
        b: [4; 64],
        c: [5; 32],
    };
    let ix = Cancel {
        maker,
        payer: maker,
        tree,
        cancel_proof: proof,
        order_expiry: 1_763_900_030,
        spp_proof: transact(),
    }
    .instruction()
    .unwrap();
    check(
        "cancel-ix.json",
        json!({
            "about": ABOUT,
            "inputs": {
                "maker": maker.to_string(),
                "payer": maker.to_string(),
                "maker_keypair_seed": hex(&[1; 32]),
                "tree": tree.to_string(),
                "order_expiry": 1_763_900_030u64,
                "cancel_proof": proof_json(&proof.a, &proof.b, &proof.c),
                "transact": transact_json(&transact()),
            },
            "derived": {
                "authority_bump": order_authority_bump(&maker),
                "order_authority": order_authority_pda(&maker).to_string(),
            },
            "instruction": instruction_json(&ix),
        }),
    );
}
