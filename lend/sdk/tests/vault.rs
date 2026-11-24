//! The Vault builders natively (no network, no proofs): every rule the
//! circuits enforce, the outputs' owners and keys, and the keeper reading
//! every output with its transaction viewing key.
mod utxo;

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
    let p = draw(user, reserve(1_000 * USD), open(user, SOL, 7), borrow, 0, P150).unwrap();
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
        (state.owner, state.vault, state.position_id, state.debt_shares),
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
    assert_eq!(read[DEPOSIT_POSITION_SLOT].as_ref().unwrap().amount, 2 * SOL);
}

#[test]
fn deposit_open_refusals() {
    let user = keypair(7);
    let other = keypair(9);
    let k = keeper();
    let good = || intent_full(&user, coll(), SOL, purpose::DEPOSIT, 7);
    // Credited to someone other than the maker.
    assert!(vault::deposit_open(&k, &addr(&other), good(), V, NOW, TREE).is_err());
    // A repay intent, or a deposit in the debt mint (a junk-mint position).
    let repay = intent_full(&user, coll(), SOL, purpose::REPAY, 7);
    assert!(vault::deposit_open(&k, &addr(&user), repay, V, NOW, TREE).is_err());
    let junk = intent_full(&user, debt(), SOL, purpose::DEPOSIT, 7);
    assert!(vault::deposit_open(&k, &addr(&user), junk, V, NOW, TREE).is_err());
    // Below the minimum, after expiry, an unknown vault.
    let small = intent_full(&user, coll(), TSOL_TUSD_MIN_COLLATERAL - 1, purpose::DEPOSIT, 7);
    assert!(vault::deposit_open(&k, &addr(&user), small, V, NOW, TREE).is_err());
    assert!(vault::deposit_open(&k, &addr(&user), good(), V, EXPIRY + 1, TREE).is_err());
    assert!(vault::deposit_open(&k, &addr(&user), good(), 2, NOW, TREE).is_err());
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
    assert!(
        vault::deposit_add(&k, &addr(&user), position.clone(), elsewhere, V, NOW, TREE).is_err()
    );
    // Someone else's intent into this position, either way round.
    let theirs = intent_full(&other, coll(), SOL / 2, purpose::DEPOSIT, 7);
    assert!(vault::deposit_add(
        &k,
        &addr(&user),
        position.clone(),
        theirs.clone(),
        V,
        NOW,
        TREE
    )
    .is_err());
    assert!(vault::deposit_add(&k, &addr(&other), position, theirs, V, NOW, TREE).is_err());
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
    let state: PositionState = decode(&pda_input(&user, &p.outputs[DRAW_POSITION_SLOT], 4)).unwrap();
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
    assert!(draw(&user, reserve(1_000 * USD), open(&user, SOL, 7), 120 * USD, 0, P150).is_ok());
}

#[test]
fn draw_refusals() {
    let user = keypair(7);
    let other = keypair(9);
    let position = || open(&user, SOL, 7);
    let big = || reserve(1_000 * USD);
    // Not the owner: the circuit ties the position to the signer's identity.
    assert!(vault::draw(
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
    )
    .is_err());
    // Past the collateral factor, at a lower bound.
    assert!(draw(&user, big(), position(), 120 * USD + 1, 0, P150).is_err());
    assert!(draw(&user, big(), position(), 100 * USD, 0, 120_000_000_000).is_err());
    // Past the idle liquidity.
    assert!(draw(&user, reserve(50 * USD), position(), 60 * USD, 0, P150).is_err());
    // Past the 95% utilisation cap (at it is fine).
    assert!(draw(&user, reserve(100 * USD), position(), 96 * USD, 0, P150).is_err());
    assert!(draw(&user, reserve(100 * USD), position(), 95 * USD, 0, P150).is_ok());
    // Below the minimum debt.
    assert!(draw(&user, big(), position(), TSOL_TUSD_MIN_DEBT - 1, 0, P150).is_err());
    // Withdrawing past the factor or below the minimum collateral.
    let (pos, r) = borrowed(&user, 100 * USD);
    assert!(draw(&user, r.clone(), pos.clone(), 0, SOL / 5, P150).is_err());
    assert!(draw(&user, r.clone(), pos.clone(), 0, SOL / 100, P150).is_ok());
    let p = draw(&user, big(), position(), 0, SOL - TSOL_TUSD_MIN_COLLATERAL + 1, P150);
    assert!(p.is_err());
    // All of it with no debt is fine, paid to the owner.
    let p = draw(&user, big(), position(), 0, SOL, P150).unwrap();
    assert_eq!(p.outputs[DRAW_COLLATERAL_SLOT].amount, SOL);
    assert_eq!(p.outputs[DRAW_COLLATERAL_SLOT].owner_address, Some(addr(&user)));
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
    let state: PositionState = decode(&pda_input(&user, &p.outputs[DRAW_POSITION_SLOT], 4)).unwrap();
    assert_eq!(
        state.debt_shares,
        core::borrow_shares(10 * USD, expected.borrow_price).unwrap()
    );
    // A reserve updated after `now` is refused.
    let future = reserve_with(1_000 * USD, |s| s.last_update = NOW + 1);
    assert!(draw(&user, future, open(&user, SOL, 7), 10 * USD, 0, P150).is_err());
}

