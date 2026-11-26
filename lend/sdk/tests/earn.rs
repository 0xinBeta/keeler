//! `supply` and `redeem`, natively (no network; one proof): the circuit's
//! refusals, the shared rounding vectors, who can read each output, and every
//! output recomputed by its owner from the user note and the first nullifier.
//! Pool-owned notes with chosen states are forged here: native execution does
//! not check the tree, only the note's contents.
mod refusal;
mod utxo;

use keeler_lend::circuits::{Redeem, Supply};
use keeler_lend_sdk::{proof::CircuitId, IntentState};
use refusal::{identity_of, refused, reserve_refusals, state_leaves, under, Compiled};

use keeler_lend::circuits::{
    REDEEM_BLINDING_DOMAIN, REDEEM_PAYOUT_SLOT, REDEEM_RECEIPT_SLOT, REDEEM_RESERVE_SLOT,
    SUPPLY_BLINDING_DOMAIN, SUPPLY_RECEIPT_SLOT, SUPPLY_RESERVE_SLOT, TUSD_MIN_SUPPLY,
};
use keeler_lend_sdk::{
    builders::{self, earn_blinding_seed, recover_output, Prepared},
    decode, note_data_hash, output_blinding, output_hash, pool_pda, purpose, user_pda, NoteKind,
    ReceiptState, ReserveState, ADMIN,
};
use serde_json::Value;
use solana_address::Address;
use solana_keypair::Keypair;
use spr_keypair::{ShieldedAddress, ShieldedKeypair, X25519Pubkey};
use spr_program::{DataUtxo, ProgramOwner};
use spr_transaction::{
    serialization::confidential::ConfidentialOutputPlaintext, Data, DataRecord, Mint,
    SppProofOutputUtxo, Utxo, WalletUtxo,
};

const TREE_ID: u16 = 0;
const MINT: Mint = Mint::new(Address::new_from_array([3; 32]), 2);
const OTHER_MINT: Mint = Mint::new(Address::new_from_array([4; 32]), 5);
const NOW: u64 = 1_800_000_000;
const EXPIRY: u64 = NOW + 600;
const TUSD: u64 = keeler_lend::pool::TUSD;
const PRICE: u64 = 1_000_000_000_000;
const VECTORS: &str = include_str!("../../../web/tests/fixtures/lend-vectors.json");

fn keypair(seed: u8) -> ShieldedKeypair {
    ShieldedKeypair::from_keypair(&Keypair::new_from_array([seed; 32])).unwrap()
}

fn keeper() -> ShieldedKeypair {
    keypair(1)
}

fn address(k: &ShieldedKeypair) -> ShieldedAddress {
    k.shielded_address().unwrap()
}

