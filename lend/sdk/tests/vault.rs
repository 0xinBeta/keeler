//! The Vault builders natively (no network, no proofs): every rule the
//! circuits enforce, the outputs' owners and keys, and the keeper reading
//! every output with its transaction viewing key.
mod refusal;
mod utxo;

use keeler_lend::circuits::{Close, DepositAdd, DepositOpen, Draw, Liquidate, Repay};
use keeler_lend_sdk::proof::CircuitId;
use refusal::{
    field, identity_of, refused, refused_because, reserve_leaves, reserve_refusals, set,
    state_leaves, under, Compiled,
};

use keeler_core::lend as core;
use keeler_lend::circuits::{
    vault::{DEPOSIT_BLINDING_DOMAIN, POSITION_BLINDING_DOMAIN},
    CLOSE_PAYOUT_SLOT, DEPOSIT_POSITION_SLOT, DRAW_COLLATERAL_SLOT, DRAW_DEBT_SLOT,
    DRAW_POSITION_SLOT, DRAW_RESERVE_SLOT, LIQUIDATE_CHANGE_SLOT, LIQUIDATE_PAYOUT_SLOT,
    LIQUIDATE_POSITION_SLOT, LIQUIDATE_RESERVE_SLOT, REPAY_CHANGE_SLOT, REPAY_POSITION_SLOT,
    REPAY_RESERVE_SLOT,
};
use keeler_lend::vaults::{TSOL_TUSD, TSOL_TUSD_MIN_COLLATERAL, TSOL_TUSD_MIN_DEBT};
use keeler_lend_sdk::{
    builders::{self, Prepared},
    decode, note_data_hash, pool_pda, purpose, user_pda,
    vault::{self, blinding_seed, read_outputs},
    IntentState, NoteKind, PositionState, ReserveState, ADMIN,
};
use solana_keypair::Keypair;
use spr_keypair::{ShieldedAddress, ShieldedKeypair};
use spr_program::DataUtxo;
use spr_transaction::{Data, DataRecord, Mint, SppProofOutputUtxo, Utxo, WalletUtxo};

const TREE: u16 = 0;
const V: u64 = 1;
const NOW: u64 = 1_800_000_000;
const EXPIRY: u64 = 2_000_000_000;
/// $150 per tSOL: tUSD base units per tSOL base unit x 1e12.
const P150: u64 = 150_000_000_000;
const SOL: u64 = 1_000_000_000;
const USD: u64 = 1_000_000;
const SALT: u128 = 9;

fn coll() -> Mint {
    Mint::new(TSOL_TUSD.collateral_mint, 1)
}
fn debt() -> Mint {
    Mint::new(TSOL_TUSD.debt_mint, 2)
}

fn keypair(seed: u8) -> ShieldedKeypair {
    ShieldedKeypair::from_keypair(&Keypair::new_from_array([seed; 32])).unwrap()
}
fn keeper() -> ShieldedKeypair {
    keypair(1)
}
fn addr(k: &ShieldedKeypair) -> ShieldedAddress {
    k.shielded_address().unwrap()
}
fn pda_input(user: &ShieldedKeypair, out: &SppProofOutputUtxo, leaf: u64) -> WalletUtxo {
    user_pda(&addr(user).solana_address().unwrap())
        .owner
        .input(out, TREE, leaf)
        .unwrap()
}

fn note(owner: &ShieldedKeypair, mint: Mint, amount: u64, blinding: u8) -> WalletUtxo {
    utxo::wallet_utxo(
        Utxo {
            owner: owner.signing_pubkey(),
            asset: mint,
            amount,
            blinding: [blinding; 32],
            ring_program_id: None,
            data: Data::default(),
        },
        &owner.nullifier_key,
        TREE,
        0,
    )
}

/// An intent note of `user` (built by the intent builder, as the PDA holds it).
fn intent_full(
    user: &ShieldedKeypair,
    mint: Mint,
    amount: u64,
    purpose: u8,
    target: u64,
) -> WalletUtxo {
    let p = builders::intent(
        user,
        note(user, mint, amount + 1, 5),
        &mint.asset,
        amount,
        purpose,
        target,
        EXPIRY,
        keeper().viewing_pubkey(),
        TREE,
    )
    .unwrap();
    pda_input(user, &p.outputs[keeler_lend::circuits::INTENT_SLOT], 1)
}

/// A reserve holding `liquidity` with as many supply shares (price 1e12),
/// then `edit`ed: a crafted note with a valid data hash, as the pool PDA holds it.
fn reserve_with(liquidity: u64, edit: impl FnOnce(&mut ReserveState)) -> WalletUtxo {
    let p = builders::init_reserve(&keeper(), ADMIN, debt(), NOW, 42, TREE).unwrap();
    let out = p.outputs.iter().find(|o| o.data_hash.is_some()).unwrap();
    let pool = pool_pda(&debt().asset).owner;
    let decoded = DataUtxo::<ReserveState>::try_from(pool.input(out, TREE, 1).unwrap()).unwrap();
    let address = decoded.address.unwrap();
    let mut state = decoded.data;
    state.supply_shares = liquidity;
    edit(&mut state);
    let bytes = borsh::to_vec(&state).unwrap();
    let mut out = out.clone();
    out.amount = liquidity;
    out.data = Data::new(vec![DataRecord::UtxoData([&address[..], &bytes].concat())]);
    out.data_hash = Some(note_data_hash(NoteKind::Reserve, &bytes, Some(&address)).unwrap());
    pool.input(&out, TREE, 1).unwrap()
}
fn reserve(liquidity: u64) -> WalletUtxo {
    reserve_with(liquidity, |_| {})
}
fn reserve_out(p: &Prepared, slot: usize) -> WalletUtxo {
    pool_pda(&debt().asset)
        .owner
        .input(&p.outputs[slot], TREE, 3)
        .unwrap()
}

