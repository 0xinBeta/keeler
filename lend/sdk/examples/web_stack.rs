//! The local network for the lend page's real browser test
//! (`web/tests/e2e/lend-real.spec.ts`, started by `scripts/lend-web-stack.sh`),
//! and the few things around it that a keeper does not do. The keeper itself
//! is the real `keeler keeper`, which the script runs next to this.
//!
//! `web_stack <offset> <dir> <port>` boots a local network with the lend
//! program and the user registry on the default ports plus `offset`,
//! registers the keeper and gives it private tUSD for liquidations, writes the
//! fixture keys the keeper's config names (`<dir>/keeper-key.json`,
//! `<dir>/admin-key.json`) and `<dir>/stack.json`, then serves on
//! `127.0.0.1:<port>` until `POST /test/stop` (the network stops with it):
//! - `GET /health`;
//! - `POST /test/fund`: public SOL (fees) and private SOL and tUSD notes to
//!   the browser test wallet;
//! - `POST /test/seed` `{keeperUrl, key, amount}`: a second wallet (the
//!   lender) supplies `amount` tUSD through the keeper's own routes, as the
//!   page does, and the call returns once the keeper has filled it.
//!
//! Every key is a fixture test key, and the browser wallet's seed is fixed:
//! local network only.
#[path = "../tests/wallet.rs"]
mod wallet;

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    thread::sleep,
    time::{Duration, Instant},
};

use anyhow::{bail, ensure, Context, Result};
use keeler_lend_sdk::{builders, purpose, KEEPER};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use solana_address::Address;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_signer::Signer;
use spr_client::{user_registry, Rpc};
use spr_keypair::ShieldedKeypair;
use spr_program_test::{
    fixture,
    localnet::{FixtureLocalnet, LocalnetPaths, LocalnetPorts},
    SprProgramTest,
};
use spr_transaction::AssetRegistry;
use wallet::{deposit, send, TestWallet};

/// The browser test wallet's Ed25519 seed (the page reads it from localStorage).
const WALLET_SEED: [u8; 32] = *b"keeler lend web test wallet seed";
/// The lender's session key seed (any fixed key will do).
const LENDER_SESSION: [u8; 32] = *b"keeler lend web lender session k";
/// The session text's network name: the Worker's NETWORK, the keeper's `lend.network`.
const NETWORK: &str = "local";
const SOL: u64 = 1_000_000_000;
const USD: u64 = 1_000_000;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn err(e: impl std::fmt::Debug) -> anyhow::Error {
    anyhow::anyhow!("{e:?}")
}

fn env_path(name: &str) -> Result<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .with_context(|| format!("{name} is not set; source .tools/env"))
}

struct Stack {
    net: FixtureLocalnet,
    keeper: ShieldedKeypair,
    user_key: Keypair,
    user: ShieldedKeypair,
    tusd: Address,
    /// Set by `POST /test/stop`: the loop ends and dropping the network stops it.
    stop: bool,
}

impl Stack {
    fn now(&self) -> Result<u64> {
        let clock: Address = "SysvarC1ock11111111111111111111111111111111"
            .parse()
            .map_err(err)?;
        let account = self
            .net
            .client
            .rpc()
            .get_account(clock)?
            .context("no clock")?;
        let bytes: [u8; 8] = account.data[32..40].try_into()?;
        Ok(u64::try_from(i64::from_le_bytes(bytes))?)
    }

    fn assets(&self) -> Result<AssetRegistry> {
        let mut assets = AssetRegistry::default();
        assets.insert(fixture::SPL_ASSET_ID, self.tusd)?;
        Ok(assets)
    }

    fn shield(&self, to: &ShieldedKeypair, sol: u64, usd: u64) -> Result<()> {
        let payer = fixture::payer();
        let to = to.shielded_address()?;
        if sol > 0 {
            deposit(
                &self.net,
                &payer,
                SprProgramTest::wallet_sol_shield_data(sol, &to)?,
            )?;
        }
        if usd > 0 {
            deposit(
                &self.net,
                &payer,
                SprProgramTest::wallet_spl_shield_data(
                    usd,
                    &to,
                    &self.tusd,
                    &fixture::payer_token_account(),
                )?,
            )?;
        }
        Ok(())
    }