fn wallet(k: &ShieldedKeypair) -> Address {
    address(k).solana_address().unwrap()
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

/// `out` as a note of `owner`; the leaf index only salts nothing natively.
fn input(owner: &ProgramOwner, out: &SppProofOutputUtxo) -> WalletUtxo {
    owner.input(out, TREE_ID, 1).unwrap()
}

/// A fresh reserve of `mint` from `init_reserve`, last updated at `NOW`.
fn fresh_reserve(mint: Mint) -> (SppProofOutputUtxo, WalletUtxo) {
    let p = builders::init_reserve(&keeper(), ADMIN, mint, NOW, 7, TREE_ID).unwrap();
    let out = p
        .outputs
        .iter()
        .find(|o| o.data_hash.is_some())
        .unwrap()
        .clone();
    let utxo = input(&pool_pda(&mint.asset).owner, &out);
    (out, utxo)
}

fn reserve_address(mint: Mint) -> [u8; 32] {
    let (_, utxo) = fresh_reserve(mint);
    DataUtxo::<ReserveState>::try_from(utxo)
        .unwrap()
        .address
        .unwrap()
}

/// A note shaped like `template` (owner address, asset, blinding) holding
/// `amount` and the data `bytes` under `hash`, spendable by `owner`.
fn forge(
    owner: &ProgramOwner,
    template: &SppProofOutputUtxo,
    amount: u64,
    bytes: Vec<u8>,
    hash: [u8; 32],
) -> WalletUtxo {
    let mut out = template.clone();
    out.amount = amount;
    out.data = Data::new(vec![DataRecord::UtxoData(bytes)]);
    out.data_hash = Some(hash);
    input(owner, &out)
}

/// A pool-owned reserve of `MINT` at its real address with `state` and
/// `idle` liquidity.
fn reserve_with(state: &ReserveState, idle: u64) -> WalletUtxo {
    let (template, _) = fresh_reserve(MINT);
    let address = reserve_address(MINT);
    let bytes = borsh::to_vec(state).unwrap();
    let hash = note_data_hash(NoteKind::Reserve, &bytes, Some(&address)).unwrap();
    forge(
        &pool_pda(&MINT.asset).owner,
        &template,
        idle,
        [address.to_vec(), bytes].concat(),
        hash,
    )
}

fn intent_of(
    user: &ShieldedKeypair,
    mint: Mint,
    amount: u64,
    purpose: u8,
    target: u64,
) -> WalletUtxo {
    let p = builders::intent(
        user,
        note(user, mint, 1_000_000_000),
        &mint.asset,
        amount,
        purpose,
        target,
        EXPIRY,
        keeper().viewing_pubkey(),
        TREE_ID,
    )
    .unwrap();
    input(
        &user_pda(&wallet(user)).owner,
        &p.outputs[keeler_lend::circuits::INTENT_SLOT],
    )
}

fn supply_intent(user: &ShieldedKeypair, amount: u64) -> WalletUtxo {
    intent_of(user, MINT, amount, purpose::SUPPLY, TUSD)
}

fn supply(
    owner: &ShieldedAddress,
    reserve: WalletUtxo,
    intent: WalletUtxo,
    now: u64,
) -> anyhow::Result<Prepared> {
    builders::supply(&keeper(), owner, reserve, intent, now, 99, TREE_ID)
}

fn redeem(
    owner: &ShieldedAddress,
    reserve: WalletUtxo,
    receipt: WalletUtxo,
    amount: u64,
    now: u64,
) -> anyhow::Result<Prepared> {
    builders::redeem(&keeper(), owner, reserve, receipt, amount, now, 98, TREE_ID)
}

/// Every output's blinding and hash recompute from the published first
/// nullifier and the blinding seed derived from `note` under `domain`.
fn check_outputs(p: &Prepared, domain: u32, note: &WalletUtxo) {
    assert_eq!(p.first_nullifier, note.nullifier, "the user note is first");
    let seed = earn_blinding_seed(domain, note).unwrap();
    assert_eq!(seed, p.spp.blinding_seed, "the seed is the note's");
    for (slot, out) in p.outputs.iter().enumerate() {
        // Compact padding up to the SPP shape has no owner.
        let Some(owner) = out.owner_address else {
            continue;
        };
        let blinding = output_blinding(&p.first_nullifier, &seed, slot as u32).unwrap();
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
    }
}

fn reserve_out(p: &Prepared, slot: usize) -> ReserveState {
    decode(&input(&pool_pda(&MINT.asset).owner, &p.outputs[slot])).unwrap()
}

fn receipt_out(p: &Prepared, user: &ShieldedKeypair, slot: usize) -> (WalletUtxo, ReceiptState) {
    let utxo = input(&user_pda(&wallet(user)).owner, &p.outputs[slot]);
    let state = decode(&utxo).unwrap();
    (utxo, state)
}

/// Supplies `amount` from `user` into a fresh reserve: the prepared supply,
/// the new reserve and the receipt, as spendable notes.
fn supplied(user: &ShieldedKeypair, amount: u64) -> (Prepared, WalletUtxo, WalletUtxo) {
    let (_, reserve) = fresh_reserve(MINT);
    let p = supply(&address(user), reserve, supply_intent(user, amount), NOW).unwrap();
    let reserve = input(
        &pool_pda(&MINT.asset).owner,
        &p.outputs[SUPPLY_RESERVE_SLOT],
    );
    let receipt = input(
        &user_pda(&wallet(user)).owner,
        &p.outputs[SUPPLY_RECEIPT_SLOT],
    );
    (p, reserve, receipt)
}

/// The outputs that are notes, not padding.
fn owned(p: &Prepared) -> usize {
    p.outputs
        .iter()
        .filter(|o| o.owner_address.is_some())
        .count()
}

fn viewing(out: &SppProofOutputUtxo) -> X25519Pubkey {
    out.owner_address.unwrap().viewing_pubkey
}

#[test]
fn supply_proves_with_the_checked_keys() {
    let user = keypair(7);
    let (_, reserve) = fresh_reserve(MINT);
    let p = supply(
        &address(&user),
        reserve,
        supply_intent(&user, 2_500_000),
        NOW + 30,
    )
    .unwrap();
    keeler_lend_sdk::proof::prove(p.circuit(), &p.proof_inputs).unwrap();
}

#[test]
fn supply_mints_the_maker_a_receipt_and_fills_the_reserve() {
    let user = keypair(7);
    let amount = 2_500_000;
    let (_, reserve) = fresh_reserve(MINT);
    let intent = supply_intent(&user, amount);
    let p = supply(&address(&user), reserve, intent.clone(), NOW + 30).unwrap();
    check_outputs(&p, SUPPLY_BLINDING_DOMAIN, &intent);
    assert_eq!(owned(&p), 2, "reserve and receipt");

    let r = reserve_out(&p, SUPPLY_RESERVE_SLOT);
    assert_eq!(
        p.outputs[SUPPLY_RESERVE_SLOT].amount, amount,
        "all of the intent"
    );
    assert_eq!((r.supply_shares, r.supply_price), (amount, PRICE));
    assert_eq!((r.last_update, r.salt, r.rate_bps), (NOW + 30, 99, 0));

    let (_, receipt) = receipt_out(&p, &user, SUPPLY_RECEIPT_SLOT);
    assert_eq!(p.outputs[SUPPLY_RECEIPT_SLOT].amount, 0);
    assert_eq!((receipt.pool, receipt.supply_shares), (TUSD, amount));
    let maker: keeler_lend_sdk::IntentState = decode(&intent).unwrap();
    assert!(receipt.owner == maker.maker, "the receipt is the maker's");
}

#[test]
fn outputs_are_encrypted_to_their_owners_and_the_keeper_recovers_them() {
    let keeper = keeper();
    let user = keypair(7);
    let (supplied, reserve, receipt) = supplied(&user, 3_000_000);
    let p = redeem(&address(&user), reserve, receipt, 1_000_000, NOW).unwrap();
    for (p, receipt_slot, reserve_slot) in [
        (&supplied, SUPPLY_RECEIPT_SLOT, SUPPLY_RESERVE_SLOT),
        (&p, REDEEM_RECEIPT_SLOT, REDEEM_RESERVE_SLOT),
    ] {
        assert_eq!(viewing(&p.outputs[receipt_slot]), user.viewing_pubkey());
        assert_eq!(viewing(&p.outputs[reserve_slot]), keeper.viewing_pubkey());
        let ext = &p.spp.external_data;
        let writer = keeper
            .get_transaction_viewing_key(&p.first_nullifier)
            .unwrap();
        let tx_pk = writer.pubkey();
        assert_eq!(tx_pk.as_bytes(), &ext.tx_viewing_pk);
        for (slot, out) in p.outputs.iter().enumerate() {
            if out.owner_address.is_none() {
                continue;
            }
            let payload = ext.outputs[slot].data.as_ref().unwrap();
            // The keeper, who built it, reads every slot with its writer key.
            let plain = recover_output(&keeper, &p.first_nullifier, payload, ext.salt, slot as u32)
                .unwrap();
            assert_eq!((plain.amount, plain.blinding), (out.amount, out.blinding));
            assert_eq!(plain.data, out.data, "slot {slot} data");
            // The owner reads it with their viewing key; the keeper's does not.
            let owner = if viewing(out) == user.viewing_pubkey() {
                &user
            } else {
                &keeper
            };
            let not_owner = if owner.viewing_pubkey() == user.viewing_pubkey() {
                &keeper
            } else {
                &user
            };
            // The body after the encoding (6 bytes) and the embedded key (33).
            let rest = &payload[6 + 33..];
            let read = |k: &ShieldedKeypair| {
                k.decrypt_utxo(rest, &tx_pk, ext.salt, slot as u32)
                    .ok()
                    .and_then(|b| ConfidentialOutputPlaintext::deserialize(&b).ok())
            };
            assert_eq!(read(owner).as_ref(), Some(&plain), "slot {slot} owner");
            assert_ne!(read(not_owner).as_ref(), Some(&plain), "slot {slot} other");
        }
    }
    assert_eq!(
        viewing(&p.outputs[REDEEM_PAYOUT_SLOT]),
        user.viewing_pubkey()
    );
    assert_eq!(
        p.outputs[REDEEM_PAYOUT_SLOT].owner_address,
        Some(address(&user))
    );
}

#[test]
fn supply_refuses_another_owner_pool_purpose_mint_and_expiry() {
    let user = keypair(7);
    let other = keypair(9);
    let (_, reserve) = fresh_reserve(MINT);
    let ok = supply_intent(&user, 2_000_000);
    assert!(supply(&address(&user), reserve.clone(), ok.clone(), NOW).is_ok());
    // A receipt minted to someone other than the maker.
    refused(
        supply(&address(&other), reserve.clone(), ok.clone(), NOW),
        "circuits/supply.rs",
        "require_eq(intent.owner(), *public.user_owner.owner());",
    );
    // Another pool, or a deposit / repay intent.
    for (purpose, target, needle) in [
        (
            purpose::SUPPLY,
            TUSD + 1,
            "require_eq(intent.target, target);",
        ),
        (
            purpose::DEPOSIT,
            TUSD,
            "require_eq(intent.purpose, purpose);",
        ),
        (purpose::REPAY, TUSD, "require_eq(intent.purpose, purpose);"),
    ] {
        let intent = intent_of(&user, MINT, 2_000_000, purpose, target);
        refused(
            supply(&address(&user), reserve.clone(), intent, NOW),
            "circuits/notes.rs",
            needle,
        );
    }
    // An intent of another mint into this pool.
    let wrong = intent_of(&user, OTHER_MINT, 2_000_000, purpose::SUPPLY, TUSD);
    refused(
        supply(&address(&user), reserve.clone(), wrong, NOW),
        "circuits/notes.rs",
        "require_eq(hi, mint_hi);",
    );
    // Filled after its expiry, or with `now` before the reserve's last update.
    assert!(supply(&address(&user), reserve.clone(), ok.clone(), EXPIRY).is_ok());
    refused(
        supply(&address(&user), reserve.clone(), ok.clone(), EXPIRY + 1),
        "circuits/notes.rs",
        "require(now <= intent.expiry);",
    );
    refused(
        supply(&address(&user), reserve.clone(), ok, NOW - 1),
        "circuits/math.rs",
        "require(now >= state.last_update);",
    );
    // Below the pool's minimum.
    let small = supply_intent(&user, TUSD_MIN_SUPPLY - 1);
    refused(
        supply(&address(&user), reserve.clone(), small, NOW),
        "circuits/supply.rs",
        "require(amount >= TUSD_MIN_SUPPLY);",
    );
    let min = supply_intent(&user, TUSD_MIN_SUPPLY);
    assert!(supply(&address(&user), reserve, min, NOW).is_ok());
}

#[test]
fn supply_refuses_a_fake_reserve() {
    let user = keypair(7);
    let intent = supply_intent(&user, 2_000_000);
    let state = ReserveState {
        supply_price: PRICE,
        borrow_price: PRICE,
        last_update: NOW,
        ..Default::default()
    };
    let bytes = borsh::to_vec(&state).unwrap();
    let (template, _) = fresh_reserve(MINT);
    let pool = pool_pda(&MINT.asset).owner;
    // The real one, forged the same way, is accepted: the forgery is sound.
    assert!(supply(
        &address(&user),
        reserve_with(&state, 0),
        intent.clone(),
        NOW
    )
    .is_ok());
    // Another mint's reserve.
    let (_, other) = fresh_reserve(OTHER_MINT);
    refused(
        supply(&address(&user), other, intent.clone(), NOW),
        "circuits/notes.rs",
        "require_eq(hi, mint_hi);",
    );
    // A second "reserve" of this mint at another address.
    let elsewhere = reserve_address(OTHER_MINT);
    let fake = forge(
        &pool,
        &template,
        0,
        [elsewhere.to_vec(), bytes.clone()].concat(),
        note_data_hash(NoteKind::Reserve, &bytes, Some(&elsewhere)).unwrap(),
    );
    refused(
        supply(&address(&user), fake, intent.clone(), NOW),
        "utxo/data.rs",
        "require_eq(input.data_hash, expected);",
    );
    // A plain data note with the reserve's state, at no address.
    let plain = forge(
        &pool,
        &template,
        0,
        bytes.clone(),
        spr_program::hasher::data_hash(&spr_program::hasher::DataHasher::hash(&state).unwrap())
            .unwrap(),
    );
    refused(
        supply(&address(&user), plain, intent.clone(), NOW),
        "utxo/data.rs",
        "require_eq(input.data_hash, expected);",
    );
    // The reserve's address and state under another owner (the user's PDA).
    let address_ = reserve_address(MINT);
    let mut template_user = template.clone();
    template_user.owner_address = Some(
        user_pda(&wallet(&user))
            .owner
            .address(keeper().viewing_pubkey())
            .unwrap(),
    );
    let foreign = forge(
        &user_pda(&wallet(&user)).owner,
        &template_user,
        0,
        [address_.to_vec(), bytes.clone()].concat(),
        note_data_hash(NoteKind::Reserve, &bytes, Some(&address_)).unwrap(),
    );
    refused(
        supply(&address(&user), foreign, intent, NOW),
        "circuits/notes.rs",
        "require_eq(note.owner(), *owner);",
    );
}

#[test]
fn supply_accrues_first() {
    // The "one day at the kink" accrual vector, then a supply.
    let user = keypair(7);
    let state = ReserveState {
        supply_shares: 1_000_000_000_000,
        debt_shares: 880_000_000_000,
        supply_price: PRICE,
        borrow_price: PRICE,
        rate_bps: 500,
        last_update: 1_763_900_000,
        salt: 0,
    };
    let now = 1_763_986_400;
    let amount = 1_000_000;
    let reserve = reserve_with(&state, 120_000_000_000);
    let p = supply(&address(&user), reserve, supply_intent(&user, amount), now).unwrap();
    let r = reserve_out(&p, SUPPLY_RESERVE_SLOT);
    assert_eq!(
        (r.borrow_price, r.supply_price),
        (1_000_136_986_302, 1_000_108_493_150)
    );
    let shares = (u128::from(amount) * u128::from(PRICE) / 1_000_108_493_150) as u64;
    assert_eq!(r.supply_shares, state.supply_shares + shares);
    assert_eq!(r.last_update, now);
    let (_, receipt) = receipt_out(&p, &user, SUPPLY_RECEIPT_SLOT);
    assert_eq!(receipt.supply_shares, shares);
    assert_eq!(
        p.outputs[SUPPLY_RESERVE_SLOT].amount,
        120_000_000_000 + amount
    );
}

/// A receipt of `user` for `shares` of the tUSD pool, shaped like a real one.
fn receipt_with(user: &ShieldedKeypair, shares: u64) -> WalletUtxo {
    let (p, _, receipt) = supplied(user, 2_000_000);
    let mut state: ReceiptState = decode(&receipt).unwrap();
    state.supply_shares = shares;
    let bytes = borsh::to_vec(&state).unwrap();
    let hash = note_data_hash(NoteKind::Receipt, &bytes, None).unwrap();
    forge(
        &user_pda(&wallet(user)).owner,
        &p.outputs[SUPPLY_RECEIPT_SLOT],
        0,
        bytes,
        hash,
    )
}

fn case<'a>(all: &'a Value, f: &str, name: &str) -> &'a Value {
    all["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["fn"] == f && c["name"] == name)
        .unwrap_or_else(|| panic!("no vector {f} / {name}"))
}