/// `user`'s position `id` holding `amount` of tSOL, opened by the keeper.
fn open(user: &ShieldedKeypair, amount: u64, id: u64) -> WalletUtxo {
    let p = vault::deposit_open(
        &keeper(),
        &addr(user),
        intent_full(user, coll(), amount, purpose::DEPOSIT, id),
        V,
        NOW,
        TREE,
    )
    .unwrap();
    pda_input(user, &p.outputs[DEPOSIT_POSITION_SLOT], 2)
}

fn draw(
    user: &ShieldedKeypair,
    reserve: WalletUtxo,
    position: WalletUtxo,
    borrow: u64,
    withdraw: u64,
    bound: u64,
) -> anyhow::Result<Prepared> {
    vault::draw(
        &keeper(),
        &addr(user),
        reserve,
        position,
        borrow,
        withdraw,
        TSOL_TUSD.price_account,
        V,
        NOW,
        bound,
        SALT,
        TREE,
    )
}

/// `user`'s position (1 tSOL) after borrowing `borrow`, and the reserve.
fn borrowed(user: &ShieldedKeypair, borrow: u64) -> (WalletUtxo, WalletUtxo) {
    let p = draw(
        user,
        reserve(1_000 * USD),
        open(user, SOL, 7),
        borrow,
        0,
        P150,
    )
    .unwrap();
    (
        pda_input(user, &p.outputs[DRAW_POSITION_SLOT], 4),
        reserve_out(&p, DRAW_RESERVE_SLOT),
    )
}

fn viewing_of(out: &SppProofOutputUtxo) -> [u8; 33] {
    *out.owner_address.unwrap().viewing_pubkey.as_bytes()
}

#[test]
fn deposit_open_credits_the_maker_and_the_keeper_reads_it() {
    let user = keypair(7);
    let intent = intent_full(&user, coll(), 2 * SOL, purpose::DEPOSIT, 7);
    let p = vault::deposit_open(&keeper(), &addr(&user), intent.clone(), V, NOW, TREE).unwrap();
    let out = &p.outputs[DEPOSIT_POSITION_SLOT];
    assert_eq!(out.amount, 2 * SOL);
    assert_eq!(viewing_of(out), *addr(&user).viewing_pubkey.as_bytes());
    let state: PositionState = decode(&pda_input(&user, out, 2)).unwrap();
    let maker: IntentState = decode(&intent).unwrap();
    assert_eq!(
        (
            state.owner,
            state.vault,
            state.position_id,
            state.debt_shares
        ),
        (maker.maker, V, 7, 0)
    );
    // The blinding seed comes from the intent, whose nullifier is published first.
    assert_eq!(p.first_nullifier, intent.nullifier);
    assert_eq!(
        p.spp.blinding_seed,
        blinding_seed(DEPOSIT_BLINDING_DOMAIN, &intent.utxo.blinding).unwrap()
    );
    // The keeper recovers the outputs through its transaction viewing key.
    let read = read_outputs(&keeper(), &p.first_nullifier, &p.spp).unwrap();
    assert_eq!(
        read[DEPOSIT_POSITION_SLOT].as_ref().unwrap().amount,
        2 * SOL
    );
}

#[test]
fn deposit_open_refusals() {
    let user = keypair(7);
    let other = keypair(9);
    let k = keeper();
    let good = || intent_full(&user, coll(), SOL, purpose::DEPOSIT, 7);
    // Credited to someone other than the maker.
    refused(
        vault::deposit_open(&k, &addr(&other), good(), V, NOW, TREE),
        "circuits/deposit.rs",
        "require_eq(intent.owner(), *public_owner.owner());",
    );
    // A repay intent, or a deposit in the debt mint (a junk-mint position).
    let repay = intent_full(&user, coll(), SOL, purpose::REPAY, 7);
    refused(
        vault::deposit_open(&k, &addr(&user), repay, V, NOW, TREE),
        "circuits/deposit.rs",
        "require_eq(intent.purpose, purpose::DEPOSIT);",
    );
    let junk = intent_full(&user, debt(), SOL, purpose::DEPOSIT, 7);
    refused(
        vault::deposit_open(&k, &addr(&user), junk, V, NOW, TREE),
        "circuits/notes.rs",
        "require_eq(hi, mint_hi);",
    );
    // Below the minimum, after expiry, an unknown vault.
    let small = intent_full(
        &user,
        coll(),
        TSOL_TUSD_MIN_COLLATERAL - 1,
        purpose::DEPOSIT,
        7,
    );
    refused(
        vault::deposit_open(&k, &addr(&user), small, V, NOW, TREE),
        "circuits/deposit.rs",
        "require(amount >= rules.min_collateral);",
    );
    refused(
        vault::deposit_open(&k, &addr(&user), good(), V, EXPIRY + 1, TREE),
        "circuits/deposit.rs",
        "require(now <= intent.expiry);",
    );
    refused_because(
        vault::deposit_open(&k, &addr(&user), good(), 2, NOW, TREE),
        "no vault 2",
    );
    assert!(vault::deposit_open(&k, &addr(&user), good(), V, EXPIRY, TREE).is_ok());
}

