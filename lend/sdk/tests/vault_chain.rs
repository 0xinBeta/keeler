//! The Vault on a local network, end to end: a lender supplies tUSD; a
//! borrower opens a tSOL position and adds to it (the keeper recovers the
//! position from the chain with its writer key), borrows (the owner
//! co-signs), interest accrues, repays part; the price drops and the keeper
//! liquidates back to the threshold, paying in exactly the debt it repays; a
//! second drop takes it past the max limit and the second keeper of the set
//! (same pool key, its own funds) liquidates all of the debt from the
//! reserve the first wrote; the borrower closes it.
//! A second position borrows, repays all with change, and closes. Prints
//! compute units and sizes. Draw, repay and liquidate proofs are first
//! resent with one argument changed (bound, vault, owner, now, expiry, a
//! bump) and refused with the expected error.
//!
//! Run through `lend/scripts/run-chain-test.sh` with
//! `LEND_CHAIN_TESTS=vault_chain` (`ZOLANA_PORT_OFFSET`, the tool paths from
//! `.tools/env`, the SBF build in `lend/target/deploy`). Ignored in a plain
//! `cargo test`.
#[path = "measure.rs"]
mod measure;
#[path = "tamper.rs"]
mod tamper;
#[path = "wallet.rs"]
mod wallet;

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{ensure, Context, Result};
use keeler_core::lend as core;
use keeler_lend::{
    circuits::{
        CLOSE_PAYOUT_SLOT, DEPOSIT_POSITION_SLOT, DRAW_DEBT_SLOT, DRAW_POSITION_SLOT,
        DRAW_RESERVE_SLOT, INTENT_SLOT, LIQUIDATE_CHANGE_SLOT, LIQUIDATE_PAYOUT_SLOT,
        LIQUIDATE_POSITION_SLOT, LIQUIDATE_RESERVE_SLOT, REPAY_CHANGE_SLOT, REPAY_POSITION_SLOT,
        REPAY_RESERVE_SLOT, SUPPLY_RESERVE_SLOT,
    },
    instructions::{draw::DrawArgs, liquidate::LiquidateArgs, repay::RepayArgs},
    vaults::TSOL_TUSD,
    USER_SEED,
};
use keeler_lend_sdk::{
    builders::{self, recover_output, Prepared},
    decode, note_data_hash, pool_pda, purpose, user_pda,
    vault::{self, test_price_account},
    NoteKind, PositionState, ReserveState, ADMIN, KEEPERS,
};
use solana_address::Address;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_signature::Signature;
use solana_signer::{Signer, SignerError};
use spr_client::{compile_message, sign_transaction, ComputeBudgetConfig, Rpc};
use spr_keypair::ShieldedKeypair;
use spr_program::ProgramOwner;
use spr_program_test::{
    fixture,
    localnet::{FixtureLocalnet, LocalnetPaths, LocalnetPorts},
    SprProgramTest,
};
use spr_transaction::{AssetRegistry, Mint, SppProofOutputUtxo, WalletUtxo};
use tamper::{
    canonical, non_canonical, refused_with, tampered, INVALID_PROOF, INVALID_SEEDS, UNKNOWN_VAULT,
};
use wallet::{deposit, send, wait_for_merkle_proof, TestWallet};

const BUDGET: u32 = 1_400_000;
const SOL: u64 = 1_000_000_000;
const USD: u64 = 1_000_000;
const V: u64 = 1;
/// Test prices: tUSD base units per tSOL base unit x 1e12 ($1 = 1e9).
const DOLLAR: u64 = 1_000_000_000;
const CONF: u64 = 10_000_000;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn err(e: impl std::fmt::Debug) -> anyhow::Error {
    anyhow::anyhow!("{e:?}")
}

struct Env {
    localnet: FixtureLocalnet,
    rpc_url: String,
    keeper_key: Keypair,
    keeper: ShieldedKeypair,
}

impl Env {
    fn now(&self) -> Result<u64> {
        let clock: Address = "SysvarC1ock11111111111111111111111111111111"
            .parse()
            .map_err(err)?;
        let account = self
            .localnet
            .client
            .rpc()
            .get_account(clock)?
            .context("no clock")?;
        let bytes: [u8; 8] = account.data[32..40].try_into()?;
        Ok(u64::try_from(i64::from_le_bytes(bytes))?)
    }