fn num(v: &Value) -> u64 {
    v.as_str().unwrap().parse().unwrap()
}

fn vector_reserve(v: &Value) -> ReserveState {
    ReserveState {
        supply_shares: num(&v["supply_shares"]),
        debt_shares: num(&v["debt_shares"]),
        supply_price: num(&v["supply_price"]),
        borrow_price: num(&v["borrow_price"]),
        rate_bps: num(&v["rate_bps"]),
        last_update: num(&v["last_update"]),
        salt: 0,
    }
}

fn same(got: &ReserveState, want: &Value) {
    let want = vector_reserve(want);
    assert_eq!(
        (
            got.supply_shares,
            got.debt_shares,
            got.supply_price,
            got.borrow_price,
            got.rate_bps,
            got.last_update
        ),
        (
            want.supply_shares,
            want.debt_shares,
            want.supply_price,
            want.borrow_price,
            want.rate_bps,
            want.last_update
        )
    );
}

#[test]
fn supply_and_redeem_pass_the_shared_vectors() {
    let all: Value = serde_json::from_str(VECTORS).unwrap();
    let user = keypair(7);
    // Supply: the zero-shares case is below the pool's minimum, refused too.
    for name in ["first supply", "into a busy pool", "zero shares"] {
        let c = case(&all, "supply", name);
        let i = &c["in"];
        let state = vector_reserve(&i["reserve"]);
        let now = state.last_update;
        let got = supply(
            &address(&user),
            reserve_with(&state, 0),
            supply_intent(&user, num(&i["amount"])),
            now,
        );
        if c["out"].is_null() {
            refused(
                got,
                "circuits/supply.rs",
                "require(amount >= TUSD_MIN_SUPPLY);",
            );
            continue;
        }
        let p = got.unwrap_or_else(|e| panic!("{name}: {e}"));
        same(&reserve_out(&p, SUPPLY_RESERVE_SLOT), &c["out"]["reserve"]);
        let (_, receipt) = receipt_out(&p, &user, SUPPLY_RECEIPT_SLOT);
        assert_eq!(receipt.supply_shares, num(&c["out"]["shares"]), "{name}");
    }
    // Redeem: partial, all of a receipt, more than held, more than idle, zero.
    for name in [
        "partial",
        "all of a receipt",
        "more than held",
        "more than idle",
        "zero",
    ] {
        let c = case(&all, "redeem", name);
        let i = &c["in"];
        let state = vector_reserve(&i["reserve"]);
        let (held, amount) = (num(&i["held"]), num(&i["amount"]));
        let got = redeem(
            &address(&user),
            reserve_with(&state, num(&i["idle"])),
            receipt_with(&user, held),
            amount,
            state.last_update,
        );
        if c["out"].is_null() {
            let (file, needle) = match name {
                "more than held" => ("circuits/math.rs", "require(a >= b);"),
                "more than idle" => ("utxo/balance.rs", "require_eq(high, u64::from(borrow));"),
                _ => ("circuits/redeem.rs", "require(p.amount != 0);"),
            };
            refused(got, file, needle);
            continue;
        }
        let p = got.unwrap_or_else(|e| panic!("{name}: {e}"));
        same(&reserve_out(&p, REDEEM_RESERVE_SLOT), &c["out"]["reserve"]);
        let (_, receipt) = receipt_out(&p, &user, REDEEM_RECEIPT_SLOT);
        assert_eq!(
            receipt.supply_shares,
            held - num(&c["out"]["burned"]),
            "{name}"
        );
        assert_eq!(p.outputs[REDEEM_PAYOUT_SLOT].amount, amount);
        assert_eq!(
            p.outputs[REDEEM_RESERVE_SLOT].amount,
            num(&i["idle"]) - amount
        );
    }
}

