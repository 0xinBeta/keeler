//! The lend keeper end to end on a local network: a test-only "page" signs
//! requests, makes intents and co-signs owner builds; the keeper fills,
//! builds, liquidates, and is killed and restarted along the way. Prints the
//! time each operation took.
//!
//! Run through `scripts/run-lend-e2e.sh` (port offset 2500, keeper HTTP on
//! 47800; release, for the provers). Ignored in a plain `cargo test`.
use std::{
    collections::BTreeMap,
    net::{SocketAddr, TcpListener},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    thread::{self, sleep},
    time::{Duration, Instant},
};

use anyhow::{anyhow, ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use keeler::{
    chain::shielded,
    config::Network,
    dealer::{self, Api},
    keeper::{self, Book, Built, Intent, Job, Keeper, Ledger, Lend, Note, Op, Quote, TicketStatus},
    lend_auth::{authorisation_text, canonical, request_bytes, to_hex},
    lend_chain::{self, LendChain},
};
use keeler_core::lend as core;
use keeler_lend::circuits::INTENT_SLOT;
use keeler_lend_sdk::{builders, purpose, user_pda, PositionState, ReceiptState};
use serde_json::{json, Value};
use solana_address::Address;
use solana_keypair::Keypair;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use spr_client::{ComputeBudgetConfig, Rpc};
use spr_keypair::ShieldedKeypair;
use spr_program::instruction::{AssetDeposit, Deposit};
use spr_program_test::{
    fixture,
    localnet::{FixtureLocalnet, LocalnetPaths, LocalnetPorts},
    SprProgramTest,
};
use spr_transaction::SOL_MINT;

const USD: u64 = 1_000_000;
const SOL: u64 = 1_000_000_000;
/// tUSD base units per tSOL base unit x 1e12 ($1 = 1e9).
const DOLLAR: u64 = 1_000_000_000;
const CONF: u64 = 10_000_000;
const HTTP_PORT: u16 = 47_800;
const KEY: &str = "lend-e2e-http-key-0123";
const NET: &str = "localnet";
const ORIGIN: &str = "http://127.0.0.1:5300";

fn env_path(name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("{name} is not set; source .tools/env"))
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// Times of each operation, printed at the end.
static TIMES: Mutex<Vec<(String, Duration)>> = Mutex::new(Vec::new());

fn timed<T>(name: &str, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let out = f();
    let took = t.elapsed();
    println!("LEND {name}: {:.1} s", took.as_secs_f64());
    TIMES.lock().unwrap().push((name.into(), took));
    out
}

/// The real chain with a kill switch on send.
struct Probe<'a> {
    inner: &'a LendChain,
    /// The next send is "killed": recorded, never sent.
    drop_send: AtomicBool,
    builds: Mutex<Vec<Op>>,
}

impl Ledger for Probe<'_> {
    fn now(&self) -> Result<u64> {
        self.inner.now()
    }
    fn registered(&self, w: &Address) -> Result<bool> {
        self.inner.registered(w)
    }
    fn spent(&self, n: &[[u8; 32]]) -> Result<Vec<bool>> {
        self.inner.spent(n)
    }
    fn spender(&self, n: &[u8; 32]) -> Result<Option<Vec<([u8; 32], u64)>>> {
        self.inner.spender(n)
    }
    fn intents(
        &self,
        w: &Address,
        after: Option<&str>,
        pages: usize,
    ) -> Result<(Vec<Intent>, Option<String>)> {
        self.inner.intents(w, after, pages)
    }
    fn quote(&self, v: u64) -> Result<Quote> {
        self.inner.quote(v)
    }
    fn set_price(&self, v: u64, price: u64, conf: u64) -> Result<String> {
        self.inner.set_price(v, price, conf)
    }
    fn build(&self, job: &Job, reserve: Option<&Note>, now: u64) -> Result<Built> {
        let b = self.inner.build(job, reserve, now)?;
        self.builds.lock().unwrap().push(b.pending.op);
        Ok(b)
    }
    fn send(&self, tx: &[u8]) -> Result<String> {
        if self.drop_send.swap(false, Ordering::SeqCst) {
            anyhow::bail!("killed before the send");
        }
        self.inner.send(tx)
    }
    fn debt_held(&self) -> Result<u64> {
        self.inner.debt_held()
    }
    fn history(&self, payer: &Address, after: Option<&str>) -> Result<(Vec<Note>, Option<String>)> {
        self.inner.history(payer, after)
    }
}

/// `/health` for the HTTP server.
struct NoSwap;

impl Api for NoSwap {
    fn quote(&self, _: keeler_core::rfq::Rfq) -> Result<keeler_core::rfq::Quote, String> {
        Err("no".into())
    }
    fn faucet(&self, _: Address) -> Result<(), (u16, String)> {
        Err((404, "off".into()))
    }
    fn now(&self) -> Result<u64, String> {
        Ok(0)
    }
}

/// A wallet as the page drives it.
struct Page {
    key: Keypair,
    kp: ShieldedKeypair,
    session: Keypair,
    expiry: u64,
}

impl Page {
    fn new(key: Keypair, now: u64) -> Self {
        Self {
            kp: shielded(&key).unwrap(),
            key,
            session: Keypair::new(),
            expiry: now + 80_000,
        }
    }

    fn addr(&self) -> Address {
        self.key.pubkey()
    }