#[test]
fn deposit_add_only_into_the_named_position_of_the_maker() {
    let user = keypair(7);
    let other = keypair(9);
    let k = keeper();
    let position = open(&user, SOL, 7);
    let add = vault::deposit_add(
        &k,
        &addr(&user),
        position.clone(),
        intent_full(&user, coll(), SOL / 2, purpose::DEPOSIT, 7),
        V,
        NOW,
        TREE,
    )
    .unwrap();
    assert_eq!(add.outputs[DEPOSIT_POSITION_SLOT].amount, SOL + SOL / 2);
    assert_eq!(add.first_nullifier, position.nullifier);
    assert_eq!(
        add.spp.blinding_seed,
        blinding_seed(POSITION_BLINDING_DOMAIN, &position.utxo.blinding).unwrap()
    );
    // An intent naming another position.
    let elsewhere = intent_full(&user, coll(), SOL / 2, purpose::DEPOSIT, 8);
    refused(
        vault::deposit_add(&k, &addr(&user), position.clone(), elsewhere, V, NOW, TREE),
        "circuits/notes.rs",
        "require_eq(intent.target, target);",
    );
    // Someone else's intent into this position, either way round.
    let theirs = intent_full(&other, coll(), SOL / 2, purpose::DEPOSIT, 7);
    refused(
        vault::deposit_add(
            &k,
            &addr(&user),
            position.clone(),
            theirs.clone(),
            V,
            NOW,
            TREE,
        ),
        "circuits/deposit.rs",
        "require_eq(intent.owner(), *public_owner.owner());",
    );
    refused(
        vault::deposit_add(&k, &addr(&other), position, theirs, V, NOW, TREE),
        "circuits/vault.rs",
        "require_eq(position.owner(), *user);",
    );
}

#[test]
fn draw_borrows_to_the_owner() {
    let user = keypair(7);
    let r = reserve(1_000 * USD);
    let p = draw(&user, r, open(&user, SOL, 7), 100 * USD, 0, P150).unwrap();
    let payout = &p.outputs[DRAW_DEBT_SLOT];
    assert_eq!(payout.amount, 100 * USD);
    assert_eq!(payout.owner_address, Some(addr(&user)));
    assert_eq!(p.outputs[DRAW_COLLATERAL_SLOT].amount, 0);
    let state: PositionState =
        decode(&pda_input(&user, &p.outputs[DRAW_POSITION_SLOT], 4)).unwrap();
    assert_eq!(
        state.debt_shares,
        core::borrow_shares(100 * USD, 1_000_000_000_000).unwrap()
    );
    let after: ReserveState = decode(&reserve_out(&p, DRAW_RESERVE_SLOT)).unwrap();
    assert_eq!(after.debt_shares, state.debt_shares);
    assert_eq!(p.outputs[DRAW_RESERVE_SLOT].amount, 900 * USD);
    // The reserve is encrypted to the keeper, the position to the owner.
    assert_eq!(
        viewing_of(&p.outputs[DRAW_RESERVE_SLOT]),
        *keeper().viewing_pubkey().as_bytes()
    );
    assert_eq!(
        viewing_of(&p.outputs[DRAW_POSITION_SLOT]),
        *addr(&user).viewing_pubkey.as_bytes()
    );
    let read = read_outputs(&keeper(), &p.first_nullifier, &p.spp).unwrap();
    assert_eq!(read[DRAW_DEBT_SLOT].as_ref().unwrap().amount, 100 * USD);
    // Exactly at the collateral factor: $150 x 80%.
    assert!(draw(
        &user,
        reserve(1_000 * USD),
        open(&user, SOL, 7),
        120 * USD,
        0,
        P150
    )
    .is_ok());
}

#[test]
fn draw_refusals() {
    let user = keypair(7);
    let other = keypair(9);
    let position = || open(&user, SOL, 7);
    let big = || reserve(1_000 * USD);
    // Not the owner: the circuit ties the position to the signer's identity.
    refused(
        vault::draw(
            &keeper(),
            &addr(&other),
            big(),
            position(),
            10 * USD,
            0,
            TSOL_TUSD.price_account,
            V,
            NOW,
            P150,
            SALT,
            TREE,
        ),
        "circuits/vault.rs",
        "require_eq(position.owner(), *user);",
    );
    // Past the collateral factor, at a lower bound.
    refused(
        draw(&user, big(), position(), 120 * USD + 1, 0, P150),
        "circuits/draw.rs",
        "require(can_draw(",
    );
    refused(
        draw(&user, big(), position(), 100 * USD, 0, 120_000_000_000),
        "circuits/draw.rs",
        "require(can_draw(",
    );
    // Past the idle liquidity.
    refused(
        draw(&user, reserve(50 * USD), position(), 60 * USD, 0, P150),
        "circuits/draw.rs",
        "require(p.borrow <= reserve.amount()?);",
    );
    // Past the 95% utilisation cap (at it is fine).
    refused(
        draw(&user, reserve(100 * USD), position(), 96 * USD, 0, P150),
        "circuits/draw.rs",
        "require(!(borrowing && over_cap));",
    );
    assert!(draw(&user, reserve(100 * USD), position(), 95 * USD, 0, P150).is_ok());
    // Below the minimum debt.
    refused(
        draw(&user, big(), position(), TSOL_TUSD_MIN_DEBT - 1, 0, P150),
        "circuits/draw.rs",
        "require(zero_or_at_least(debt, rules.min_debt));",
    );
    // Withdrawing past the factor or below the minimum collateral.
    let (pos, r) = borrowed(&user, 100 * USD);
    refused(
        draw(&user, r.clone(), pos.clone(), 0, SOL / 5, P150),
        "circuits/draw.rs",
        "require(can_draw(",
    );
    assert!(draw(&user, r.clone(), pos.clone(), 0, SOL / 100, P150).is_ok());
    let p = draw(
        &user,
        big(),
        position(),
        0,
        SOL - TSOL_TUSD_MIN_COLLATERAL + 1,
        P150,
    );
    refused(
        p,
        "circuits/draw.rs",
        "require(zero_or_at_least(collateral, rules.min_collateral));",
    );
    // All of it with no debt is fine, paid to the owner.
    let p = draw(&user, big(), position(), 0, SOL, P150).unwrap();
    assert_eq!(p.outputs[DRAW_COLLATERAL_SLOT].amount, SOL);
    assert_eq!(
        p.outputs[DRAW_COLLATERAL_SLOT].owner_address,
        Some(addr(&user))
    );
}