    /// 1 SOL public (fees), 3 SOL and 50 tUSD private, to the test wallet.
    fn fund(&self) -> Result<Value> {
        let payer = fixture::payer();
        let mut data = vec![2, 0, 0, 0];
        data.extend(SOL.to_le_bytes());
        let transfer = Instruction {
            program_id: Address::default(),
            accounts: vec![
                AccountMeta::new(payer.pubkey(), true),
                AccountMeta::new(self.user_key.pubkey(), false),
            ],
            data,
        };
        send(self.net.client.rpc(), &[&payer], transfer)?;
        self.shield(&self.user, 3 * SOL, 50 * USD)?;
        Ok(json!({"wallet": self.user_key.pubkey().to_string()}))
    }

    /// The lender supplies `amount` tUSD through the keeper's routes, signed as
    /// the page signs them (docs/lend-api.md), and waits for the fill.
    fn seed(&self, body: &Value) -> Result<Value> {
        let url = body["keeperUrl"].as_str().context("no keeperUrl")?;
        let key = body["key"].as_str().context("no key")?;
        let amount: u64 = body["amount"].as_str().context("no amount")?.parse()?;
        let lender_key = fixture::actor(2);
        let lender = ShieldedKeypair::from_keypair(&lender_key)?;
        user_registry::ensure_registered(self.net.client.rpc(), &lender_key, &lender)
            .map_err(err)?;
        self.shield(&lender, 0, amount)?;
        let session = Session::new(&lender_key, self.now()?);
        let fields = session.sign(
            "POST",
            "/lend/ticket",
            &[
                ("purpose", "supply".into()),
                ("pool", self.tusd.to_string()),
                ("amount", amount.to_string()),
            ],
            self.now()?,
        );
        let (status, t) = http(url, "POST", "/lend/ticket", key, Some(&json!(fields)))?;
        ensure!(status == 200, "ticket: {status} {t}");
        let expiry = t["expiry"].as_u64().context("ticket expiry")?;

        let wallet = TestWallet::scan(lender.clone(), self.assets()?, &self.net.client)?;
        let source = wallet
            .spendable_utxo(self.tusd, amount)
            .context("the lender has no note to supply")?;
        let p = builders::intent(
            &lender,
            source,
            &self.tusd,
            amount,
            purpose::SUPPLY,
            keeler_lend::pool::TUSD,
            expiry,
            self.keeper.viewing_pubkey(),
            self.net.tree_id,
        )?;
        let ix = p.prove(self.net.client.indexer(), self.net.tree, &lender)?;
        let sig = send(self.net.client.rpc(), &[&lender_key], ix)?;
        self.net
            .client
            .confirm_private_transaction_sync(sig)
            .map_err(err)?;

        let started = Instant::now();
        loop {
            let f = session.sign("GET", "/lend/account", &[], self.now()?);
            let q: Vec<String> = f.iter().map(|(k, v)| format!("{k}={v}")).collect();
            let path = format!("/lend/account?{}", q.join("&"));
            let (status, v) = http(url, "GET", &path, key, None)?;
            ensure!(status == 200, "account: {status} {v}");
            if v["receipts"].as_array().is_some_and(|r| !r.is_empty()) {
                return Ok(json!({"receipts": v["receipts"], "intent": sig.to_string()}));
            }
            ensure!(
                started.elapsed() < Duration::from_secs(300),
                "the keeper did not fill the lender's supply in 300 s"
            );
            sleep(Duration::from_secs(2));
        }
    }

    fn route(&mut self, method: &str, path: &str, body: &Value) -> Result<Value> {
        match (method, path.split('?').next().unwrap_or("")) {
            ("GET", "/health") => Ok(json!({"ok": true})),
            ("POST", "/test/fund") => self.fund(),
            ("POST", "/test/seed") => self.seed(body),
            ("POST", "/test/stop") => {
                self.stop = true;
                Ok(json!({"stopping": true}))
            }
            _ => bail!("no route {method} {path}"),
        }
    }
}