    fn tree(&self) -> (Address, u16) {
        (self.localnet.tree, self.localnet.tree_id)
    }

    /// `output` as an input of the PDA `owner`, once indexed.
    fn indexed(&self, owner: &ProgramOwner, output: &SppProofOutputUtxo) -> Result<WalletUtxo> {
        let (tree, tree_id) = self.tree();
        let proof = wait_for_merkle_proof(
            self.localnet.client.indexer(),
            tree,
            output.hash(tree_id).map_err(err)?,
        )?;
        owner.input(output, tree_id, proof.leaf_index).map_err(err)
    }

    fn wait(&self, output: &SppProofOutputUtxo) -> Result<()> {
        let (tree, tree_id) = self.tree();
        wait_for_merkle_proof(
            self.localnet.client.indexer(),
            tree,
            output.hash(tree_id).map_err(err)?,
        )?;
        Ok(())
    }

    fn report(&self, name: &str, payer: &Address, ix: &Instruction, sig: &Signature) -> Result<()> {
        measure::report(&self.rpc_url, name, payer, ix, sig)
    }

    /// Proves and sends a keeper-built transaction; the keeper alone signs.
    fn keeper_sends(&self, name: &str, p: Prepared) -> Result<Signature> {
        self.keeper_sends_after(name, p, |_| Ok(()))
    }

    /// [`Self::keeper_sends`], with `first` run on the proved instruction
    /// before it is sent.
    fn keeper_sends_after(
        &self,
        name: &str,
        p: Prepared,
        first: impl FnOnce(&Instruction) -> Result<()>,
    ) -> Result<Signature> {
        self.sends_as(&self.keeper_key, &self.keeper, name, p, first)
    }

    /// [`Self::keeper_sends_after`] by any keeper of the set.
    fn sends_as(
        &self,
        key: &Keypair,
        keeper: &ShieldedKeypair,
        name: &str,
        p: Prepared,
        first: impl FnOnce(&Instruction) -> Result<()>,
    ) -> Result<Signature> {
        let (tree, _) = self.tree();
        measure::lend_proof(name, &p)?;
        let ix = p.prove(self.localnet.client.indexer(), tree, keeper)?;
        first(&ix)?;
        let sig = send(self.localnet.client.rpc(), &[key], ix.clone())?;
        self.confirm(sig)?;
        self.report(name, &key.pubkey(), &ix, &sig)?;
        Ok(sig)
    }

    fn confirm(&self, sig: Signature) -> Result<()> {
        self.localnet
            .client
            .confirm_private_transaction_sync(sig)
            .map_err(err)
    }

    fn set_price(&self, price: u64) -> Result<()> {
        let ix = vault::set_test_price(
            self.keeper_key.pubkey(),
            V,
            price,
            CONF,
            i64::try_from(self.now()?)?,
        )?;
        send(self.localnet.client.rpc(), &[&self.keeper_key], ix)?;
        Ok(())
    }
}

/// A signature made elsewhere: the keeper's, handed to the owner with the
/// message.
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

/// The keeper compiles and signs `ix` (it pays); the owner checks it is a
/// read-only signer and not the payer, co-signs, and sends.
fn keeper_then_owner(
    rpc: &impl Rpc,
    keeper: &dyn Signer,
    owner: &dyn Signer,
    ix: Instruction,
) -> Result<Signature> {
    ensure!(
        ix.accounts
            .iter()
            .filter(|a| a.pubkey == owner.pubkey())
            .all(|a| a.is_signer && !a.is_writable),
        "the owner must be a read-only signer"
    );
    let (blockhash, _) = rpc.get_latest_blockhash()?;
    let message = compile_message(
        &keeper.pubkey(),
        std::slice::from_ref(&ix),
        blockhash,
        ComputeBudgetConfig::new(BUDGET),
    )?;
    let keeper_signature = keeper.sign_message(&message.serialize());
    ensure!(
        message.static_account_keys().first() == Some(&keeper.pubkey()),
        "the keeper must pay"
    );
    let presigned = Presigned(keeper.pubkey(), keeper_signature);
    let tx = sign_transaction(message, &[&presigned, owner])?;
    Ok(rpc.process_transaction(tx)?)
}