#[test]
fn draw_accrues_first() {
    let user = keypair(7);
    let year = 365 * 24 * 3600;
    let r = reserve_with(1_000 * USD, |s| {
        s.debt_shares = 500 * USD;
        s.rate_bps = 500;
        s.last_update = NOW - year;
    });
    let before: ReserveState = decode(&r).unwrap();
    let p = draw(&user, r.clone(), open(&user, SOL, 7), 10 * USD, 0, P150).unwrap();
    let after: ReserveState = decode(&reserve_out(&p, DRAW_RESERVE_SLOT)).unwrap();
    let expected = vault::accrued(&before, NOW).unwrap();
    assert!(expected.borrow_price > 1_000_000_000_000);
    assert_eq!(
        (after.borrow_price, after.supply_price, after.last_update),
        (expected.borrow_price, expected.supply_price, NOW)
    );
    // The new shares are at the accrued price.
    let state: PositionState =
        decode(&pda_input(&user, &p.outputs[DRAW_POSITION_SLOT], 4)).unwrap();
    assert_eq!(
        state.debt_shares,
        core::borrow_shares(10 * USD, expected.borrow_price).unwrap()
    );
    // A reserve updated after `now` is refused.
    let future = reserve_with(1_000 * USD, |s| s.last_update = NOW + 1);
    refused(
        draw(&user, future, open(&user, SOL, 7), 10 * USD, 0, P150),
        "circuits/math.rs",
        "require(now >= state.last_update);",
    );
}

fn repay(
    user: &ShieldedKeypair,
    r: WalletUtxo,
    position: WalletUtxo,
    intent: WalletUtxo,
) -> anyhow::Result<Prepared> {
    vault::repay(
        &keeper(),
        &addr(user),
        r,
        position,
        intent,
        V,
        NOW,
        SALT,
        TREE,
    )
}

#[test]
fn repay_partial_and_with_change() {
    let user = keypair(7);
    let (pos, r) = borrowed(&user, 100 * USD);
    let shares = decode::<PositionState>(&pos).unwrap().debt_shares;
    let p = repay(
        &user,
        r.clone(),
        pos.clone(),
        intent_full(&user, debt(), 40 * USD, purpose::REPAY, 7),
    )
    .unwrap();
    let after: PositionState =
        decode(&pda_input(&user, &p.outputs[REPAY_POSITION_SLOT], 5)).unwrap();
    let want = core::repay_shares(shares, 40 * USD, 1_000_000_000_000).unwrap();
    assert_eq!(after.debt_shares, want.shares);
    assert_eq!(p.outputs[REPAY_CHANGE_SLOT].amount, 0);
    assert_eq!(p.outputs[REPAY_RESERVE_SLOT].amount, 940 * USD);
    // Paying more than the debt clears it and returns the change to the maker.
    let p = repay(
        &user,
        r.clone(),
        pos.clone(),
        intent_full(&user, debt(), 150 * USD, purpose::REPAY, 7),
    )
    .unwrap();
    let after: PositionState =
        decode(&pda_input(&user, &p.outputs[REPAY_POSITION_SLOT], 5)).unwrap();
    assert_eq!(after.debt_shares, 0);
    let change = &p.outputs[REPAY_CHANGE_SLOT];
    assert_eq!(
        (change.amount, change.owner_address),
        (50 * USD, Some(addr(&user)))
    );
    assert_eq!(p.outputs[REPAY_RESERVE_SLOT].amount, 1_000 * USD);
}

#[test]
fn repay_refusals() {
    let user = keypair(7);
    let other = keypair(9);
    let (pos, r) = borrowed(&user, 100 * USD);
    // Into another position, the wrong purpose, the wrong mint.
    let elsewhere = intent_full(&user, debt(), 10 * USD, purpose::REPAY, 8);
    refused(
        repay(&user, r.clone(), pos.clone(), elsewhere),
        "circuits/notes.rs",
        "require_eq(intent.target, target);",
    );
    let deposit = intent_full(&user, debt(), 10 * USD, purpose::DEPOSIT, 7);
    refused(
        repay(&user, r.clone(), pos.clone(), deposit),
        "circuits/notes.rs",
        "require_eq(intent.purpose, purpose);",
    );
    let junk = intent_full(&user, coll(), 10 * USD, purpose::REPAY, 7);
    refused(
        repay(&user, r.clone(), pos.clone(), junk),
        "circuits/notes.rs",
        "require_eq(hi, mint_hi);",
    );
    // Someone else's intent into this position.
    let theirs = intent_full(&other, debt(), 10 * USD, purpose::REPAY, 7);
    refused(
        repay(&user, r.clone(), pos.clone(), theirs.clone()),
        "circuits/repay.rs",
        "require_eq(intent.owner(), *user);",
    );
    refused(
        repay(&other, r.clone(), pos.clone(), theirs),
        "circuits/vault.rs",
        "require_eq(position.owner(), *user);",
    );
    // After expiry.
    let late = vault::repay(
        &keeper(),
        &addr(&user),
        r,
        pos,
        intent_full(&user, debt(), 10 * USD, purpose::REPAY, 7),
        V,
        EXPIRY + 1,
        SALT,
        TREE,
    );
    refused(late, "circuits/notes.rs", "require(now <= intent.expiry);");
}