#[test]
fn redeem_partial_then_full_keeps_three_outputs() {
    let user = keypair(7);
    let (_, reserve, receipt) = supplied(&user, 3_000_000);
    let p = redeem(
        &address(&user),
        reserve,
        receipt.clone(),
        1_000_000,
        NOW + 5,
    )
    .unwrap();
    check_outputs(&p, REDEEM_BLINDING_DOMAIN, &receipt);
    assert_eq!(owned(&p), 3);
    let (left, state) = receipt_out(&p, &user, REDEEM_RECEIPT_SLOT);
    assert_eq!(state.supply_shares, 2_000_000);
    let payout = &p.outputs[REDEEM_PAYOUT_SLOT];
    assert_eq!((payout.amount, payout.data_hash), (1_000_000, None));
    let r = reserve_out(&p, REDEEM_RESERVE_SLOT);
    assert_eq!(
        (r.supply_shares, r.last_update, r.salt),
        (2_000_000, NOW + 5, 98)
    );
    assert_eq!(p.outputs[REDEEM_RESERVE_SLOT].amount, 2_000_000);

    // The rest: a zero-share receipt keeps the output count.
    let reserve = input(
        &pool_pda(&MINT.asset).owner,
        &p.outputs[REDEEM_RESERVE_SLOT],
    );
    let full = redeem(&address(&user), reserve, left.clone(), 2_000_000, NOW + 5).unwrap();
    check_outputs(&full, REDEEM_BLINDING_DOMAIN, &left);
    assert_eq!(owned(&full), 3);
    assert_eq!(full.outputs.len(), p.outputs.len(), "the same shape");
    let (_, state) = receipt_out(&full, &user, REDEEM_RECEIPT_SLOT);
    assert_eq!(state.supply_shares, 0);
    assert_eq!(full.outputs[REDEEM_RESERVE_SLOT].amount, 0);
    assert_eq!(reserve_out(&full, REDEEM_RESERVE_SLOT).supply_shares, 0);
}