fn balance(wallet: &mut TestWallet, env: &Env, mint: Address) -> Result<u64> {
    wallet.rescan(&env.localnet.client)?;
    Ok(wallet
        .balance(mint)
        .map(|b| b.utxos.iter().map(|u| u.utxo.amount).sum())
        .unwrap_or(0))
}

/// `user` moves `amount` of `mint` into an intent for `purpose` / `target`.
fn intent(
    env: &Env,
    user_key: &Keypair,
    wallet: &mut TestWallet,
    mint: &Address,
    amount: u64,
    purpose: u8,
    target: u64,
) -> Result<WalletUtxo> {
    wallet.rescan(&env.localnet.client)?;
    let source = wallet
        .spendable_utxo(*mint, amount)
        .with_context(|| format!("no note of {amount}"))?;
    let (tree, tree_id) = env.tree();
    let p = builders::intent(
        &wallet.keypair,
        source,
        mint,
        amount,
        purpose,
        target,
        env.now()? + 600,
        env.keeper.viewing_pubkey(),
        tree_id,
    )?;
    let out = p.outputs[INTENT_SLOT].clone();
    let ix = p.prove(env.localnet.client.indexer(), tree, &wallet.keypair)?;
    let sig = send(env.localnet.client.rpc(), &[user_key], ix)?;
    env.confirm(sig)?;
    env.indexed(&user_pda(&user_key.pubkey()).owner, &out)
}

fn debt(position: &WalletUtxo, reserve: &WalletUtxo) -> Result<u64> {
    let p: PositionState = decode(position)?;
    let r: ReserveState = decode(reserve)?;
    core::debt_of(p.debt_shares, r.borrow_price).map_err(err)
}