    fn signed(
        &self,
        method: &str,
        path: &str,
        extra: &[(&str, String)],
        ts: u64,
    ) -> BTreeMap<String, String> {
        let mut f = BTreeMap::new();
        f.insert("wallet".into(), self.addr().to_string());
        f.insert("sessionKey".into(), self.session.pubkey().to_string());
        f.insert("sessionExpiry".into(), self.expiry.to_string());
        f.insert("origin".into(), ORIGIN.into());
        let text = authorisation_text(
            NET,
            ORIGIN,
            &keeler_lend::ID,
            &self.addr(),
            &self.session.pubkey(),
            self.expiry,
        );
        f.insert(
            "sessionAuth".into(),
            to_hex(self.key.sign_message(text.as_bytes()).as_ref()),
        );
        for (k, v) in extra {
            f.insert((*k).into(), v.clone());
        }
        let nonce: [u8; 16] = rand16();
        let m = request_bytes(method, path, &nonce, ts, &canonical(&f));
        f.insert("nonce".into(), to_hex(&nonce));
        f.insert("timestamp".into(), ts.to_string());
        f.insert(
            "signature".into(),
            to_hex(self.session.sign_message(&m).as_ref()),
        );
        f
    }
}

fn rand16() -> [u8; 16] {
    let mut b = [0; 16];
    getrandom::getrandom(&mut b).unwrap();
    b
}

struct Env {
    localnet: FixtureLocalnet,
    network: Network,
    chain: LendChain,
    tusd: Address,
    http: reqwest::blocking::Client,
}

impl Env {
    fn rpc(&self) -> &impl Rpc {
        self.localnet.client.rpc()
    }

    fn now(&self) -> u64 {
        self.chain.now().unwrap()
    }

    fn post(&self, path: &str, fields: &BTreeMap<String, String>) -> (u16, Value) {
        self.post_value(path, &serde_json::to_value(fields).unwrap())
    }

    fn post_value(&self, path: &str, body: &Value) -> (u16, Value) {
        let r = self
            .http
            .post(format!("http://127.0.0.1:{HTTP_PORT}{path}"))
            .header("x-keeler-key", KEY)
            .json(body)
            .send()
            .unwrap();
        (r.status().as_u16(), r.json().unwrap())
    }

    fn get(&self, path: &str) -> (u16, Value) {
        let r = self
            .http
            .get(format!("http://127.0.0.1:{HTTP_PORT}{path}"))
            .header("x-keeler-key", KEY)
            .send()
            .unwrap();
        (r.status().as_u16(), r.json().unwrap())
    }

    fn account(&self, page: &Page) -> Value {
        let f = page.signed("POST", "/lend/account", &[], self.now());
        let (st, v) = self.post("/lend/account", &f);
        assert_eq!(st, 200, "{v}");
        v
    }

    fn ticket(&self, page: &Page, extra: &[(&str, String)]) -> (u16, Value) {
        self.post(
            "/lend/ticket",
            &page.signed("POST", "/lend/ticket", extra, self.now()),
        )
    }

    /// A ticket, then the intent it names, as the page makes it.
    fn intent(&self, page: &Page, purpose: u8, target: u64, amount: u64) -> Result<String> {
        let extra: Vec<(&str, String)> = match purpose {
            purpose::SUPPLY => vec![
                ("purpose", "supply".into()),
                ("pool", self.tusd.to_string()),
                ("amount", amount.to_string()),
            ],
            purpose::DEPOSIT => vec![
                ("purpose", "deposit".into()),
                ("vault", "1".into()),
                ("position", target.to_string()),
                ("amount", amount.to_string()),
            ],
            _ => vec![
                ("purpose", "repay".into()),
                ("vault", "1".into()),
                ("position", target.to_string()),
                ("amount", amount.to_string()),
            ],
        };
        let (st, t) = self.ticket(page, &extra);
        ensure!(st == 200, "ticket: {st} {t}");
        let expiry = t["expiry"].as_u64().context("expiry")?;
        let mint = if purpose == purpose::DEPOSIT {
            SOL_MINT
        } else {
            self.tusd
        };
        self.raw_intent(page, mint, purpose, target, amount, expiry)?;
        Ok(t["id"].as_str().unwrap().to_owned())
    }

    /// An intent with no ticket behind it.
    fn raw_intent(
        &self,
        page: &Page,
        mint: Address,
        purpose: u8,
        target: u64,
        amount: u64,
        expiry: u64,
    ) -> Result<()> {
        let mut notes = self.chain.chain.notes(&page.kp, mint)?;
        notes.sort_by_key(|u| u.utxo.amount);
        let source = notes
            .into_iter()
            .find(|u| u.utxo.amount >= amount)
            .context("no note covers the intent")?;
        let p = builders::intent(
            &page.kp,
            source,
            &mint,
            amount,
            purpose,
            target,
            expiry,
            self.chain.kp.viewing_pubkey(),
            0,
        )?;
        let out = p.outputs[INTENT_SLOT].clone();
        let ix = p.prove(self.localnet.client.indexer(), self.localnet.tree, &page.kp)?;
        let sig = self.rpc().create_and_send_transaction(
            std::slice::from_ref(&ix),
            page.addr(),
            &[&page.key],
            ComputeBudgetConfig::new(1_400_000),
        )?;
        self.localnet
            .client
            .confirm_private_transaction_sync(sig)
            .map_err(|e| anyhow!("{e:?}"))?;
        let _ = (out, user_pda(&page.addr()));
        Ok(())
    }

