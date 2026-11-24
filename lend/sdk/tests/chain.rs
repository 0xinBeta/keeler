//! On a local network: init the reserve (a second init is refused), make an
//! intent, cancel it after its expiry (refused before it, and resent with
//! another expiry or bump), and find the refund indexed.
//!
//! Run through `lend/scripts/run-chain-test.sh` (port offset 2100, the tool
//! paths from `.tools/env`, the SBF build in `lend/target/deploy`). Ignored in
//! a plain `cargo test`.
#[path = "tamper.rs"]
mod tamper;
#[path = "wallet.rs"]
mod wallet;

use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};
use keeler_lend::{instructions::cancel_intent::CancelIntentArgs, USER_SEED};
use keeler_lend_sdk::{
    builders, decode, pool_pda, purpose, user_pda, IntentState, ReserveState, ADMIN, KEEPER,
};
use solana_address::Address;
use solana_signer::Signer;
use spr_client::Rpc;
use spr_keypair::ShieldedKeypair;
use spr_program::ProgramOwner;
use spr_program_test::{
    fixture,
    localnet::{FixtureLocalnet, LocalnetPaths, LocalnetPorts},
    SprProgramTest,
};
use spr_transaction::{AssetRegistry, Mint, SppProofOutputUtxo, WalletUtxo};
use tamper::{non_canonical, refused_with, tampered, INVALID_PROOF, INVALID_SEEDS, NOT_EXPIRED};
use wallet::{deposit, send, wait_for_merkle_proof, TestWallet};

const USER_FUNDS: u64 = 1_000_000;
/// Long enough to prove the cancel before the intent expires.
const EXPIRES_IN: u64 = 45;
const INTENT_AMOUNT: u64 = 250_000;

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

#[test]
#[ignore = "needs a local network: lend/scripts/run-chain-test.sh"]
fn reserve_intent_and_cancel() -> Result<()> {
    let localnet = FixtureLocalnet::start(
        "keeler-lend",
        LocalnetPorts::for_test(0)?,
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
    let user_key = fixture::actor(1);
    let admin_key = fixture::actor(3);
    ensure!(
        keeper_key.pubkey() == KEEPER && admin_key.pubkey() == ADMIN,
        "build with the default keeper and admin"
    );
    let keeper = ShieldedKeypair::from_keypair(&keeper_key)?;
    let user = ShieldedKeypair::from_keypair(&user_key)?;
    let indexer = localnet.client.indexer();
    let rpc = localnet.client.rpc();

    // 1. The reserve, once.
    let now = chain_now(&localnet)?;
    let first = builders::init_reserve(&keeper, ADMIN, asset, now, 1, tree_id)?;
    // Built before the first lands, so it proves the same fresh address.
    let second = builders::init_reserve(&keeper, ADMIN, asset, now, 2, tree_id)?;
    let reserve_out = first
        .outputs
        .iter()
        .find(|o| o.data_hash.is_some())
        .cloned()
        .context("no reserve output")?;
    let ix = first.prove(indexer, tree, &keeper)?;
    let sig = send(rpc, &[&keeper_key, &admin_key], ix)?;
    println!("init_reserve: {sig}");
    let refused = send(
        rpc,
        &[&keeper_key, &admin_key],
        second.prove(indexer, tree, &keeper)?,
    );
    let refused = refused
        .err()
        .context("a second reserve for the same mint was accepted")?
        .to_string();
    println!("second init_reserve refused: {refused}");
    ensure!(
        refused.contains("0x1b83"),
        "expected NullifierAlreadyQueued (0x1b83): {refused}"
    );
    let reserve: ReserveState = decode(&indexed(&localnet, &pool_pda(&mint).owner, &reserve_out)?)?;
    ensure!(
        reserve.last_update == now && reserve.salt == 1,
        "reserve state {reserve:?}"
    );

    // 2. An intent that expires shortly.
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
    let mut assets = AssetRegistry::default();
    assets.insert(fixture::SPL_ASSET_ID, mint)?;
    let wallet = TestWallet::scan(user.clone(), assets, &localnet.client)?;
    let source = wallet
        .spendable_utxo(mint, INTENT_AMOUNT)
        .context("no user note")?;
    let expiry = chain_now(&localnet)? + EXPIRES_IN;
    let viewing = keeper.viewing_pubkey();
    let intent = builders::intent(
        &user,
        source,
        &mint,
        INTENT_AMOUNT,
        purpose::SUPPLY,
        keeler_lend::pool::TUSD,
        expiry,
        viewing,
        tree_id,
    )?;
    let intent_out = intent.outputs[keeler_lend::circuits::INTENT_SLOT].clone();
    let sig = send(rpc, &[&user_key], intent.prove(indexer, tree, &user)?)?;
    println!("intent: {sig}");
    let pda = user_pda(&user_key.pubkey());
    let intent_utxo = indexed(&localnet, &pda.owner, &intent_out)?;
    let state: IntentState = decode(&intent_utxo)?;
    ensure!(
        state.expiry == expiry && state.purpose == purpose::SUPPLY,
        "intent state {state:?}"
    );

    // 3. Cancel it: refused before the expiry, and with the proof resent
    //    under another expiry or a non-canonical bump; then, once expired, it
    //    lands and the refund is the maker's, indexed.
    let cancel = builders::cancel_intent(&user, intent_utxo, viewing, tree_id)?;
    let refund = cancel.outputs[keeler_lend::circuits::CANCEL_REFUND_SLOT].clone();
    ensure!(
        refund.amount == INTENT_AMOUNT && refund.owner_address == Some(user.shielded_address()?),
        "refund {refund:?}"
    );
    let ix = cancel.prove(indexer, tree, &user)?;
    ensure!(
        chain_now(&localnet)? <= expiry,
        "proving outlasted the expiry"
    );
    refused_with(
        "cancel before expiry",
        send(rpc, &[&user_key], ix.clone()),
        NOT_EXPIRED,
    )?;
    refused_with(
        "cancel resent with an earlier expiry",
        send(
            rpc,
            &[&user_key],
            tampered(&ix, |a: &mut CancelIntentArgs| a.expiry = expiry - 3600),
        ),
        INVALID_PROOF,
    )?;
    while chain_now(&localnet)? <= expiry {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    refused_with(
        "cancel resent with a non-canonical user bump",
        send(
            rpc,
            &[&user_key],
            tampered(&ix, |a: &mut CancelIntentArgs| {
                a.user_bump = non_canonical(USER_SEED, user_key.pubkey().as_array())
            }),
        ),
        INVALID_SEEDS,
    )?;
    let sig = send(rpc, &[&user_key], ix)?;
    println!("cancel_intent: {sig}");
    wait_for_merkle_proof(indexer, tree, refund.hash(tree_id).map_err(err)?)?;
    println!("refund indexed");
    Ok(())
}