/// A wallet's lending session: the wallet's authorisation of a session key,
/// and that key's signature on each request (docs/lend-api.md).
struct Session<'a> {
    wallet: &'a Keypair,
    key: Keypair,
    expiry: u64,
}

impl<'a> Session<'a> {
    fn new(wallet: &'a Keypair, now: u64) -> Self {
        Self {
            wallet,
            key: Keypair::new_from_array(LENDER_SESSION),
            expiry: now + 3_600,
        }
    }

    fn sign(
        &self,
        method: &str,
        path: &str,
        extra: &[(&str, String)],
        timestamp: u64,
    ) -> std::collections::BTreeMap<String, String> {
        static COUNT: AtomicU64 = AtomicU64::new(0);
        let mut f = std::collections::BTreeMap::new();
        f.insert("wallet".to_owned(), self.wallet.pubkey().to_string());
        f.insert("sessionKey".to_owned(), self.key.pubkey().to_string());
        f.insert("sessionExpiry".to_owned(), self.expiry.to_string());
        let text = format!(
            "Keeler Lend: allow this browser session\n\nThis lets this browser send lending requests for this wallet to the Keeler keeper until the time below, without asking you each time. It cannot move funds: every transaction still needs your wallet's approval.\n\nNetwork: {NETWORK}\nProgram: {}\nWallet: {}\nSession key: {}\nExpires: {} ({})",
            keeler_lend_sdk::ID,
            self.wallet.pubkey(),
            self.key.pubkey(),
            self.expiry,
            iso8601(self.expiry)
        );
        f.insert(
            "sessionAuth".to_owned(),
            hex(self.wallet.sign_message(text.as_bytes()).as_ref()),
        );
        for (k, v) in extra {
            f.insert((*k).to_owned(), v.clone());
        }
        let n = COUNT.fetch_add(1, Ordering::Relaxed);
        let nonce: [u8; 16] =
            Sha256::digest([&n.to_le_bytes()[..], &timestamp.to_le_bytes()].concat())[..16]
                .try_into()
                .expect("16 bytes");
        let canonical: String = f.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
        let mut m = format!("keeler-lend-request-v1\n{method}\n{path}\n").into_bytes();
        m.extend(nonce);
        m.extend(timestamp.to_le_bytes());
        m.extend(Sha256::digest(canonical.as_bytes()));
        f.insert("nonce".to_owned(), hex(&nonce));
        f.insert("timestamp".to_owned(), timestamp.to_string());
        f.insert(
            "signature".to_owned(),
            hex(self.key.sign_message(&m).as_ref()),
        );
        f
    }
}

/// Unix seconds as ISO 8601 UTC to the second (the keeper's `lend_auth::iso8601`).
fn iso8601(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// One request to the keeper's HTTP server: (status, JSON reply).
fn http(
    base: &str,
    method: &str,
    path: &str,
    key: &str,
    body: Option<&Value>,
) -> Result<(u16, Value)> {
    let host = base.strip_prefix("http://").context("an http:// URL")?;
    let mut s = TcpStream::connect(host)?;
    s.set_read_timeout(Some(Duration::from_secs(60)))?;
    let body = body.map(Value::to_string).unwrap_or_default();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nhost: {host}\r\nx-keeler-key: {key}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )?;
    let mut reply = Vec::new();
    s.read_to_end(&mut reply)?;
    let text = String::from_utf8_lossy(&reply);
    let (head, rest) = text.split_once("\r\n\r\n").context("no HTTP reply")?;
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .context("no status")?
        .parse()?;
    Ok((status, serde_json::from_str(rest).unwrap_or(Value::Null)))
}