    /// An owner build: the reply, checked and co-signed, and sent unless
    /// `send` is false; the transaction either way.
    fn build(
        &self,
        page: &Page,
        extra: &[(&str, String)],
    ) -> Result<(u16, Value, Option<VersionedTransaction>)> {
        let (st, v) = self.post(
            "/lend/build",
            &page.signed("POST", "/lend/build", extra, self.now()),
        );
        if st != 200 {
            return Ok((st, v, None));
        }
        let wire = B64.decode(v["transaction"].as_str().context("no transaction")?)?;
        ensure!(wire.len() <= 4096, "transaction too large");
        let mut tx: VersionedTransaction = wincode::deserialize(&wire)?;
        let keys = tx.message.static_account_keys().to_vec();
        // Any keeper of the set may build; the one the reply names pays.
        let keeper: Address = v["keeper"].as_str().context("no keeper")?.parse()?;
        ensure!(
            keeler_lend::KEEPERS.contains(&keeper) && keys[0] == keeper,
            "the named keeper pays"
        );
        let i = keys
            .iter()
            .position(|k| *k == page.addr())
            .context("owner not a signer")?;
        let h = tx.message.header();
        let signers = usize::from(h.num_required_signatures);
        ensure!(i < signers, "owner must sign");
        ensure!(
            i >= signers - usize::from(h.num_readonly_signed_accounts),
            "the owner must be read-only"
        );
        tx.signatures[i] = page.key.sign_message(&tx.message.serialize());
        Ok((st, v, Some(tx)))
    }

    fn send(&self, tx: VersionedTransaction) -> Result<()> {
        let sig = self.rpc().process_transaction(tx)?;
        self.localnet
            .client
            .confirm_private_transaction_sync(sig)
            .map_err(|e| anyhow!("{e:?}"))?;
        Ok(())
    }

    fn set_price(&self, dollars_x10: u64) {
        lend_chain::set_test_price(&self.chain, 1, dollars_x10 * DOLLAR / 10, CONF).unwrap();
    }

    fn deposit(&self, d: AssetDeposit) {
        let payer = fixture::payer();
        let ix = Deposit {
            tree: self.localnet.tree,
            depositor: payer.pubkey(),
            deposits: vec![d],
        }
        .instruction()
        .unwrap();
        let sig = self
            .rpc()
            .create_and_send_transaction(
                &[ix],
                payer.pubkey(),
                &[&payer],
                ComputeBudgetConfig::for_instruction_count(1),
            )
            .unwrap();
        self.localnet
            .client
            .confirm_private_transaction_sync(sig)
            .unwrap();
    }
}

/// The program's Pyth account (the build default: SOL/USD's push account).
const PYTH_SOL_USD: &str = "7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE";

/// Its bytes on mainnet now (`KEELER_MAINNET_RPC`, default the public RPC).
fn mainnet_pyth_account() -> Result<Vec<u8>> {
    let url = std::env::var("KEELER_MAINNET_RPC")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".into());
    let v: Value = reqwest::blocking::Client::new()
        .post(url)
        .json(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "getAccountInfo",
            "params": [PYTH_SOL_USD, {"encoding": "base64"}]}),
        )
        .send()?
        .json()?;
    let a = &v["result"]["value"];
    ensure!(
        a["owner"] == json!(keeler_lend::vaults::PYTH_RECEIVER.to_string()),
        "not the Pyth receiver's: {v}"
    );
    let d = B64.decode(a["data"][0].as_str().context("no data")?)?;
    ensure!(d.len() >= 101, "short PriceUpdateV2");
    Ok(d)
}

/// Writes `raw` at the Pyth address with `price` and the chain's time less
/// `lag` seconds as the publish time, as a Pyth push would.
fn push_pyth(rpc: &str, raw: &[u8], price: i64, lag: i64) -> Result<()> {
    let call = |method: &str, params: Value| -> Result<Value> {
        let v: Value = reqwest::blocking::Client::new()
            .post(rpc)
            .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
            .send()?
            .json()?;
        ensure!(v.get("error").is_none(), "{method}: {v}");
        Ok(v["result"].clone())
    };
    let slot = call("getSlot", json!([]))?;
    let now = call("getBlockTime", json!([slot]))?
        .as_i64()
        .context("no block time")?;
    let mut d = raw.to_vec();
    d[73..81].copy_from_slice(&price.to_le_bytes());
    d[93..101].copy_from_slice(&(now - lag).to_le_bytes());
    call(
        "surfnet_setAccount",
        json!([PYTH_SOL_USD, {"lamports": 1_825_031, "data": to_hex(&d),
            "owner": keeler_lend::vaults::PYTH_RECEIVER.to_string()}]),
    )?;
    Ok(())
}

/// Runs passes until `done` holds of the book, or fails after `secs`.
fn settle(k: &Keeper<Probe>, what: &str, secs: u64, done: impl Fn(&Book) -> bool) -> Result<()> {
    let started = Instant::now();
    loop {
        if let Err(e) = k.step() {
            println!("step: {e:#}");
        }
        let b = k.snapshot()?;
        if done(&b) {
            return Ok(());
        }
        ensure!(
            started.elapsed() < Duration::from_secs(secs),
            "{what}: timed out; pending {:?}",
            b.pending.iter().map(|p| p.op).collect::<Vec<_>>()
        );
        sleep(Duration::from_millis(500));
    }
}

fn filled(id: &str) -> impl Fn(&Book) -> bool + '_ {
    move |b| {
        b.pending.is_empty()
            && b.tickets
                .iter()
                .any(|t| t.id == id && t.status == TicketStatus::Filled)
    }
}

fn debt(k: &Keeper<Probe>, owner: &Address, id: u64, now: u64) -> u64 {
    let b = k.snapshot().unwrap();
    let p = b
        .positions
        .iter()
        .find(|p| {
            p.wallet.as_deref() == Some(&owner.to_string())
                && p.state::<PositionState>().unwrap().position_id == id
        })
        .unwrap();
    let r = keeler_lend_sdk::vault::accrued(&b.reserve.as_ref().unwrap().state().unwrap(), now)
        .unwrap();
    core::debt_of(
        p.state::<PositionState>().unwrap().debt_shares,
        r.borrow_price,
    )
    .unwrap()
}

/// The note hashes of `v`, sorted.
fn hashes(v: &[Note]) -> Vec<String> {
    let mut h: Vec<String> = v.iter().map(|n| n.hash.clone()).collect();
    h.sort();
    h
}