fn liquidate_with(
    user: &ShieldedKeypair,
    r: WalletUtxo,
    pos: WalletUtxo,
    x: u64,
    bound: u64,
) -> anyhow::Result<Prepared> {
    vault::liquidate_with(
        &keeper(),
        &addr(user),
        r,
        pos,
        note(&keeper(), debt(), 1_000 * USD, 6),
        x,
        TSOL_TUSD.price_account,
        V,
        NOW,
        bound,
        SALT,
        TREE,
    )
}

fn liquidate(
    user: &ShieldedKeypair,
    r: WalletUtxo,
    pos: WalletUtxo,
    bound: u64,
) -> anyhow::Result<(Prepared, core::Liquidation)> {
    vault::liquidate(
        &keeper(),
        &addr(user),
        r,
        pos,
        note(&keeper(), debt(), 1_000 * USD, 6),
        TSOL_TUSD.price_account,
        V,
        NOW,
        bound,
        SALT,
        TREE,
    )
}

#[test]
fn keepers_sharing_the_pool_key_read_and_spend_each_others_pool_notes() {
    let keeper = |seed| {
        keeler_lend_sdk::fixture_keeper_keypair(&Keypair::new_from_array([seed; 32])).unwrap()
    };
    let (a, b) = (keeper(1), keeper(2));
    assert_eq!(
        a.viewing_pubkey().as_bytes(),
        &keeler_lend_sdk::POOL_VIEWING
    );
    assert_eq!(a.viewing_pubkey(), b.viewing_pubkey());
    // Each keeps its own signing and nullifier keys: they derive from its key.
    assert_eq!(a.signing_pubkey(), keypair(1).signing_pubkey());
    assert_eq!(
        a.nullifier_key.pubkey().unwrap(),
        keypair(1).nullifier_key.pubkey().unwrap()
    );
    assert_ne!(a.owner_hash().unwrap(), b.owner_hash().unwrap());

    // A reserve `a` writes: `b` reads it from the transaction, a keeper with
    // its own viewing key cannot.
    let p = builders::init_reserve(&a, ADMIN, debt(), NOW, 42, TREE).unwrap();
    let slot = p
        .outputs
        .iter()
        .position(|o| o.data_hash.is_some())
        .unwrap();
    let read = read_outputs(&b, &p.first_nullifier, &p.spp).unwrap();
    assert_eq!(read[slot].as_ref().unwrap().amount, p.outputs[slot].amount);
    assert!(read_outputs(&keypair(2), &p.first_nullifier, &p.spp).is_err());

    // `b` spends it: a supply on top of `a`'s reserve.
    let pool = pool_pda(&debt().asset).owner;
    let r = pool.input(&p.outputs[slot], TREE, 1).unwrap();
    let user = keypair(7);
    let intent = intent_full(
        &user,
        debt(),
        10 * USD,
        purpose::SUPPLY,
        keeler_lend::pool::TUSD,
    );
    let s = builders::supply(&b, &addr(&user), r, intent, NOW, SALT, TREE).unwrap();
    // And `a` reads `b`'s outputs, the new reserve with the 10 tUSD among them.
    let read = read_outputs(&a, &s.first_nullifier, &s.spp).unwrap();
    for (out, plain) in s.outputs.iter().zip(&read) {
        if let Some(plain) = plain {
            assert_eq!(plain.amount, out.amount);
        }
    }
    assert!(read
        .iter()
        .any(|o| o.as_ref().is_some_and(|o| o.amount == 10 * USD)));
}

#[test]
fn liquidate_back_to_the_threshold() {
    let user = keypair(7);
    let (pos, r) = borrowed(&user, 120 * USD);
    // $140: value 140, threshold 119 < 120 debt; max limit 126 is not passed.
    let bound = 140_000_000_000;
    let (p, plan) = liquidate(&user, r.clone(), pos.clone(), bound).unwrap();
    assert!(plan.repay > 0 && plan.repay < 120 * USD);
    // The payout formula is unchanged: x plus the 1% penalty, at the bound.
    let formula =
        ((plan.repay + plan.repay / 100) as u128 * 1_000_000_000_000 / bound as u128) as u64;
    assert_eq!(plan.payout, formula.min(SOL));
    let payout = &p.outputs[LIQUIDATE_PAYOUT_SLOT];
    assert_eq!(
        (payout.amount, payout.owner_address),
        (plan.payout, Some(addr(&keeper())))
    );
    assert_eq!(p.outputs[LIQUIDATE_POSITION_SLOT].amount, SOL - plan.payout);
    assert_eq!(
        viewing_of(&p.outputs[LIQUIDATE_POSITION_SLOT]),
        *addr(&user).viewing_pubkey.as_bytes()
    );
    // The liquidator pays exactly x; the reserve gets x and nothing more.
    assert_eq!(
        p.outputs[LIQUIDATE_RESERVE_SLOT].amount,
        880 * USD + plan.repay
    );
    let change = &p.outputs[LIQUIDATE_CHANGE_SLOT];
    assert_eq!(
        (change.amount, change.owner_address),
        (1_000 * USD - plan.repay, Some(addr(&keeper())))
    );
    let after: ReserveState = decode(&reserve_out(&p, LIQUIDATE_RESERVE_SLOT)).unwrap();
    let left: PositionState =
        decode(&pda_input(&user, &p.outputs[LIQUIDATE_POSITION_SLOT], 6)).unwrap();
    assert_eq!(after.debt_shares, left.debt_shares);
    assert_eq!(after.supply_shares, 1_000 * USD);
    // A note of exactly x leaves the change slot empty.
    let exact = vault::liquidate_with(
        &keeper(),
        &addr(&user),
        r.clone(),
        pos.clone(),
        note(&keeper(), debt(), plan.repay, 6),
        plan.repay,
        TSOL_TUSD.price_account,
        V,
        NOW,
        bound,
        SALT,
        TREE,
    )
    .unwrap();
    assert_eq!(exact.outputs[LIQUIDATE_CHANGE_SLOT].amount, 0);
    assert_eq!(exact.outputs[LIQUIDATE_PAYOUT_SLOT].amount, plan.payout);
    // Less is fine; more takes the position below the threshold.
    assert!(liquidate_with(&user, r.clone(), pos.clone(), plan.repay / 2, bound).is_ok());
    refused(
        liquidate_with(&user, r.clone(), pos.clone(), plan.repay + USD, bound),
        "circuits/liquidate.rs",
        "require(absorb || at_or_past_threshold(left, worth_after, &v));",
    );
    refused(
        liquidate_with(&user, r.clone(), pos.clone(), 120 * USD, bound),
        "circuits/liquidate.rs",
        "require(absorb || at_or_past_threshold(left, worth_after, &v));",
    );
    refused(
        liquidate_with(&user, r, pos, 0, bound),
        "circuits/liquidate.rs",
        "require(x != 0 && x <= debt);",
    );
}