#[test]
#[ignore = "needs a local network: lend/scripts/run-chain-test.sh"]
fn borrow_accrue_repay_liquidate_close() -> Result<()> {
    let ports = LocalnetPorts::for_test(0)?;
    let rpc_url = ports.rpc_url();
    let localnet = FixtureLocalnet::start(
        "keeler-lend-vault",
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
    let tusd = fixture::spl_mint();
    ensure!(
        TSOL_TUSD.debt_mint == tusd && TSOL_TUSD.collateral_mint == Address::default(),
        "build with the default vault mints (SOL and the fixture mint)"
    );
    let debt_asset = Mint::new(tusd, fixture::SPL_ASSET_ID);
    let keeper_key = fixture::actor(0);
    let borrower_key = fixture::actor(1);
    let lender_key = fixture::actor(2);
    let admin_key = fixture::actor(3);
    ensure!(
        keeper_key.pubkey() == KEEPERS[0] && admin_key.pubkey() == ADMIN,
        "build with the default keeper and admin"
    );
    let env = Env {
        keeper: keeler_lend_sdk::fixture_keeper_keypair(&keeper_key)?,
        keeper_key,
        localnet,
        rpc_url,
    };
    let keeper = &env.keeper;
    let borrower = ShieldedKeypair::from_keypair(&borrower_key)?;
    let lender = ShieldedKeypair::from_keypair(&lender_key)?;
    let owner = borrower.shielded_address()?;
    let user = user_pda(&borrower_key.pubkey()).owner;
    let pool = pool_pda(&tusd).owner;
    let (tree, tree_id) = env.tree();
    let indexer = env.localnet.client.indexer();
    let rpc = env.localnet.client.rpc();
    let mut assets = AssetRegistry::default();
    assets.insert(fixture::SPL_ASSET_ID, tusd)?;

    // 1. The reserve, and 20 tUSD of liquidity from the lender.
    let init = builders::init_reserve(keeper, ADMIN, debt_asset, env.now()?, 1, tree_id)?;
    let out = init
        .outputs
        .iter()
        .find(|o| o.data_hash.is_some())
        .cloned()
        .context("no reserve output")?;
    send(
        rpc,
        &[&env.keeper_key, &admin_key],
        init.prove(indexer, tree, keeper)?,
    )?;
    let mut reserve = env.indexed(&pool, &out)?;
    let fund = |key: &Keypair, to: &ShieldedKeypair, sol: u64, usd: u64| -> Result<()> {
        if sol > 0 {
            deposit(
                &env.localnet,
                &fixture::payer(),
                SprProgramTest::wallet_sol_shield_data(sol, &to.shielded_address()?)?,
            )?;
        }
        if usd > 0 {
            deposit(
                &env.localnet,
                &fixture::payer(),
                SprProgramTest::wallet_spl_shield_data(
                    usd,
                    &to.shielded_address()?,
                    &tusd,
                    &fixture::payer_token_account(),
                )?,
            )?;
        }
        let _ = key;
        Ok(())
    };
    fund(&lender_key, &lender, 0, 20 * USD)?;
    fund(&borrower_key, &borrower, 3 * SOL, 0)?;
    fund(&env.keeper_key, keeper, 0, 50 * USD)?;
    // The second keeper of the set, under the same pool key: it liquidates
    // in step 7, spending the reserve the first keeper wrote.
    let second_key = fixture::payer();
    ensure!(
        KEEPERS[1] == second_key.pubkey(),
        "build with the default keepers"
    );
    let second = keeler_lend_sdk::fixture_keeper_keypair(&second_key)?;
    fund(&second_key, &second, 0, 50 * USD)?;
    let mut lender_wallet = TestWallet::scan(lender.clone(), assets.clone(), &env.localnet.client)?;
    let supply_intent = intent(
        &env,
        &lender_key,
        &mut lender_wallet,
        &tusd,
        20 * USD,
        purpose::SUPPLY,
        keeler_lend::pool::TUSD,
    )?;
    let now = env.now()?;
    let p = builders::supply(
        keeper,
        &lender.shielded_address()?,
        reserve.clone(),
        supply_intent,
        now,
        u128::from(now),
        tree_id,
    )?;
    let out = p.outputs[SUPPLY_RESERVE_SLOT].clone();
    env.keeper_sends("supply", p)?;
    reserve = env.indexed(&pool, &out)?;
    ensure!(
        reserve.utxo.amount == 20 * USD,
        "liquidity {}",
        reserve.utxo.amount
    );
    println!("reserve with 20 tUSD");

    // 2. Open position 1 with 1 tSOL; the keeper recovers it from the chain.
    let mut wallet = TestWallet::scan(borrower.clone(), assets.clone(), &env.localnet.client)?;
    let sol = Address::default();
    let i = intent(
        &env,
        &borrower_key,
        &mut wallet,
        &sol,
        SOL,
        purpose::DEPOSIT,
        1,
    )?;
    let p = vault::deposit_open(keeper, &owner, i, V, env.now()?, tree_id)?;
    let out = p.outputs[DEPOSIT_POSITION_SLOT].clone();
    let sig = env.keeper_sends("deposit_open", p)?;
    let mut position = env.indexed(&user, &out)?;
    let found = indexer.get_shielded_transactions_by_signature(sig, None)?;
    let tx = &found
        .transactions
        .first()
        .context("deposit not indexed")?
        .transaction;
    let slot = tx
        .output_slots
        .get(DEPOSIT_POSITION_SLOT)
        .context("no position slot")?;
    let plain = recover_output(
        keeper,
        tx.nullifiers.first().context("no nullifier")?,
        &slot.payload,
        tx.salt.context("no salt")?,
        DEPOSIT_POSITION_SLOT as u32,
    )?;
    let bytes = plain.data.utxo_data().context("no position data")?;
    ensure!(
        plain.amount == SOL
            && Some(note_data_hash(NoteKind::Position, bytes, None).map_err(err)?) == out.data_hash,
        "the recovered position is not the one on chain"
    );
    println!("position 1 opened; keeper recovered it with its writer key");

    // 3. Add 0.5 tSOL.
    let i = intent(
        &env,
        &borrower_key,
        &mut wallet,
        &sol,
        SOL / 2,
        purpose::DEPOSIT,
        1,
    )?;
    let p = vault::deposit_add(keeper, &owner, position, i, V, env.now()?, tree_id)?;
    let out = p.outputs[DEPOSIT_POSITION_SLOT].clone();
    env.keeper_sends("deposit_add", p)?;
    position = env.indexed(&user, &out)?;
    ensure!(
        position.utxo.amount == 3 * SOL / 2,
        "position {}",
        position.utxo.amount
    );

    // 4. Borrow 11 tUSD at $10 (cap 0.8 x 1.5 x 9.99 = 11.988); the owner co-signs.
    env.set_price(10 * DOLLAR)?;
    let price = test_price_account(V);
    let draw = |reserve: WalletUtxo,
                position: WalletUtxo,
                borrow: u64,
                bound: u64|
     -> Result<(WalletUtxo, WalletUtxo, SppProofOutputUtxo)> {
        let now = env.now()?;
        let p = vault::draw(
            keeper,
            &owner,
            reserve,
            position,
            borrow,
            0,
            price,
            V,
            now,
            bound,
            u128::from(now) + 2,
            tree_id,
        )?;
        let (pos, res, pay) = (
            p.outputs[DRAW_POSITION_SLOT].clone(),
            p.outputs[DRAW_RESERVE_SLOT].clone(),
            p.outputs[DRAW_DEBT_SLOT].clone(),
        );
        measure::lend_proof("draw (borrow)", &p)?;
        let ix = p.prove(indexer, tree, keeper)?;
        // The proof resent with one argument changed is refused.
        let resend = |edit: fn(&mut DrawArgs)| {
            keeper_then_owner(rpc, &env.keeper_key, &borrower_key, tampered(&ix, edit))
        };
        refused_with(
            "draw at a lower bound",
            resend(|a| a.bound -= 1),
            INVALID_PROOF,
        )?;
        refused_with("draw on vault 2", resend(|a| a.vault = 2), UNKNOWN_VAULT)?;
        let sig = keeper_then_owner(rpc, &env.keeper_key, &borrower_key, ix.clone())?;
        env.confirm(sig)?;
        env.report("draw (borrow)", &env.keeper_key.pubkey(), &ix, &sig)?;
        Ok((env.indexed(&user, &pos)?, env.indexed(&pool, &res)?, pay))
    };
    let (pos, res, pay) = draw(reserve, position, 11 * USD, 10 * DOLLAR - CONF)?;
    (position, reserve) = (pos, res);
    env.wait(&pay)?;
    ensure!(balance(&mut wallet, &env, tusd)? == 11 * USD, "borrowed");
    let rate = decode::<ReserveState>(&reserve)?.rate_bps;
    ensure!(rate > 0, "a borrow sets a rate");
    println!("borrowed 11 tUSD, rate {rate} bps");

    // 5. Interest accrues; repay 3 tUSD.
    std::thread::sleep(Duration::from_secs(4));
    let i = intent(
        &env,
        &borrower_key,
        &mut wallet,
        &tusd,
        3 * USD,
        purpose::REPAY,
        1,
    )?;
    let now = env.now()?;
    let p = vault::repay(
        keeper,
        &owner,
        reserve,
        position,
        i,
        V,
        now,
        u128::from(now) + 3,
        tree_id,
    )?;
    let (pos, res) = (
        p.outputs[REPAY_POSITION_SLOT].clone(),
        p.outputs[REPAY_RESERVE_SLOT].clone(),
    );
    ensure!(p.outputs[REPAY_CHANGE_SLOT].amount == 0, "no change");
    let lender = lender_key.pubkey().to_bytes();
    env.keeper_sends_after("repay (partial)", p, |ix| {
        let resend = |edit: &dyn Fn(&mut RepayArgs)| {
            send(rpc, &[&env.keeper_key], tampered(ix, |a| edit(a)))
        };
        refused_with(
            "repay for another owner",
            resend(&|a| {
                a.owner = lender;
                a.user_bump = canonical(USER_SEED, &lender);
            }),
            INVALID_PROOF,
        )?;
        refused_with(
            "repay at another now",
            resend(&|a| a.now -= 1),
            INVALID_PROOF,
        )?;
        refused_with(
            "repay with another expiry",
            resend(&|a| a.expiry += 1),
            INVALID_PROOF,
        )?;
        refused_with(
            "repay with a non-canonical user bump",
            resend(&|a| a.user_bump = non_canonical(USER_SEED, &a.owner)),
            INVALID_SEEDS,
        )
    })?;
    (position, reserve) = (env.indexed(&user, &pos)?, env.indexed(&pool, &res)?);
    let r: ReserveState = decode(&reserve)?;
    ensure!(
        r.borrow_price > 1_000_000_000_000,
        "interest accrued: {r:?}"
    );
    let owed = debt(&position, &reserve)?;
    ensure!(owed > 8 * USD && owed < 8 * USD + 1_000, "owed {owed}");
    println!(
        "repaid 3 tUSD; borrow price {}, owed {owed}",
        r.borrow_price
    );

    // 6. The price drops to $6.10: past the threshold, under the max limit.
    let mut keeper_wallet = TestWallet::scan(keeper.clone(), assets.clone(), &env.localnet.client)?;
    let mut second_wallet = TestWallet::scan(second.clone(), assets.clone(), &env.localnet.client)?;
    let liquidate = |key: &Keypair,
                     keeper_wallet: &mut TestWallet,
                     reserve: WalletUtxo,
                     position: WalletUtxo,
                     bound: u64|
     -> Result<(WalletUtxo, WalletUtxo, core::Liquidation)> {
        let keeper = &keeper_wallet.keypair.clone();
        keeper_wallet.rescan(&env.localnet.client)?;
        let funds = keeper_wallet
            .spendable_utxo(tusd, 10 * USD)
            .context("no keeper funds")?;
        let (reserve_before, funds_amount) = (reserve.utxo.amount, funds.utxo.amount);
        let now = env.now()?;
        let (p, plan) = vault::liquidate(
            keeper,
            &owner,
            reserve,
            position,
            funds,
            price,
            V,
            now,
            bound,
            u128::from(now) + 4,
            tree_id,
        )?;
        let (pos, res, change, payout) = (
            p.outputs[LIQUIDATE_POSITION_SLOT].clone(),
            p.outputs[LIQUIDATE_RESERVE_SLOT].clone(),
            p.outputs[LIQUIDATE_CHANGE_SLOT].clone(),
            p.outputs[LIQUIDATE_PAYOUT_SLOT].clone(),
        );
        ensure!(payout.amount == plan.payout, "payout");
        // The liquidator pays exactly x: the reserve gains x, nothing more.
        ensure!(
            p.outputs[LIQUIDATE_RESERVE_SLOT].amount == reserve_before + plan.repay
                && change.amount == funds_amount - plan.repay,
            "funding of exactly x: {plan:?}"
        );
        env.sends_as(key, keeper, "liquidate", p, |ix| {
            refused_with(
                "liquidate at a higher bound",
                send(
                    rpc,
                    &[key],
                    tampered(ix, |a: &mut LiquidateArgs| a.bound += 1),
                ),
                INVALID_PROOF,
            )
        })?;
        env.wait(&change)?;
        env.wait(&payout)?;
        Ok((env.indexed(&user, &pos)?, env.indexed(&pool, &res)?, plan))
    };
    let bound = 6 * DOLLAR + DOLLAR / 10;
    env.set_price(bound)?;
    let (pos, res, plan) = liquidate(
        &env.keeper_key,
        &mut keeper_wallet,
        reserve,
        position,
        bound + CONF,
    )?;
    (position, reserve) = (pos, res);
    let owed = debt(&position, &reserve)?;
    let value = core::collateral_value(position.utxo.amount, bound + CONF).map_err(err)?;
    // Back to the threshold: the rule keeps `debt x 1e4 >= lt x value`, so it
    // stops at it, within a unit of debt plus the value's rounding.
    let gap = u128::from(owed) * 10_000 - 8_500 * u128::from(value);
    ensure!(
        plan.repay > 0
            && plan.debt_shares == decode::<PositionState>(&position)?.debt_shares
            && plan.collateral == position.utxo.amount
            && gap < 20_000
            && !core::above_max_limit(owed, value, &core::TSOL_TUSD_VAULT),
        "back to the threshold: {plan:?}, owed {owed}, value {value}"
    );
    println!(
        "liquidated {plan:?} at $6.11; owed {owed}, collateral {}",
        position.utxo.amount
    );

    // 7. The price drops to $5.50: past the max limit; all of the debt goes.
    let bound = 5 * DOLLAR + DOLLAR / 2;
    env.set_price(bound)?;
    let value = core::collateral_value(position.utxo.amount, bound + CONF).map_err(err)?;
    ensure!(
        core::above_max_limit(owed, value, &core::TSOL_TUSD_VAULT),
        "above max"
    );
    let (pos, res, plan) = liquidate(
        &second_key,
        &mut second_wallet,
        reserve,
        position,
        bound + CONF,
    )?;
    (position, reserve) = (pos, res);
    ensure!(
        plan.repay == owed && decode::<PositionState>(&position)?.debt_shares == 0,
        "full liquidation {plan:?}"
    );
    ensure!(position.utxo.amount > 0, "collateral left to close");
    println!(
        "liquidated all {plan:?}; collateral left {}",
        position.utxo.amount
    );

    // 8. The borrower closes position 1 itself.
    let close = |position: WalletUtxo| -> Result<()> {
        let left = position.utxo.amount;
        let p = vault::close(&borrower, position, V, tree_id)?;
        let out = p.outputs[CLOSE_PAYOUT_SLOT].clone();
        ensure!(out.amount == left, "close pays all");
        measure::lend_proof("close", &p)?;
        let ix = p.prove(indexer, tree, &borrower)?;
        let sig = send(rpc, &[&borrower_key], ix.clone())?;
        env.confirm(sig)?;
        env.report("close", &borrower_key.pubkey(), &ix, &sig)?;
        env.wait(&out)?;
        Ok(())
    };
    let before = balance(&mut wallet, &env, sol)?;
    let left = position.utxo.amount;
    close(position)?;
    ensure!(balance(&mut wallet, &env, sol)? == before + left, "closed");
    println!("position 1 closed");

    // 9. Position 2: borrow 2 tUSD at $5.50, repay all with 3 (1 back), close.
    let i = intent(
        &env,
        &borrower_key,
        &mut wallet,
        &sol,
        SOL,
        purpose::DEPOSIT,
        2,
    )?;
    let p = vault::deposit_open(keeper, &owner, i, V, env.now()?, tree_id)?;
    let out = p.outputs[DEPOSIT_POSITION_SLOT].clone();
    env.keeper_sends("deposit_open", p)?;
    let position = env.indexed(&user, &out)?;
    env.set_price(bound)?;
    let (position, res, pay) = draw(reserve, position, 2 * USD, bound - CONF)?;
    reserve = res;
    env.wait(&pay)?;
    let usd_before = balance(&mut wallet, &env, tusd)?;
    let i = intent(
        &env,
        &borrower_key,
        &mut wallet,
        &tusd,
        3 * USD,
        purpose::REPAY,
        2,
    )?;
    let now = env.now()?;
    let owed = core::debt_of(
        decode::<PositionState>(&position)?.debt_shares,
        vault::accrued(&decode(&reserve)?, now)?.borrow_price,
    )
    .map_err(err)?;
    let p = vault::repay(
        keeper,
        &owner,
        reserve,
        position,
        i,
        V,
        now,
        u128::from(now) + 5,
        tree_id,
    )?;
    let (pos, change) = (
        p.outputs[REPAY_POSITION_SLOT].clone(),
        p.outputs[REPAY_CHANGE_SLOT].clone(),
    );
    ensure!(change.amount == 3 * USD - owed, "change {}", change.amount);
    env.keeper_sends("repay (all, with change)", p)?;
    let position = env.indexed(&user, &pos)?;
    ensure!(
        decode::<PositionState>(&position)?.debt_shares == 0,
        "repaid all"
    );
    env.wait(&change)?;
    ensure!(
        balance(&mut wallet, &env, tusd)? == usd_before - owed,
        "the change came back"
    );
    close(position)?;
    println!("position 2 repaid ({owed}) and closed");
    Ok(())
}