#[test]
fn redeem_refuses_non_owner_overdraw_and_stale_now() {
    let user = keypair(7);
    let other = keypair(9);
    let (_, reserve, receipt) = supplied(&user, 3_000_000);
    assert!(redeem(&address(&user), reserve.clone(), receipt.clone(), 1, NOW).is_ok());
    // Someone else redeeming the user's receipt.
    refused(
        redeem(&address(&other), reserve.clone(), receipt.clone(), 1, NOW),
        "circuits/redeem.rs",
        "require_eq(receipt.owner(), *public.user_owner.owner());",
    );
    // More than the receipt holds, though the pool has it.
    let (_, big_reserve, _) = supplied(&other, 50_000_000);
    let big = ReserveState {
        supply_shares: 53_000_000,
        ..decode(&big_reserve).unwrap()
    };
    refused(
        redeem(
            &address(&user),
            reserve_with(&big, 53_000_000),
            receipt.clone(),
            3_000_001,
            NOW,
        ),
        "circuits/math.rs",
        "require(a >= b);",
    );
    // More than the reserve's idle balance, though the receipt holds it.
    let thin = ReserveState {
        supply_shares: 3_000_000,
        ..decode(&reserve).unwrap()
    };
    refused(
        redeem(
            &address(&user),
            reserve_with(&thin, 2_999_999),
            receipt.clone(),
            3_000_000,
            NOW,
        ),
        "utxo/balance.rs",
        "crate::require_eq(high, u64::from(borrow));",
    );
    assert!(redeem(
        &address(&user),
        reserve_with(&thin, 3_000_000),
        receipt.clone(),
        3_000_000,
        NOW
    )
    .is_ok());
    // Zero, and `now` before the reserve's last update.
    refused(
        redeem(&address(&user), reserve.clone(), receipt.clone(), 0, NOW),
        "circuits/redeem.rs",
        "require(p.amount != 0);",
    );
    refused(
        redeem(
            &address(&user),
            reserve.clone(),
            receipt.clone(),
            1,
            NOW - 1,
        ),
        "circuits/math.rs",
        "require(now >= state.last_update);",
    );
    // Another mint's reserve.
    let (_, other_reserve) = fresh_reserve(OTHER_MINT);
    refused(
        redeem(&address(&user), other_reserve, receipt, 1, NOW),
        "circuits/notes.rs",
        "require_eq(hi, mint_hi);",
    );
}