#[test]
fn liquidate_refuses_healthy_and_takes_all_above_the_max_limit() {
    let user = keypair(7);
    let (pos, r) = borrowed(&user, 100 * USD);
    // Healthy at $150 (threshold 127.5) and at exactly the threshold.
    refused(
        liquidate_with(&user, r.clone(), pos.clone(), USD, P150),
        "circuits/liquidate.rs",
        "require(is_liquidatable(debt, worth, &v));",
    );
    refused_because(
        liquidate(&user, r.clone(), pos.clone(), P150),
        "NotLiquidatable",
    );
    let (pos, r) = borrowed(&user, 120 * USD);
    // $130: 120 > 0.9 x 130 = 117, past the max limit: all of it may go.
    let bound = 130_000_000_000;
    let (p, plan) = liquidate(&user, r.clone(), pos.clone(), bound).unwrap();
    assert_eq!(plan.repay, 120 * USD);
    let left: PositionState =
        decode(&pda_input(&user, &p.outputs[LIQUIDATE_POSITION_SLOT], 6)).unwrap();
    assert_eq!(left.debt_shares, 0);
    refused(
        liquidate_with(&user, r.clone(), pos.clone(), 120 * USD + 1, bound),
        "circuits/liquidate.rs",
        "require(x != 0 && x <= debt);",
    );
    // The payout caps at the collateral when the price has fallen far enough.
    let crash = 100_000_000_000;
    let (p, plan) = liquidate(&user, r, pos, crash).unwrap();
    assert_eq!(plan.payout, SOL);
    assert_eq!(p.outputs[LIQUIDATE_POSITION_SLOT].amount, 0);
}

#[test]
fn liquidate_accepts_the_reference_plans() {
    let user = keypair(7);
    for (borrow, bound) in [
        (120 * USD, 141_000_000_000),
        (120 * USD, 139_500_000_000),
        (110 * USD, 129_000_000_000),
        (115 * USD, 133_333_333_333),
        (120 * USD, 125_000_000_000),
        (100 * USD, 110_000_000_000),
    ] {
        let (pos, r) = borrowed(&user, borrow);
        let (p, plan) = liquidate(&user, r, pos, bound)
            .unwrap_or_else(|e| panic!("borrow {borrow} at {bound}: {e}"));
        assert_eq!(p.outputs[LIQUIDATE_PAYOUT_SLOT].amount, plan.payout);
    }
}

#[test]
fn close_only_without_debt_and_only_by_the_owner() {
    let user = keypair(7);
    let other = keypair(9);
    let position = open(&user, SOL, 7);
    let p = vault::close(&user, position.clone(), V, TREE).unwrap();
    let out = &p.outputs[CLOSE_PAYOUT_SLOT];
    assert_eq!((out.amount, out.owner_address), (SOL, Some(addr(&user))));
    refused(
        vault::close(&other, position, V, TREE),
        "circuits/vault.rs",
        "require_eq(position.owner(), *user);",
    );
    let (pos, _) = borrowed(&user, 10 * USD);
    refused(
        vault::close(&user, pos, V, TREE),
        "circuits/close.rs",
        "require_eq(position.debt_shares, 0u64);",
    );
}

// --- the compiled circuits: valid inputs with one witness changed ---

const LIQ: &str = "circuits/liquidate.rs";
const VAULT_RS: &str = "circuits/vault.rs";

fn position_leaves(state: &PositionState) -> Vec<(String, xark::prover::Fr)> {
    state_leaves("position", NoteKind::Position, state, None)
}

fn intent_leaves(state: &IntentState) -> Vec<(String, xark::prover::Fr)> {
    state_leaves("intent", NoteKind::Intent, state, None)
}

/// A direct `require` in the circuit's `execute`.
const IN_EXECUTE: &str = "execute → require";
const IN_EXECUTE_EQ: &str = "execute → require_eq";