/// One process of the first keeper: the HTTP server on its own thread while
/// `f` runs.
fn session<T>(
    probe: &Probe,
    cfg: &Lend,
    state: &std::path::Path,
    f: impl FnOnce(&Keeper<Probe>) -> Result<T>,
) -> Result<T> {
    session_as(probe, cfg, state, &fixture::actor(0), f)
}

/// One keeper process under `signer`.
fn session_as<T>(
    probe: &Probe,
    cfg: &Lend,
    state: &std::path::Path,
    signer: &Keypair,
    f: impl FnOnce(&Keeper<Probe>) -> Result<T>,
) -> Result<T> {
    let k = Keeper::open(cfg, probe, signer, state.to_path_buf())?;
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], HTTP_PORT)))?;
    let stop = AtomicBool::new(false);
    thread::scope(|s| {
        let server = s.spawn(|| {
            dealer::serve_http(
                listener,
                KEY,
                &stop,
                &NoSwap,
                Some(&k),
                Duration::from_secs(3),
                false,
            )
        });
        let out = f(&k);
        stop.store(true, Ordering::Relaxed);
        server.join().unwrap()?;
        out
    })
}

#[test]
#[ignore = "needs a local network: scripts/run-lend-e2e.sh"]
fn keeper_end_to_end() -> Result<()> {
    let ports = LocalnetPorts::for_test(0)?;
    let localnet = FixtureLocalnet::start(
        "keeler-lend-keeper",
        ports,
        vec![
            (
                keeler_lend::ID,
                root().join("lend/target/deploy/keeler_lend.so"),
            ),
            (
                spr_user_registry_interface::user_registry_program_id(),
                env_path("USER_REGISTRY_PROGRAM_PATH"),
            ),
        ],
        &LocalnetPaths {
            proving_keys_dir: PathBuf::from(std::env::var("HOME")?)
                .join(".config/zolana/proving-keys"),
            scratch_dir: root().join("target/localnet"),
            ..LocalnetPaths::workspace()
        },
    )?;
    ensure!(localnet.tree_id == 0, "tree 0");
    let tusd = fixture::spl_mint();
    let keeper_key = fixture::actor(0);
    let admin = fixture::actor(3);
    ensure!(
        keeler_lend::KEEPERS == [keeper_key.pubkey(), fixture::payer().pubkey()]
            && admin.pubkey() == keeler_lend::ADMIN,
        "default keepers and admin"
    );
    let network = Network::local(ports.rpc_url(), ports.photon_url());
    let lend_chain = |key: &Keypair, price| {
        LendChain::connect(
            &network,
            key,
            keeler_lend_sdk::fixture_keeper_keypair(key)?,
            price,
            0,
        )
    };
    let chain = lend_chain(&keeper_key, keeper::LendPrice::Test)?;
    let pyth_chain = lend_chain(&keeper_key, keeper::LendPrice::Pyth)?;
    let second_key = fixture::payer();
    let second_chain = lend_chain(&second_key, keeper::LendPrice::Test)?;
    let env = Env {
        localnet,
        network: network.clone(),
        chain,
        tusd,
        http: reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()?,
    };
    let _ = &env.network;
    let now = env.now();
    let borrower = Page::new(fixture::actor(1), now);
    let lender = Page::new(fixture::actor(2), now);
    for p in [&borrower, &lender] {
        env.chain.chain.register(&p.kp)?;
    }
    let shield = |to: &ShieldedKeypair, sol: u64, usd: u64| {
        let a = to.shielded_address().unwrap();
        if sol > 0 {
            env.deposit(SprProgramTest::wallet_sol_shield_data(sol, &a).unwrap());
        }
        if usd > 0 {
            env.deposit(
                SprProgramTest::wallet_spl_shield_data(
                    usd,
                    &a,
                    &tusd,
                    &fixture::payer_token_account(),
                )
                .unwrap(),
            );
        }
    };
    shield(&lender.kp, 0, 40 * USD);
    shield(&borrower.kp, 4 * SOL, 2 * USD);
    shield(&env.chain.kp, 0, 30 * USD);
    timed("init_reserve (admin CLI path)", || {
        lend_chain::init_reserve(&env.chain, &admin)
    })?;

    let dir = root().join("target/localnet/lend-keeper");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let state = dir.join("state.json");
    let cfg: Lend = serde_json::from_value(json!({
        "program_id": keeler_lend::ID.to_string(),
        "network": NET,
        "origins": [ORIGIN],
        "pause_file": dir.join("pause"),
        "limits": { "tickets_per_wallet_hour": 100 }
    }))?;
    cfg.check()?;
    let probe = Probe {
        inner: &env.chain,
        drop_send: AtomicBool::new(false),
        builds: Mutex::new(Vec::new()),
    };

    // 1. Supply, deposit, borrow, interest, repay, redeem.
    let pos1 = 1;
    session(&probe, &cfg, &state, |k| {
        settle(k, "reserve found", 60, |b| b.reserve.is_some())?;
        let id = env.intent(&lender, purpose::SUPPLY, 1, 20 * USD)?;
        timed("supply fill (find, build, prove, send)", || {
            settle(k, "supply", 180, filled(&id))
        })?;
        let id = env.intent(&borrower, purpose::DEPOSIT, pos1, 3 * SOL / 2)?;
        timed("deposit_open fill", || {
            settle(k, "deposit", 180, filled(&id))
        })?;
        let v = env.account(&borrower);
        assert_eq!(
            v["positions"][0]["collateral"],
            json!((3 * SOL / 2).to_string())
        );
        assert!(v["positions"][0]["message"]
            .as_str()
            .is_some_and(|m| !m.is_empty()));

        env.set_price(100);
        let draw = |borrow: u64| {
            vec![
                ("op", "draw".to_string()),
                ("vault", "1".into()),
                ("position", pos1.to_string()),
                ("borrow", borrow.to_string()),
                ("withdraw", "0".into()),
            ]
        };
        // Too much for 1.5 tSOL at $10 (cap 0.8 x 15 = 12 less the margin).
        assert_eq!(env.build(&borrower, &draw(12 * USD))?.0, 423);
        let (st, v, tx) = timed("draw build (HTTP, prove)", || {
            env.build(&borrower, &draw(11 * USD))
        })?;
        ensure!(st == 200, "draw: {v}");
        assert!(
            v["bound"].is_string()
                && v["index"].is_string()
                && v["outputs"].as_array().is_some_and(|o| !o.is_empty())
        );
        timed("draw co-sign and land", || env.send(tx.unwrap()))?;
        settle(k, "draw resolved", 120, |b| {
            b.pending.is_empty() && b.reservation.is_none()
        })?;
        let owed = debt(k, &borrower.addr(), pos1, env.now());
        assert!(owed >= 11 * USD, "owed {owed}");

        sleep(Duration::from_secs(4));
        let id = env.intent(&borrower, purpose::REPAY, pos1, 3 * USD)?;
        timed("repay fill", || settle(k, "repay", 180, filled(&id)))?;
        let owed = debt(k, &borrower.addr(), pos1, env.now());
        assert!(
            owed > 8 * USD && owed < 8 * USD + 1_000,
            "interest accrued: owed {owed}"
        );

        let redeem = |amount: u64| {
            vec![
                ("op", "redeem".to_string()),
                ("pool", tusd.to_string()),
                ("amount", amount.to_string()),
            ]
        };
        let (st, v, tx) = timed("redeem build (HTTP, prove)", || {
            env.build(&lender, &redeem(2 * USD))
        })?;
        ensure!(st == 200, "redeem: {v}");
        timed("redeem co-sign and land", || env.send(tx.unwrap()))?;
        settle(k, "redeem resolved", 120, |b| b.pending.is_empty())?;
        let v = env.account(&lender);
        let shares: u64 = v["receipts"][0]["shares"].as_str().unwrap().parse()?;
        assert!(shares < 20 * USD && shares > 17 * USD, "shares {shares}");
        let (st, p) = env.get("/lend/pools");
        assert_eq!(st, 200);
        println!("pools: {p}");

        // 2. The price drops to $6.10: a partial liquidation to the threshold.
        env.set_price(61);
        timed("liquidation (partial)", || {
            settle(k, "liquidation", 180, |b| {
                b.pending.is_empty() && probe.builds.lock().unwrap().contains(&Op::Liquidate)
            })
        })?;
        let owed = debt(k, &borrower.addr(), pos1, env.now());
        assert!(owed > 0, "partial");
        let b = k.snapshot()?;
        let p = b
            .positions
            .iter()
            .find(|p| p.wallet.as_deref() == Some(&borrower.addr().to_string()))
            .unwrap();
        let value = core::collateral_value(p.amount, 61 * DOLLAR / 10 + CONF).unwrap();
        assert!(!core::above_max_limit(owed, value, &core::TSOL_TUSD_VAULT));
        // 3. $5.50: past the max limit, all of the debt.
        env.set_price(55);
        timed("liquidation (full, above max)", || {
            settle(k, "full liquidation", 180, |b| {
                b.pending.is_empty()
                    && b.positions
                        .iter()
                        .all(|p| p.state::<PositionState>().unwrap().debt_shares == 0)
            })
        })?;
        Ok(())
    })?;

    // 4. Killed between build and landing: the supply is sent, the keeper
    // dies before it sees it land, and the restart finds it landed.
    let id = session(&probe, &cfg, &state, |k| {
        let id = env.intent(&lender, purpose::SUPPLY, 1, 3 * USD)?;
        let started = Instant::now();
        while k.snapshot()?.pending.is_empty() {
            k.step()?;
            ensure!(started.elapsed() < Duration::from_secs(180), "no supply");
            sleep(Duration::from_millis(500));
        }
        Ok(id)
    })?;
    session(&probe, &cfg, &state, |k| {
        let b = k.snapshot()?;
        ensure!(b.pending.is_empty(), "the restart settled the build");
        ensure!(
            b.tickets
                .iter()
                .any(|t| t.id == id && t.status == TicketStatus::Filled),
            "the ticket is filled"
        );
        ensure!(
            b.receipts
                .iter()
                .filter(|r| r.wallet.as_deref() == Some(&lender.addr().to_string()))
                .count()
                == 2,
            "two receipts"
        );
        Ok(())
    })?;

    // 5. Killed between record and send: nothing lands; after the window
    // the record is dropped and the ticket is filled on the retry.
    probe.drop_send.store(true, Ordering::SeqCst);
    let id = session(&probe, &cfg, &state, |k| {
        let id = env.intent(&lender, purpose::SUPPLY, 1, 2 * USD)?;
        let started = Instant::now();
        while k.snapshot()?.pending.is_empty() {
            k.step()?;
            ensure!(started.elapsed() < Duration::from_secs(180), "no supply");
            sleep(Duration::from_millis(500));
        }
        Ok(id)
    })?;
    session(&probe, &cfg, &state, |k| {
        ensure!(
            k.snapshot()?.pending.len() == 1,
            "the record survived the kill"
        );
        timed("recovery from an unsent record (window + refill)", || {
            settle(k, "refill", 400, filled(&id))
        })
    })?;

    // 6. Two users race for one reserve; and an owner build never sent.
    session(&probe, &cfg, &state, |k| {
        env.set_price(100);
        // The borrower withdraws what the liquidations left; the lender
        // redeems. Both spend the one reserve.
        let mine = |b: &Book| {
            b.positions
                .iter()
                .find(|p| p.wallet.as_deref() == Some(&borrower.addr().to_string()))
                .cloned()
                .unwrap()
        };
        let left = mine(&k.snapshot()?);
        let draw = vec![
            ("op", "draw".to_string()),
            ("vault", "1".into()),
            ("position", pos1.to_string()),
            ("borrow", "0".into()),
            ("withdraw", left.amount.to_string()),
        ];
        let (st, v, unsent) = env.build(&borrower, &draw)?;
        ensure!(st == 200, "draw: {v}");
        // The lender withdraws everything in their smallest receipt, by
        // shares: that receipt ends at exactly zero.
        let receipts = |b: &Book| -> Vec<u64> {
            b.receipts
                .iter()
                .filter(|r| r.wallet.as_deref() == Some(&lender.addr().to_string()))
                .map(|r| r.state::<ReceiptState>().unwrap().supply_shares)
                .collect()
        };
        let held = receipts(&k.snapshot()?);
        let smallest = *held.iter().min().unwrap();
        let redeem = vec![
            ("op", "redeem".to_string()),
            ("pool", tusd.to_string()),
            ("shares", smallest.to_string()),
        ];
        ensure!(
            env.build(&lender, &redeem)?.0 == 409,
            "the pool is reserved for the borrower"
        );
        sleep(Duration::from_secs(21));
        let (st, v, tx) = env.build(&lender, &redeem)?;
        ensure!(st == 200, "the reservation expired: {v}");
        ensure!(
            k.snapshot()?.pending.len() == 2,
            "two builds on one reserve"
        );
        env.send(tx.unwrap())?;
        ensure!(env.send(unsent.unwrap()).is_err(), "the loser cannot land");
        settle(k, "race resolved", 120, |b| b.pending.is_empty())?;
        ensure!(mine(&k.snapshot()?) == left, "the withdrawal did not land");
        let after = receipts(&k.snapshot()?);
        ensure!(
            after.len() == held.len() - 1 && !after.contains(&0),
            "the shares redeem emptied one receipt: {held:?} -> {after:?}"
        );
        Ok(())
    })?;

    // 7. A flood of dust and unticketed intents: no builds.
    session(&probe, &cfg, &state, |k| {
        let before = probe.builds.lock().unwrap().len();
        let far = env.now() + 600;
        env.raw_intent(&lender, tusd, purpose::SUPPLY, 1, 5 * USD, far)?;
        env.raw_intent(&lender, tusd, purpose::SUPPLY, 1, 1, far)?;
        env.raw_intent(&borrower, SOL_MINT, purpose::DEPOSIT, 9, SOL / 100, far)?;
        // A dust ticket is refused outright.
        let (st, _) = env.ticket(
            &lender,
            &[
                ("purpose", "supply".into()),
                ("pool", tusd.to_string()),
                ("amount", "1".into()),
            ],
        );
        ensure!(st == 422, "dust ticket: {st}");
        for _ in 0..10 {
            k.step()?;
            sleep(Duration::from_millis(500));
        }
        ensure!(
            probe.builds.lock().unwrap().len() == before,
            "nothing was built"
        );
        Ok(())
    })?;

    // 8. Restart with an empty state file: the chain gives it all back.
    let before = session(&probe, &cfg, &state, |k| k.snapshot())?;
    std::fs::remove_file(&state)?;
    timed("restart from an empty state file", || {
        session(&probe, &cfg, &state, |k| {
            let b = k.snapshot()?;
            ensure!(
                b.reserve.as_ref().map(|r| &r.hash) == before.reserve.as_ref().map(|r| &r.hash),
                "the reserve"
            );
            ensure!(
                hashes(&b.receipts) == hashes(&before.receipts),
                "the receipts"
            );
            ensure!(
                hashes(&b.positions) == hashes(&before.positions),
                "the positions"
            );
            let receipts: Vec<ReceiptState> =
                b.receipts.iter().map(|r| r.state().unwrap()).collect();
            ensure!(
                receipts.iter().all(|r| r.supply_shares > 0),
                "live receipts only"
            );
            Ok(())
        })
    })?;

    // 9. Two keepers, the second a standby with its own funds at its own
    // lend address. The primary fills a supply, a deposit, a top-up and a
    // borrow; it issues a repay ticket, the intent is made, and the primary
    // dies with the repay built and never sent. The standby takes the ticket
    // on from the page and fills the repay, serves a draw on the topped-up
    // position (notes the primary wrote) and a redeem, and liquidates after
    // its grace on its own funds, then sweeps the collateral. The primary,
    // back, ends with the standby's reserve, receipts and positions.
    let probe2 = Probe {
        inner: &second_chain,
        drop_send: AtomicBool::new(false),
        builds: Mutex::new(Vec::new()),
    };
    timed("fund the standby (keeler lend-fund)", || -> Result<()> {
        lend_chain::fund(&second_chain, 20 * USD, fixture::payer_token_account())?;
        let started = Instant::now();
        while lend_chain::balances(&second_chain)?.0 < 20 * USD {
            ensure!(
                started.elapsed() < Duration::from_secs(60),
                "the standby's funds"
            );
            sleep(Duration::from_millis(500));
        }
        Ok(())
    })?;
    let pos = 3;
    let draw_of = |borrow: u64, withdraw: u64| {
        vec![
            ("op", "draw".to_string()),
            ("vault", "1".into()),
            ("position", pos.to_string()),
            ("borrow", borrow.to_string()),
            ("withdraw", withdraw.to_string()),
        ]
    };
    let redeem_of = |amount: u64| {
        vec![
            ("op", "redeem".to_string()),
            ("pool", tusd.to_string()),
            ("amount", amount.to_string()),
        ]
    };
    let (repay_held, repay_id) = session(&probe, &cfg, &state, |k| {
        env.set_price(100);
        let id = env.intent(&lender, purpose::SUPPLY, 1, 4 * USD)?;
        settle(k, "supply (primary)", 180, filled(&id))?;
        let id = env.intent(&borrower, purpose::DEPOSIT, pos, SOL / 2)?;
        settle(k, "deposit (primary)", 180, filled(&id))?;
        let id = env.intent(&borrower, purpose::DEPOSIT, pos, SOL / 4)?;
        settle(k, "top-up (primary)", 180, filled(&id))?;
        env.set_price(100);
        let (st, v, tx) = env.build(&borrower, &draw_of(4 * USD, 0))?;
        ensure!(st == 200, "borrow (primary): {v}");
        env.send(tx.unwrap())?;
        settle(k, "borrow (primary)", 120, |b| {
            b.pending.is_empty() && b.reservation.is_none()
        })?;
        let (st, t) = env.ticket(
            &borrower,
            &[
                ("purpose", "repay".into()),
                ("vault", "1".into()),
                ("position", pos.to_string()),
                ("amount", USD.to_string()),
            ],
        );
        ensure!(st == 200, "repay ticket: {st} {t}");
        ensure!(t["keeper"] == json!(keeler_lend::KEEPERS[0].to_string()));
        let expiry = t["expiry"].as_u64().context("expiry")?;
        env.raw_intent(&borrower, tusd, purpose::REPAY, pos, USD, expiry)?;
        // Killed mid-operation: the repay is built and recorded, never sent.
        probe.drop_send.store(true, Ordering::SeqCst);
        settle(k, "the primary builds the repay", 180, |b| {
            b.pending.iter().any(|p| p.op == Op::Repay)
        })?;
        let held = json!({
            "id": t["id"], "expiry": t["expiry"], "signature": t["signature"],
            "keeper": t["keeper"], "purpose": "repay", "vault": 1,
            "position": pos.to_string(), "amount": USD.to_string(),
        });
        Ok((held, t["id"].as_str().unwrap().to_owned()))
    })?;
    let standby_cfg: Lend = serde_json::from_value(json!({
        "program_id": keeler_lend::ID.to_string(),
        "network": NET,
        "origins": [ORIGIN],
        "pause_file": dir.join("pause"),
        "role": "standby",
        "standby_grace_secs": 10,
        "limits": { "tickets_per_wallet_hour": 100 }
    }))?;
    standby_cfg.check()?;
    let b_state = dir.join("standby.json");
    let standby = timed("standby: take on, repay, draw, redeem, liquidate", || {
        session_as(&probe2, &standby_cfg, &b_state, &second_key, |k| {
            let mut body =
                serde_json::to_value(borrower.signed("POST", "/lend/account", &[], env.now()))?;
            body["tickets"] = json!([repay_held]);
            let (st, v) = env.post_value("/lend/account", &body);
            ensure!(st == 200, "account: {v}");
            ensure!(
                k.snapshot()?
                    .tickets
                    .iter()
                    .any(|t| t.id == repay_id && t.keeper.is_some()),
                "the standby took the primary's ticket on"
            );
            settle(k, "repay (standby)", 180, filled(&repay_id))?;
            // A draw on the position the primary wrote and topped up.
            env.set_price(100);
            let (st, v, tx) = env.build(&borrower, &draw_of(USD / 2, 0))?;
            ensure!(st == 200, "draw (standby): {v}");
            ensure!(v["keeper"] == json!(second_key.pubkey().to_string()));
            env.send(tx.unwrap())?;
            settle(k, "draw (standby)", 120, |b| {
                b.pending.is_empty() && b.reservation.is_none()
            })?;
            let (st, v, tx) = env.build(&lender, &redeem_of(USD))?;
            ensure!(st == 200, "redeem (standby): {v}");
            env.send(tx.unwrap())?;
            settle(k, "redeem (standby)", 120, |b| {
                b.pending.is_empty() && b.reservation.is_none()
            })?;
            // $5: past the threshold; liquidated after the grace, paid from
            // the standby's own lend address.
            let (held, _) = lend_chain::balances(&second_chain)?;
            env.set_price(50);
            let started = Instant::now();
            settle(k, "liquidation (standby)", 180, |b| {
                b.pending.is_empty() && probe2.builds.lock().unwrap().contains(&Op::Liquidate)
            })?;
            ensure!(started.elapsed() >= Duration::from_secs(10), "the grace");
            let started = Instant::now();
            let collateral = loop {
                let (left, collateral) = lend_chain::balances(&second_chain)?;
                if left < held && collateral > 0 {
                    break collateral;
                }
                ensure!(
                    started.elapsed() < Duration::from_secs(60),
                    "the liquidation's funds"
                );
                sleep(Duration::from_millis(500));
            };
            let lamports = second_chain.chain.lamports(second_key.pubkey())?;
            let sigs = timed("sweep (keeler lend-sweep)", || {
                lend_chain::sweep(&second_chain)
            })?;
            ensure!(!sigs.is_empty(), "nothing swept");
            let started = Instant::now();
            while lend_chain::balances(&second_chain)?.1 > 0 {
                ensure!(started.elapsed() < Duration::from_secs(60), "the sweep");
                sleep(Duration::from_millis(500));
            }
            let gained = second_chain.chain.lamports(second_key.pubkey())? - lamports;
            println!("LEND swept {collateral} lamports of collateral; the key gained {gained}");
            ensure!(
                gained > collateral / 2,
                "the collateral reached the standby key"
            );
            k.snapshot()
        })
    })?;
    ensure!(
        probe2.builds.lock().unwrap().first() == Some(&Op::Repay),
        "the standby filled the repay"
    );
    timed("primary: back, ends with the standby's book", || {
        session(&probe, &cfg, &state, |k| {
            settle(k, "the primary follows", 180, |b| {
                b.pending.is_empty()
                    && b.reserve.as_ref().map(|r| &r.hash)
                        == standby.reserve.as_ref().map(|r| &r.hash)
                    && b.tickets
                        .iter()
                        .any(|t| t.id == repay_id && t.status == TicketStatus::Filled)
            })?;
            let b = k.snapshot()?;
            ensure!(
                hashes(&b.receipts) == hashes(&standby.receipts),
                "both keepers hold the same receipts"
            );
            ensure!(
                hashes(&b.positions) == hashes(&standby.positions),
                "both keepers hold the same positions"
            );
            Ok(())
        })
    })?;

    // 10. The real Pyth account. The local validator runs offline, so the
    // mainnet SOL/USD PriceUpdateV2 bytes are loaded with surfpool's
    // `surfnet_setAccount` at the address the program is built with, and a
    // stand-in pusher keeps its publish time at chain time. The keeper reads
    // it with `price: "pyth"`, a draw lands, and a price drop is liquidated.
    let raw = mainnet_pyth_account()?;
    let at = |o: usize| i64::from_le_bytes(raw[o..o + 8].try_into().unwrap());
    let (real, conf) = (at(73), at(81) as u64);
    let expo = i32::from_le_bytes(raw[89..93].try_into()?);
    ensure!(expo == -8, "SOL/USD is quoted at 1e-8, not {expo}");
    println!("LEND mainnet SOL/USD: {real} x 1e-8 +- {conf}");
    let wanted = std::sync::atomic::AtomicI64::new(real);
    let lag = std::sync::atomic::AtomicI64::new(0);
    let pushing = AtomicBool::new(true);
    let rpc_url = ports.rpc_url();
    let pyth_cfg: Lend = serde_json::from_value(json!({
        "program_id": keeler_lend::ID.to_string(),
        "network": NET,
        "origins": [ORIGIN],
        "pause_file": dir.join("pause"),
        "price": "pyth",
        "limits": { "tickets_per_wallet_hour": 100 }
    }))?;
    pyth_cfg.check()?;
    let pyth_probe = Probe {
        inner: &pyth_chain,
        drop_send: AtomicBool::new(false),
        builds: Mutex::new(Vec::new()),
    };
    thread::scope(|s| {
        s.spawn(|| {
            while pushing.load(Ordering::Relaxed) {
                let (price, behind) = (wanted.load(Ordering::Relaxed), lag.load(Ordering::Relaxed));
                if let Err(e) = push_pyth(&rpc_url, &raw, price, behind) {
                    println!("pyth push: {e:#}");
                }
                sleep(Duration::from_secs(5));
            }
        });
        let out = session(&pyth_probe, &pyth_cfg, &state, |k| {
            sleep(Duration::from_secs(1));
            // The chain layer: 1e-8 USD per SOL is 10 tUSD base units per
            // lamport x 1e12.
            let q = pyth_chain.quote(1)?;
            ensure!(
                q.low == (real as u64 - conf) * 10 && q.high == (real as u64 + conf) * 10,
                "quote {q:?}"
            );
            ensure!(env.now().abs_diff(q.published as u64) <= 10, "fresh");
            let pos = 2;
            // 0.2 SOL at the real price, 4 tUSD drawn.
            let id = env.intent(&borrower, purpose::DEPOSIT, pos, SOL / 5)?;
            settle(k, "deposit (pyth)", 180, filled(&id))?;
            let draw = vec![
                ("op", "draw".to_string()),
                ("vault", "1".into()),
                ("position", pos.to_string()),
                ("borrow", (4 * USD).to_string()),
                ("withdraw", "0".into()),
            ];
            // Left unrefreshed past the keeper's 65 s: the draw is refused.
            lag.store(70, Ordering::Relaxed);
            sleep(Duration::from_secs(7));
            let (st, v, _) = env.build(&borrower, &draw)?;
            ensure!(st == 412, "a draw on a stale Pyth price: {st} {v}");
            lag.store(0, Ordering::Relaxed);
            sleep(Duration::from_secs(7));
            let (st, v, tx) = timed("draw build on the Pyth price", || {
                env.build(&borrower, &draw)
            })?;
            ensure!(st == 200, "draw: {v}");
            let bound: u64 = v["bound"].as_str().context("bound")?.parse()?;
            ensure!(bound < q.low && bound > q.low / 100 * 99, "bound {bound}");
            env.send(tx.unwrap())?;
            settle(k, "draw resolved (pyth)", 120, |b| b.pending.is_empty())?;
            // $15: 0.2 SOL is worth $3 against $4 of debt. The liquidation
            // takes the collateral; what it cannot cover is bad debt, which
            // halts borrowing.
            wanted.store(15_0000_0000, Ordering::Relaxed);
            timed("liquidation on the Pyth price", || {
                settle(k, "liquidation (pyth)", 180, |b| {
                    b.pending.is_empty()
                        && pyth_probe.builds.lock().unwrap().contains(&Op::Liquidate)
                })
            })?;
            let owed = debt(k, &borrower.addr(), pos, env.now());
            ensure!(owed < 2 * USD, "liquidated: owed {owed}");
            ensure!(env.build(&borrower, &draw)?.0 == 403, "borrowing halts");
            Ok(())
        });
        pushing.store(false, Ordering::Relaxed);
        out
    })?;
    println!("LEND times:");
    for (name, t) in TIMES.lock().unwrap().iter() {
        println!("  {name}: {:.1} s", t.as_secs_f64());
    }
    Ok(())
}