#[test]
fn redeem_rounds_for_the_pool() {
    // Price 1.5: one token burns ceil(1 / 1.5) = 1 share; three shares are
    // worth 4 tokens, not 4.5.
    let user = keypair(7);
    let state = ReserveState {
        supply_shares: 3,
        supply_price: PRICE + PRICE / 2,
        borrow_price: PRICE,
        last_update: NOW,
        ..Default::default()
    };
    let p = redeem(
        &address(&user),
        reserve_with(&state, 10),
        receipt_with(&user, 3),
        1,
        NOW,
    )
    .unwrap();
    assert_eq!(
        receipt_out(&p, &user, REDEEM_RECEIPT_SLOT).1.supply_shares,
        2
    );
    refused(
        redeem(
            &address(&user),
            reserve_with(&state, 10),
            receipt_with(&user, 3),
            5,
            NOW,
        ),
        "circuits/math.rs",
        "require(a >= b);",
    );
    let p = redeem(
        &address(&user),
        reserve_with(&state, 10),
        receipt_with(&user, 3),
        4,
        NOW,
    )
    .unwrap();
    assert_eq!(
        receipt_out(&p, &user, REDEEM_RECEIPT_SLOT).1.supply_shares,
        0
    );
}

#[test]
fn redeem_instruction_has_the_keeper_paying_and_the_owner_read_only() {
    let user = keypair(7);
    let (_, reserve, receipt) = supplied(&user, 3_000_000);
    let p = redeem(&address(&user), reserve, receipt, 1_000_000, NOW).unwrap();
    // The head of the instruction, before proving: keeper, then owner.
    let head = p.head();
    assert_eq!(head.len(), 2);
    assert_eq!(head[0].pubkey, wallet(&keeper()));
    assert!(head[0].is_signer && head[0].is_writable);
    assert_eq!(head[1].pubkey, wallet(&user));
    assert!(head[1].is_signer && !head[1].is_writable);
    assert_eq!(p.spp.payer, wallet(&keeper()), "the keeper pays SPP");
}