#[test]
fn compiled_liquidate_refusals() {
    let user = keypair(7);
    let other = keypair(9);
    let (pos, r) = borrowed(&user, 120 * USD);
    let bound = 140_000_000_000;
    let (p, plan) = liquidate(&user, r.clone(), pos.clone(), bound).unwrap();
    let i = &p.proof_inputs;
    let c = Compiled {
        id: CircuitId::Liquidate,
        inputs: i,
        execute: Liquidate::execute,
    };
    // A healthy position: the same one at $150.
    c.refuses(
        field(i, "program.public.{now,bound}", 64, 64, P150.into()),
        LIQ,
        "require(is_liquidatable(",
        IN_EXECUTE,
    );
    // Below the threshold: more than the plan. And zero.
    let repay = |x: u64| field(i, "program.private.{repay,salt}", 0, 64, x.into());
    c.refuses(
        repay(plan.repay + USD),
        LIQ,
        "require(absorb || at_or_past_threshold(",
        IN_EXECUTE,
    );
    c.refuses(repay(0), LIQ, "require(x != 0 && x <= debt)", IN_EXECUTE);
    // A position of another vault, or another owner's user PDA as the target.
    let mut state: PositionState = decode(&pos).unwrap();
    state.vault = 2;
    c.refuses(
        position_leaves(&state),
        VAULT_RS,
        "require_eq(position.vault, vault)",
        "assert_position → require_eq",
    );
    let (opos, or) = borrowed(&other, 120 * USD);
    let (op, _) = liquidate(&other, or, opos, bound).unwrap();
    c.refuses(
        under(&op.proof_inputs, "program.public.user_owner"),
        VAULT_RS,
        "require_eq(position.owner(), *user)",
        "assert_position → require_eq",
    );
    reserve_refusals(&c, &r, NOW);
}

#[test]
fn liquidate_removes_at_least_one_debt_share() {
    let user = keypair(7);
    // Borrow price 1.5e12: a repay of 1 is less than one share's worth.
    let r = reserve_with(1_000 * USD, |s| s.borrow_price = 1_500_000_000_000);
    let p = draw(&user, r, open(&user, SOL, 7), 120 * USD, 0, P150).unwrap();
    let pos = pda_input(&user, &p.outputs[DRAW_POSITION_SLOT], 4);
    let r = reserve_out(&p, DRAW_RESERVE_SLOT);
    let bound = 140_000_000_000;
    refused(
        liquidate_with(&user, r.clone(), pos.clone(), 1, bound),
        LIQ,
        "require(removed != 0)",
    );
    let (p, plan) = liquidate(&user, r.clone(), pos.clone(), bound).unwrap();
    assert!(plan.repay > 1);
    // Two units remove one share: accepted.
    assert!(liquidate_with(&user, r, pos, 2, bound).is_ok());
    Compiled {
        id: CircuitId::Liquidate,
        inputs: &p.proof_inputs,
        execute: Liquidate::execute,
    }
    .refuses(
        field(&p.proof_inputs, "program.private.{repay,salt}", 0, 64, 1),
        LIQ,
        "require(removed != 0)",
        IN_EXECUTE,
    );
}

#[test]
fn repay_removes_at_least_one_debt_share() {
    let user = keypair(7);
    let r = reserve_with(1_000 * USD, |s| s.borrow_price = 1_500_000_000_000);
    let p = draw(&user, r, open(&user, SOL, 7), 100 * USD, 0, P150).unwrap();
    let pos = pda_input(&user, &p.outputs[DRAW_POSITION_SLOT], 4);
    let r = reserve_out(&p, DRAW_RESERVE_SLOT);
    let shares = decode::<PositionState>(&pos).unwrap().debt_shares;

    let dust = intent_full(&user, debt(), 1, purpose::REPAY, 7);
    refused_because(
        repay(&user, r.clone(), pos.clone(), dust.clone()),
        "ZeroShares",
    );
    let back =
        builders::cancel_intent(&user, dust.clone(), keeper().viewing_pubkey(), TREE).unwrap();
    let refund = &back.outputs[keeler_lend::circuits::CANCEL_REFUND_SLOT];
    assert_eq!(
        (refund.amount, refund.owner_address),
        (1, Some(addr(&user)))
    );

    let two = intent_full(&user, debt(), 2, purpose::REPAY, 7);
    let p = repay(&user, r.clone(), pos.clone(), two).unwrap();
    let after: PositionState =
        decode(&pda_input(&user, &p.outputs[REPAY_POSITION_SLOT], 5)).unwrap();
    assert_eq!(after.debt_shares, shares - 1);
    Compiled {
        id: CircuitId::Repay,
        inputs: &p.proof_inputs,
        execute: Repay::execute,
    }
    .refuses(
        reserve_leaves(&r, |s| s.borrow_price = 3_000_000_000_000, None),
        "circuits/repay.rs",
        "require(left == 0 || removed != 0)",
        IN_EXECUTE,
    );
}

#[test]
fn compiled_draw_refusals() {
    let user = keypair(7);
    let other = keypair(9);
    let r = reserve(1_000 * USD);
    let p = draw(&user, r.clone(), open(&user, SOL, 7), 100 * USD, 0, P150).unwrap();
    let i = &p.proof_inputs;
    let c = Compiled {
        id: CircuitId::Draw,
        inputs: i,
        execute: Draw::execute,
    };
    let borrow = |x: u64| field(i, "program.private.{borrow,withdraw}", 0, 64, x.into());
    // Past the collateral factor: $150 x 80% is 120.
    c.refuses(
        borrow(120 * USD + 1),
        "circuits/draw.rs",
        "require(can_draw(",
        IN_EXECUTE,
    );
    // The owner's PDA but another signer's identity: only the identity check.
    c.refuses(
        set("program.public.owner_identity", identity_of(&other)),
        "circuits/draw.rs",
        "require_eq(owner.key().identity()?, public.owner_identity)",
        IN_EXECUTE_EQ,
    );
    // Another owner's PDA: the position is not theirs.
    let op = draw(&other, r.clone(), open(&other, SOL, 7), 100 * USD, 0, P150).unwrap();
    c.refuses(
        under(&op.proof_inputs, "program.public.user_owner"),
        VAULT_RS,
        "require_eq(position.owner(), *user)",
        "assert_position → require_eq",
    );
    c.refuses(
        under(&op.proof_inputs, "program.private.owner_address"),
        "circuits/draw.rs",
        "require_eq(*p.owner_address.owner(), owner)",
        IN_EXECUTE_EQ,
    );
    reserve_refusals(&c, &r, NOW);

    // Past the 95% utilisation cap (95 of 100 is at it).
    let small = reserve(100 * USD);
    let p = draw(&user, small, open(&user, SOL, 7), 95 * USD, 0, P150).unwrap();
    let i = &p.proof_inputs;
    Compiled {
        id: CircuitId::Draw,
        inputs: i,
        execute: Draw::execute,
    }
    .refuses(
        field(
            i,
            "program.private.{borrow,withdraw}",
            0,
            64,
            (96 * USD).into(),
        ),
        "circuits/draw.rs",
        "require(!(borrowing && over_cap))",
        IN_EXECUTE,
    );
}