fn repay(
    user: &ShieldedKeypair,
    r: WalletUtxo,
    position: WalletUtxo,
    intent: WalletUtxo,
) -> anyhow::Result<Prepared> {
    vault::repay(&keeper(), &addr(user), r, position, intent, V, NOW, SALT, TREE)
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
    let after: PositionState = decode(&pda_input(&user, &p.outputs[REPAY_POSITION_SLOT], 5)).unwrap();
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
    let after: PositionState = decode(&pda_input(&user, &p.outputs[REPAY_POSITION_SLOT], 5)).unwrap();
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
    assert!(repay(&user, r.clone(), pos.clone(), elsewhere).is_err());
    let deposit = intent_full(&user, debt(), 10 * USD, purpose::DEPOSIT, 7);
    assert!(repay(&user, r.clone(), pos.clone(), deposit).is_err());
    let junk = intent_full(&user, coll(), 10 * USD, purpose::REPAY, 7);
    assert!(repay(&user, r.clone(), pos.clone(), junk).is_err());
    // Someone else's intent into this position.
    let theirs = intent_full(&other, debt(), 10 * USD, purpose::REPAY, 7);
    assert!(repay(&user, r.clone(), pos.clone(), theirs.clone()).is_err());
    assert!(repay(&other, r.clone(), pos.clone(), theirs).is_err());
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
    assert!(late.is_err());
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
fn liquidate_back_to_the_threshold() {
    let user = keypair(7);
    let (pos, r) = borrowed(&user, 120 * USD);
    // $140: value 140, threshold 119 < 120 debt; max limit 126 is not passed.
    let bound = 140_000_000_000;
    let (p, plan) = liquidate(&user, r.clone(), pos.clone(), bound).unwrap();
    assert!(plan.repay > 0 && plan.repay < 120 * USD);
    assert_eq!(plan.buffer, plan.repay / 100);
    let formula =
        ((plan.repay + plan.buffer) as u128 * 1_000_000_000_000 / bound as u128) as u64;
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
    assert_eq!(
        p.outputs[LIQUIDATE_RESERVE_SLOT].amount,
        880 * USD + plan.repay + plan.buffer
    );
    let change = &p.outputs[LIQUIDATE_CHANGE_SLOT];
    assert_eq!(
        (change.amount, change.owner_address),
        (1_000 * USD - plan.repay - plan.buffer, Some(addr(&keeper())))
    );
    // The buffer stays in the reserve without shares.
    let after: ReserveState = decode(&reserve_out(&p, LIQUIDATE_RESERVE_SLOT)).unwrap();
    let left: PositionState =
        decode(&pda_input(&user, &p.outputs[LIQUIDATE_POSITION_SLOT], 6)).unwrap();
    assert_eq!(after.debt_shares, left.debt_shares);
    assert_eq!(after.supply_shares, 1_000 * USD);
    // Less is fine; more takes the position below the threshold.
    assert!(liquidate_with(&user, r.clone(), pos.clone(), plan.repay / 2, bound).is_ok());
    assert!(liquidate_with(&user, r.clone(), pos.clone(), plan.repay + USD, bound).is_err());
    assert!(liquidate_with(&user, r.clone(), pos.clone(), 120 * USD, bound).is_err());
    assert!(liquidate_with(&user, r, pos, 0, bound).is_err());
}

#[test]
fn liquidate_refuses_healthy_and_takes_all_above_the_max_limit() {
    let user = keypair(7);
    let (pos, r) = borrowed(&user, 100 * USD);
    // Healthy at $150 (threshold 127.5) and at exactly the threshold.
    assert!(liquidate_with(&user, r.clone(), pos.clone(), USD, P150).is_err());
    assert!(liquidate(&user, r.clone(), pos.clone(), P150).is_err());
    let (pos, r) = borrowed(&user, 120 * USD);
    // $130: 120 > 0.9 x 130 = 117, past the max limit: all of it may go.
    let bound = 130_000_000_000;
    let (p, plan) = liquidate(&user, r.clone(), pos.clone(), bound).unwrap();
    assert_eq!(plan.repay, 120 * USD);
    let left: PositionState =
        decode(&pda_input(&user, &p.outputs[LIQUIDATE_POSITION_SLOT], 6)).unwrap();
    assert_eq!(left.debt_shares, 0);
    assert!(liquidate_with(&user, r.clone(), pos.clone(), 120 * USD + 1, bound).is_err());
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
    assert!(vault::close(&other, position, V, TREE).is_err());
    let (pos, _) = borrowed(&user, 10 * USD);
    assert!(vault::close(&user, pos, V, TREE).is_err());
}