// The compiled circuits: valid inputs with one witness changed.

const IN_EXECUTE_EQ: &str = "execute → require_eq";

#[test]
fn compiled_supply_refusals() {
    let user = keypair(7);
    let other = keypair(9);
    let (_, reserve) = fresh_reserve(MINT);
    let intent = supply_intent(&user, 2_000_000);
    let p = supply(&address(&user), reserve.clone(), intent.clone(), NOW).unwrap();
    let c = Compiled {
        id: CircuitId::Supply,
        inputs: &p.proof_inputs,
        execute: Supply::execute,
    };
    // A receipt for someone other than the intent's maker.
    let op = supply(
        &address(&other),
        reserve.clone(),
        supply_intent(&other, 2_000_000),
        NOW,
    )
    .unwrap();
    c.refuses(
        under(&op.proof_inputs, "program.public.user_owner"),
        "circuits/supply.rs",
        "require_eq(intent.owner(), *public.user_owner.owner());",
        IN_EXECUTE_EQ,
    );
    // An intent for another pool.
    let mut state: IntentState = decode(&intent).unwrap();
    state.target = TUSD + 1;
    c.refuses(
        state_leaves("intent", NoteKind::Intent, &state, None),
        "circuits/notes.rs",
        "require_eq(intent.target, target);",
        "assert_intent → require_eq",
    );
    reserve_refusals(&c, &reserve, NOW);
}