#[test]
fn compiled_repay_refusals() {
    let user = keypair(7);
    let other = keypair(9);
    let (pos, r) = borrowed(&user, 100 * USD);
    let intent = intent_full(&user, debt(), 10 * USD, purpose::REPAY, 7);
    let p = repay(&user, r.clone(), pos, intent.clone()).unwrap();
    let c = Compiled {
        id: CircuitId::Repay,
        inputs: &p.proof_inputs,
        execute: Repay::execute,
    };
    // Into another position.
    let mut state: IntentState = decode(&intent).unwrap();
    state.target = 8;
    c.refuses(
        intent_leaves(&state),
        "circuits/notes.rs",
        "require_eq(intent.target, target)",
        "assert_intent → require_eq",
    );
    // Made by someone other than the position's owner.
    let mut state: IntentState = decode(&intent).unwrap();
    let theirs: IntentState =
        decode(&intent_full(&other, debt(), 10 * USD, purpose::REPAY, 7)).unwrap();
    state.maker = theirs.maker;
    c.refuses(
        intent_leaves(&state),
        "circuits/repay.rs",
        "require_eq(position.owner, maker)",
        IN_EXECUTE_EQ,
    );
    // An expiry other than the intent's.
    c.refuses(
        field(
            &p.proof_inputs,
            "program.public.{now,expiry}",
            64,
            64,
            (EXPIRY + 1).into(),
        ),
        "circuits/repay.rs",
        "require_eq(intent.expiry, public.expiry)",
        IN_EXECUTE_EQ,
    );
    let (opos, or) = borrowed(&other, 100 * USD);
    let op = repay(
        &other,
        or,
        opos,
        intent_full(&other, debt(), 10 * USD, purpose::REPAY, 7),
    )
    .unwrap();
    c.refuses(
        under(&op.proof_inputs, "program.private.maker_address"),
        "circuits/repay.rs",
        "require_eq(*p.maker_address.owner(), maker)",
        IN_EXECUTE_EQ,
    );
    reserve_refusals(&c, &r, NOW);
}

#[test]
fn compiled_deposit_refusals() {
    let user = keypair(7);
    let other = keypair(9);
    let k = keeper();
    // Open: a position for someone other than the maker.
    let intent = intent_full(&user, coll(), SOL, purpose::DEPOSIT, 7);
    let p = vault::deposit_open(&k, &addr(&user), intent, V, NOW, TREE).unwrap();
    let theirs = intent_full(&other, coll(), SOL, purpose::DEPOSIT, 7);
    let op = vault::deposit_open(&k, &addr(&other), theirs, V, NOW, TREE).unwrap();
    Compiled {
        id: CircuitId::DepositOpen,
        inputs: &p.proof_inputs,
        execute: DepositOpen::execute,
    }
    .refuses(
        under(&op.proof_inputs, "program.public.user_owner"),
        "circuits/deposit.rs",
        "require_eq(intent.owner(), *public_owner.owner())",
        "deposit_intent → require_eq",
    );

    // Add: into another position, or someone else's intent.
    let intent = intent_full(&user, coll(), SOL / 2, purpose::DEPOSIT, 7);
    let p = vault::deposit_add(
        &k,
        &addr(&user),
        open(&user, SOL, 7),
        intent.clone(),
        V,
        NOW,
        TREE,
    )
    .unwrap();
    let c = Compiled {
        id: CircuitId::DepositAdd,
        inputs: &p.proof_inputs,
        execute: DepositAdd::execute,
    };
    let mut state: IntentState = decode(&intent).unwrap();
    state.target = 8;
    c.refuses(
        intent_leaves(&state),
        "circuits/notes.rs",
        "require_eq(intent.target, target)",
        "assert_intent → require_eq",
    );
    let mut state: IntentState = decode(&intent).unwrap();
    let theirs: IntentState =
        decode(&intent_full(&other, coll(), SOL / 2, purpose::DEPOSIT, 7)).unwrap();
    state.maker = theirs.maker;
    c.refuses(
        intent_leaves(&state),
        "circuits/deposit.rs",
        "require_eq(position.owner, intent.maker)",
        IN_EXECUTE_EQ,
    );
}

#[test]
fn compiled_close_refusals() {
    let user = keypair(7);
    let other = keypair(9);
    let position = open(&user, SOL, 7);
    let p = vault::close(&user, position.clone(), V, TREE).unwrap();
    let c = Compiled {
        id: CircuitId::Close,
        inputs: &p.proof_inputs,
        execute: Close::execute,
    };
    let mut state: PositionState = decode(&position).unwrap();
    state.debt_shares = 1;
    c.refuses(
        position_leaves(&state),
        "circuits/close.rs",
        "require_eq(position.debt_shares, 0u64)",
        IN_EXECUTE_EQ,
    );
    c.refuses(
        set("program.public.owner_identity", identity_of(&other)),
        "circuits/close.rs",
        "require_eq(owner.key().identity()?, public.owner_identity)",
        IN_EXECUTE_EQ,
    );
}
