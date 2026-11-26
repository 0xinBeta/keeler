//! Earn on a local network: init the reserve, two users supply, time passes
//! with no borrows (prices stay, `last_update` moves), the keeper recovers a
//! receipt from the chain with its writer key, a partial redeem (keeper signs
//! first, the owner co-signs) and a full redeem, with compute units and sizes.
//! The first supply proof is resent with one argument changed (maker, now,
//! expiry, a bump) and refused with the expected error.
//!
//! Run through `lend/scripts/run-chain-test.sh` (`ZOLANA_PORT_OFFSET`, the
//! tool paths from `.tools/env`, the SBF build in `lend/target/deploy`).
//! Ignored in a plain `cargo test`.
#[path = "measure.rs"]
mod measure;
#[path = "tamper.rs"]
mod tamper;
#[path = "../../../.tools/privacy-ring-examples/test-utils/wallet.rs"]
mod wallet;

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{ensure, Context, Result};
use keeler_lend::circuits::{
    REDEEM_PAYOUT_SLOT, REDEEM_RECEIPT_SLOT, REDEEM_RESERVE_SLOT, SUPPLY_RECEIPT_SLOT,
    SUPPLY_RESERVE_SLOT,
};
use keeler_lend::{instructions::supply::SupplyArgs, POOL_SEED, USER_SEED};
use keeler_lend_sdk::{
    builders::{self, recover_output},
    decode, note_data_hash, pool_pda, purpose, user_pda, NoteKind, ReceiptState, ReserveState,
    ADMIN, KEEPERS,
};
use solana_address::Address;
use solana_instruction::Instruction;
use solana_signature::Signature;
use solana_signer::{Signer, SignerError};
use spr_client::{compile_message, sign_transaction, ComputeBudgetConfig, Rpc};
use spr_keypair::{ShieldedAddress, ShieldedKeypair};
use spr_program::ProgramOwner;
use spr_program_test::{
    fixture,
    localnet::{FixtureLocalnet, LocalnetPaths, LocalnetPorts},
    SprProgramTest,
};
use spr_transaction::{AssetRegistry, Mint, SppProofOutputUtxo, WalletUtxo};
use tamper::{canonical, non_canonical, refused_with, tampered, INVALID_PROOF, INVALID_SEEDS};
use wallet::{deposit, send, wait_for_merkle_proof, TestWallet};

const USER_FUNDS: u64 = 10_000_000;
const SUPPLY_A: u64 = 4_000_000;
const SUPPLY_B: u64 = 3_000_000;
const REDEEM_A: u64 = 1_500_000;
const PRICE: u64 = 1_000_000_000_000;
const BUDGET: u32 = 1_400_000;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn err(e: impl core::fmt::Debug) -> anyhow::Error {
    anyhow::anyhow!("{e:?}")
}

fn chain_now(localnet: &FixtureLocalnet) -> Result<u64> {
    let clock: Address = "SysvarC1ock11111111111111111111111111111111"
        .parse()
        .map_err(err)?;
    let account = localnet
        .client
        .rpc()
        .get_account(clock)?
        .context("no clock")?;
    let bytes: [u8; 8] = account.data[32..40].try_into()?;
    Ok(u64::try_from(i64::from_le_bytes(bytes))?)
}

/// `output` as a spendable input of the PDA `owner`, once indexed.
fn indexed(
    localnet: &FixtureLocalnet,
    owner: &ProgramOwner,
    output: &SppProofOutputUtxo,
) -> Result<WalletUtxo> {
    let tree_id = localnet.tree_id;
    let proof = wait_for_merkle_proof(
        localnet.client.indexer(),
        localnet.tree,
        output.hash(tree_id).map_err(err)?,
    )?;
    owner.input(output, tree_id, proof.leaf_index).map_err(err)
}

/// A signature made elsewhere: the keeper's, handed to the owner with the
/// message, as the page receives it.
struct Presigned(Address, Signature);

impl Signer for Presigned {
    fn try_pubkey(&self) -> Result<Address, SignerError> {
        Ok(self.0)
    }
    fn try_sign_message(&self, _: &[u8]) -> Result<Signature, SignerError> {
        Ok(self.1)
    }
    fn is_interactive(&self) -> bool {
        false
    }
}