#[test]
fn compiled_redeem_refusals() {
    let user = keypair(7);
    let other = keypair(9);
    let (_, reserve, receipt) = supplied(&user, 3_000_000);
    let p = redeem(
        &address(&user),
        reserve.clone(),
        receipt.clone(),
        1_000_000,
        NOW,
    )
    .unwrap();
    let c = Compiled {
        id: CircuitId::Redeem,
        inputs: &p.proof_inputs,
        execute: Redeem::execute,
    };
    // More than the receipt's shares. The compiled chain shows `sub` inlined
    // into `execute`; the native run pins the line.
    let mut state: ReceiptState = decode(&receipt).unwrap();
    state.supply_shares = 999_999;
    c.refuses(
        state_leaves("receipt", NoteKind::Receipt, &state, None),
        "circuits/math.rs",
        "require(a >= b);",
        "execute → require",
    );
    // A receipt of another pool.
    let mut state: ReceiptState = decode(&receipt).unwrap();
    state.pool = TUSD + 1;
    c.refuses(
        state_leaves("receipt", NoteKind::Receipt, &state, None),
        "circuits/redeem.rs",
        "require_eq(receipt.pool, pool::TUSD);",
        IN_EXECUTE_EQ,
    );
    // The owner's PDA with another signer's identity: only that check.
    c.refuses(
        refusal::set("program.public.owner_identity", identity_of(&other)),
        "circuits/redeem.rs",
        "require_eq(owner.key().identity()?, public.owner_identity);",
        IN_EXECUTE_EQ,
    );
    // Another signer's PDA: the receipt is not theirs.
    let (_, oreserve, oreceipt) = supplied(&other, 3_000_000);
    let op = redeem(&address(&other), oreserve, oreceipt, 1_000_000, NOW).unwrap();
    c.refuses(
        under(&op.proof_inputs, "program.public.user_owner"),
        "circuits/redeem.rs",
        "require_eq(receipt.owner(), *public.user_owner.owner());",
        IN_EXECUTE_EQ,
    );
    reserve_refusals(&c, &reserve, NOW);
}