/// One HTTP/1.1 request and its JSON reply; a failed route is a 500 with `{error}`.
fn serve(stack: &mut Stack, stream: TcpStream) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (method, path) = (
        parts.next().unwrap_or("").to_owned(),
        parts.next().unwrap_or("").to_owned(),
    );
    let mut length = 0;
    loop {
        let mut h = String::new();
        reader.read_line(&mut h)?;
        if h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                length = v.trim().parse()?;
            }
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let body: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)?
    };
    let (status, reply) = match stack.route(&method, &path, &body) {
        Ok(v) => ("200 OK", v),
        Err(e) => {
            eprintln!("{method} {path}: {e:#}");
            (
                "500 Internal Server Error",
                json!({"error": format!("{e:#}")}),
            )
        }
    };
    let text = reply.to_string();
    let mut out = stream;
    write!(out, "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}", text.len())?;
    Ok(())
}

/// A keypair as the JSON byte array a `{"file": ...}` key source reads.
fn write_key(path: &Path, key: &Keypair) -> Result<()> {
    std::fs::write(path, serde_json::to_string(&key.to_bytes().to_vec())?)?;
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let [_, offset, dir, port] = args.as_slice() else {
        bail!("usage: web_stack <offset> <dir> <port>");
    };
    let (offset, dir, port): (u16, PathBuf, u16) = (offset.parse()?, dir.into(), port.parse()?);
    std::fs::create_dir_all(&dir)?;
    let ports = LocalnetPorts::offset(offset);
    let net = FixtureLocalnet::start(
        "keeler-lend-web",
        ports,
        vec![
            (
                keeler_lend_sdk::ID,
                root().join("target/deploy/keeler_lend.so"),
            ),
            (
                spr_user_registry_program(),
                env_path("USER_REGISTRY_PROGRAM_PATH")?,
            ),
        ],
        &LocalnetPaths {
            proving_keys_dir: root().join("target/proving-keys"),
            scratch_dir: dir.clone(),
            ..LocalnetPaths::workspace()
        },
    )?;
    let keeper_key = fixture::actor(0);
    let admin_key = fixture::actor(3);
    ensure!(
        keeper_key.pubkey() == KEEPER && admin_key.pubkey() == keeler_lend::ADMIN,
        "build the program with the default keeper and admin"
    );
    let keeper = ShieldedKeypair::from_keypair(&keeper_key)?;
    user_registry::ensure_registered(net.client.rpc(), &keeper_key, &keeper).map_err(err)?;
    write_key(&dir.join("keeper-key.json"), &keeper_key)?;
    write_key(&dir.join("admin-key.json"), &admin_key)?;
    let user_key = Keypair::new_from_array(WALLET_SEED);
    let user = ShieldedKeypair::from_keypair(&user_key)?;
    let tusd = fixture::spl_mint();
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let stack = Stack {
        net,
        keeper,
        user_key,
        user,
        tusd,
        stop: false,
    };
    // The keeper repays liquidated debt from its own private tUSD.
    stack.shield(&stack.keeper, 0, 30 * USD)?;
    let prover =
        std::env::var("ZOLANA_PROVER_URL").unwrap_or_else(|_| "http://127.0.0.1:3001".into());
    let info = json!({
        "rpcUrl": ports.rpc_url(),
        "indexerUrl": ports.photon_url(),
        "proverUrl": prover,
        "programId": keeler_lend_sdk::ID.to_string(),
        "keeper": KEEPER.to_string(),
        "helperUrl": format!("http://127.0.0.1:{port}"),
        "tusd": tusd.to_string(),
        "walletSeed": hex(&WALLET_SEED),
        "wallet": stack.user_key.pubkey().to_string(),
    });
    std::fs::write(
        dir.join("stack.json"),
        serde_json::to_string_pretty(&info)? + "\n",
    )?;
    eprintln!("lend web network up: {info}");
    let mut stack = stack;
    for stream in listener.incoming() {
        if let Err(e) = stream
            .map_err(anyhow::Error::from)
            .and_then(|s| serve(&mut stack, s))
        {
            eprintln!("request: {e:#}");
        }
        if stack.stop {
            break;
        }
    }
    Ok(())
}

/// The SPR user registry (spr-user-registry-interface's `user_registry_program_id`).
fn spr_user_registry_program() -> Address {
    "regy3WAeRXMYuMYxr5iK3Bf9Arr3PcxGxyzAddbiqDQ"
        .parse()
        .expect("registry address")
}