/// The keeper compiles and signs `ix` (it pays); the owner checks that it is
/// a read-only signer and not the payer, co-signs, and sends.
fn keeper_then_owner(
    rpc: &impl Rpc,
    keeper: &dyn Signer,
    owner: &dyn Signer,
    ix: Instruction,
) -> Result<Signature> {
    let meta = ix
        .accounts
        .iter()
        .find(|a| a.pubkey == owner.pubkey())
        .context("the owner is not in the instruction")?;
    ensure!(meta.is_signer && !meta.is_writable, "owner meta {meta:?}");
    ensure!(
        ix.accounts
            .iter()
            .filter(|a| a.pubkey == owner.pubkey())
            .all(|a| !a.is_writable),
        "the owner is writable somewhere"
    );
    let (blockhash, _) = rpc.get_latest_blockhash()?;
    let message = compile_message(
        &keeper.pubkey(),
        std::slice::from_ref(&ix),
        blockhash,
        ComputeBudgetConfig::new(BUDGET),
    )?;
    let keeper_signature = keeper.sign_message(&message.serialize());
    // The owner's side: only the keeper's signature and the message arrive.
    ensure!(
        message.static_account_keys().first() == Some(&keeper.pubkey()),
        "the keeper must pay"
    );
    let presigned = Presigned(keeper.pubkey(), keeper_signature);
    let tx = sign_transaction(message, &[&presigned, owner])?;
    Ok(rpc.process_transaction(tx)?)
}

/// The receipt in output `slot` of the transaction `signature`, rebuilt from
/// the chain alone with the keeper's writer key.
fn recover_receipt(
    localnet: &FixtureLocalnet,
    keeper: &ShieldedKeypair,
    owner: &ShieldedAddress,
    mint: Mint,
    signature: Signature,
    slot: usize,
) -> Result<(WalletUtxo, ReceiptState)> {
    let found = localnet
        .client
        .indexer()
        .get_shielded_transactions_by_signature(signature, None)?;
    let tx = &found
        .transactions
        .first()
        .context("transaction not indexed")?
        .transaction;
    let first_nullifier = tx.nullifiers.first().context("no nullifiers")?;
    let out_slot = tx.output_slots.get(slot).context("no such slot")?;
    let plain = recover_output(
        keeper,
        first_nullifier,
        &out_slot.payload,
        tx.salt.context("no salt")?,
        slot as u32,
    )?;
    let bytes = plain.data.utxo_data().context("no receipt data")?;
    let user = user_pda(&owner.solana_address().map_err(err)?);
    let output = SppProofOutputUtxo {
        asset: mint,
        amount: plain.amount,
        blinding: plain.blinding,
        owner_address: Some(user.owner.address(owner.viewing_pubkey).map_err(err)?),
        data_hash: Some(note_data_hash(NoteKind::Receipt, bytes, None).map_err(err)?),
        data: plain.data.clone(),
        ..Default::default()
    };
    ensure!(
        output.hash(localnet.tree_id).map_err(err)? == out_slot.output_context.hash,
        "the recovered receipt is not the one on chain"
    );
    let utxo = user
        .owner
        .input(
            &output,
            localnet.tree_id,
            out_slot.output_context.leaf_index,
        )
        .map_err(err)?;
    let state = decode(&utxo)?;
    Ok((utxo, state))
}

fn balance(wallet: &mut TestWallet, localnet: &FixtureLocalnet, mint: Address) -> Result<u64> {
    wallet.rescan(&localnet.client)?;
    Ok(wallet
        .balance(mint)
        .map(|b| b.utxos.iter().map(|u| u.utxo.amount).sum())
        .unwrap_or(0))
}

#[test]
#[ignore = "needs a local network: lend/scripts/run-chain-test.sh"]
fn supply_accrue_and_redeem() -> Result<()> {
    let ports = LocalnetPorts::for_test(0)?;
    let rpc_url = ports.rpc_url();
    let localnet = FixtureLocalnet::start(
        "keeler-lend-earn",
        ports,
        vec![(
            keeler_lend_sdk::ID,
            root().join("target/deploy/keeler_lend.so"),
        )],
        &LocalnetPaths {
            proving_keys_dir: root().join("target/proving-keys"),
            scratch_dir: root().join("target/localnet"),
            ..LocalnetPaths::workspace()
        },
    )?;
    let (tree, tree_id) = (localnet.tree, localnet.tree_id);
    let mint = fixture::spl_mint();
    let asset = Mint::new(mint, fixture::SPL_ASSET_ID);
    let keeper_key = fixture::actor(0);
    let a_key = fixture::actor(1);
    let b_key = fixture::actor(2);
    let admin_key = fixture::actor(3);
    ensure!(
        keeper_key.pubkey() == KEEPERS[0] && admin_key.pubkey() == ADMIN,
        "build with the default keeper and admin"
    );
    let keeper = keeler_lend_sdk::fixture_keeper_keypair(&keeper_key)?;
    let a = ShieldedKeypair::from_keypair(&a_key)?;
    let b = ShieldedKeypair::from_keypair(&b_key)?;
    let indexer = localnet.client.indexer();
    let rpc = localnet.client.rpc();
    let pool = pool_pda(&mint).owner;

    // 1. The reserve.
    let init = builders::init_reserve(&keeper, ADMIN, asset, chain_now(&localnet)?, 1, tree_id)?;
    let reserve_out = init
        .outputs
        .iter()
        .find(|o| o.data_hash.is_some())
        .cloned()
        .context("no reserve output")?;
    measure::lend_proof("init_reserve", &init)?;
    let ix = init.prove(indexer, tree, &keeper)?;
    let sig = send(rpc, &[&keeper_key, &admin_key], ix.clone())?;
    let mut reserve = indexed(&localnet, &pool, &reserve_out)?;
    measure::report(&rpc_url, "init_reserve", &keeper_key.pubkey(), &ix, &sig)?;
    println!("reserve created");

    // 2. Both users shield funds and make supply intents.
    let mut assets = AssetRegistry::default();
    assets.insert(fixture::SPL_ASSET_ID, mint)?;
    let mut wallets = Vec::new();
    let mut intents = Vec::new();
    for (user, key, amount) in [(&a, &a_key, SUPPLY_A), (&b, &b_key, SUPPLY_B)] {
        deposit(
            &localnet,
            &fixture::payer(),
            SprProgramTest::wallet_spl_shield_data(
                USER_FUNDS,
                &user.shielded_address()?,
                &mint,
                &fixture::payer_token_account(),
            )?,
        )?;
        let wallet = TestWallet::scan(user.clone(), assets.clone(), &localnet.client)?;
        let source = wallet
            .spendable_utxo(mint, amount)
            .context("no user note")?;
        let expiry = chain_now(&localnet)? + 600;
        let intent = builders::intent(
            user,
            source,
            &mint,
            amount,
            purpose::SUPPLY,
            keeler_lend::pool::TUSD,
            expiry,
            keeper.viewing_pubkey(),
            tree_id,
        )?;
        let out = intent.outputs[keeler_lend::circuits::INTENT_SLOT].clone();
        measure::lend_proof("intent", &intent)?;
        let ix = intent.prove(indexer, tree, user)?;
        let sig = send(rpc, &[key], ix.clone())?;
        intents.push(indexed(&localnet, &user_pda(&key.pubkey()).owner, &out)?);
        measure::report(&rpc_url, "intent", &key.pubkey(), &ix, &sig)?;
        wallets.push(wallet);
    }
    println!("intents made");

    // 3. The keeper supplies both.
    let mut supply_sigs = Vec::new();
    let mut receipt_outs = Vec::new();
    for (i, (user, intent, amount)) in [(&a, &intents[0], SUPPLY_A), (&b, &intents[1], SUPPLY_B)]
        .into_iter()
        .enumerate()
    {
        let now = chain_now(&localnet)?;
        let p = builders::supply(
            &keeper,
            &user.shielded_address()?,
            reserve.clone(),
            intent.clone(),
            now,
            u128::from(now) + 10,
            tree_id,
        )?;
        let (res_out, rec_out) = (
            p.outputs[SUPPLY_RESERVE_SLOT].clone(),
            p.outputs[SUPPLY_RECEIPT_SLOT].clone(),
        );
        measure::lend_proof("supply", &p)?;
        let ix = p.prove(indexer, tree, &keeper)?;
        if i == 0 {
            // The proof resent with one argument changed is refused.
            let b_wallet = b_key.pubkey().to_bytes();
            type Edit = Box<dyn FnOnce(&mut SupplyArgs)>;
            let cases: [(&str, Edit, &str); 4] = [
                (
                    "supply for another maker",
                    Box::new(move |a| {
                        a.maker = b_wallet;
                        a.user_bump = canonical(USER_SEED, &b_wallet);
                    }),
                    INVALID_PROOF,
                ),
                (
                    "supply at another now",
                    Box::new(|a| a.now -= 1),
                    INVALID_PROOF,
                ),
                (
                    "supply with another expiry",
                    Box::new(|a| a.expiry += 1),
                    INVALID_PROOF,
                ),
                (
                    "supply with a non-canonical pool bump",
                    Box::new(move |a| a.pool_bump = non_canonical(POOL_SEED, mint.as_array())),
                    INVALID_SEEDS,
                ),
            ];
            for (what, edit, code) in cases {
                refused_with(what, send(rpc, &[&keeper_key], tampered(&ix, edit)), code)?;
            }
        }
        let sig = send(rpc, &[&keeper_key], ix.clone())?;
        measure::report(&rpc_url, "supply", &keeper_key.pubkey(), &ix, &sig)?;
        reserve = indexed(&localnet, &pool, &res_out)?;
        let state: ReserveState = decode(&reserve)?;
        ensure!(state.last_update == now, "last update {state:?}");
        let receipt = indexed(
            &localnet,
            &user_pda(&user.shielded_address()?.solana_address()?).owner,
            &rec_out,
        )?;
        let rs: ReceiptState = decode(&receipt)?;
        ensure!(rs.supply_shares == amount, "receipt {rs:?}");
        supply_sigs.push(sig);
        receipt_outs.push(receipt);
    }
    let state: ReserveState = decode(&reserve)?;
    ensure!(
        state.supply_shares == SUPPLY_A + SUPPLY_B && reserve.utxo.amount == SUPPLY_A + SUPPLY_B,
        "reserve after both supplies {state:?}"
    );
    println!("both supplied: {state:?}");

    // 4. The keeper recovers A's receipt from the chain with its writer key.
    let (receipt_a, recovered) = recover_receipt(
        &localnet,
        &keeper,
        &a.shielded_address()?,
        asset,
        supply_sigs[0],
        SUPPLY_RECEIPT_SLOT,
    )?;
    ensure!(
        receipt_a.utxo_hash == receipt_outs[0].utxo_hash && recovered.supply_shares == SUPPLY_A,
        "recovered {recovered:?}"
    );
    println!("keeper recovered A's receipt through the writer key");

    // 5. Time passes with no borrows: prices stay at 1e12, the update moves.
    std::thread::sleep(Duration::from_secs(3));
    let before = state.last_update;

    // 6. A redeems part, from the recovered receipt; the owner co-signs.
    let now = chain_now(&localnet)?;
    ensure!(now > before, "the clock did not move");
    let p = builders::redeem(
        &keeper,
        &a.shielded_address()?,
        reserve.clone(),
        receipt_a,
        REDEEM_A,
        now,
        u128::from(now) + 20,
        tree_id,
    )?;
    let (res_out, rec_out, pay_out) = (
        p.outputs[REDEEM_RESERVE_SLOT].clone(),
        p.outputs[REDEEM_RECEIPT_SLOT].clone(),
        p.outputs[REDEEM_PAYOUT_SLOT].clone(),
    );
    measure::lend_proof("redeem (partial)", &p)?;
    let ix = p.prove(indexer, tree, &keeper)?;
    let sig = keeper_then_owner(rpc, &keeper_key, &a_key, ix.clone())?;
    localnet
        .client
        .confirm_private_transaction_sync(sig)
        .map_err(err)?;
    measure::report(
        &rpc_url,
        "redeem (partial)",
        &keeper_key.pubkey(),
        &ix,
        &sig,
    )?;
    reserve = indexed(&localnet, &pool, &res_out)?;
    let state: ReserveState = decode(&reserve)?;
    ensure!(
        state.last_update == now && state.supply_price == PRICE && state.borrow_price == PRICE,
        "borrow-free accrual {state:?}"
    );
    let left: ReceiptState = decode(&indexed(
        &localnet,
        &user_pda(&a_key.pubkey()).owner,
        &rec_out,
    )?)?;
    ensure!(left.supply_shares == SUPPLY_A - REDEEM_A, "left {left:?}");
    wait_for_merkle_proof(indexer, tree, pay_out.hash(tree_id).map_err(err)?)?;
    let got = balance(&mut wallets[0], &localnet, mint)?;
    ensure!(got == USER_FUNDS - SUPPLY_A + REDEEM_A, "A's balance {got}");
    println!("A redeemed {REDEEM_A}: balance {got}, receipt {left:?}");

    // 7. B redeems everything; the zero-share receipt keeps the output count.
    let now = chain_now(&localnet)?;
    let p = builders::redeem(
        &keeper,
        &b.shielded_address()?,
        reserve.clone(),
        receipt_outs[1].clone(),
        SUPPLY_B,
        now,
        u128::from(now) + 30,
        tree_id,
    )?;
    let (res_out, rec_out) = (
        p.outputs[REDEEM_RESERVE_SLOT].clone(),
        p.outputs[REDEEM_RECEIPT_SLOT].clone(),
    );
    measure::lend_proof("redeem (full)", &p)?;
    let ix = p.prove(indexer, tree, &keeper)?;
    let sig = keeper_then_owner(rpc, &keeper_key, &b_key, ix.clone())?;
    localnet
        .client
        .confirm_private_transaction_sync(sig)
        .map_err(err)?;
    measure::report(&rpc_url, "redeem (full)", &keeper_key.pubkey(), &ix, &sig)?;
    reserve = indexed(&localnet, &pool, &res_out)?;
    let state: ReserveState = decode(&reserve)?;
    let empty: ReceiptState = decode(&indexed(
        &localnet,
        &user_pda(&b_key.pubkey()).owner,
        &rec_out,
    )?)?;
    ensure!(empty.supply_shares == 0, "B's receipt {empty:?}");
    ensure!(
        state.supply_shares == SUPPLY_A - REDEEM_A && reserve.utxo.amount == SUPPLY_A - REDEEM_A,
        "reserve {state:?}"
    );
    let got = balance(&mut wallets[1], &localnet, mint)?;
    ensure!(got == USER_FUNDS, "B's balance {got}");
    println!("B redeemed all: balance {got}");
    Ok(())
}
