//! The local stack for the lend page's real browser test
//! (`web/tests/e2e/lend-real.spec.ts`, started by `scripts/lend-web-stack.sh`).
//!
//! `web_stack <offset> <dir> <port>` boots a local network with the lend
//! program and the user registry on the default ports plus `offset`,
//! registers the keeper, writes `<dir>/stack.json` and then serves on
//! `127.0.0.1:<port>`, until killed (the network stops with it):
//! - as the keeper, behind the Worker (no session checks: a test stub):
//!   `GET /lend/pools`, `GET /lend/account`, `POST /lend/ticket`;
//! - for the test: `POST /test/fund` gives the test wallet public SOL and
//!   private SOL and tUSD notes; `POST /test/open` has the test wallet make a
//!   deposit intent and the keeper open a debt-free position from it
//!   (`deposit_open`), which `/lend/account` then reports.
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
};

use anyhow::{bail, Context, Result};
use keeler_lend::circuits::{DEPOSIT_POSITION_SLOT, INTENT_SLOT};
use keeler_lend_sdk::{builders, purpose, user_pda, vault, KEEPER};
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
use wallet::{deposit, send, wait_for_merkle_proof, TestWallet};

/// The browser test wallet's Ed25519 seed (the page reads it from localStorage).
const WALLET_SEED: [u8; 32] = *b"keeler lend web test wallet seed";
const SOL: u64 = 1_000_000_000;
const USD: u64 = 1_000_000;
const VAULT: u64 = 1;
/// The ticket's intent lifetime: the page takes 60 s to 900 s; the refund test waits it out.
const TICKET_SECS: u64 = 65;

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
    keeper_key: Keypair,
    keeper: ShieldedKeypair,
    user_key: Keypair,
    user: ShieldedKeypair,
    tusd: Address,
    /// `/lend/account` positions, as the keeper reports them.
    positions: Vec<Value>,
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

    /// 1 SOL public (fees), 2 SOL and 50 tUSD private, to the test wallet.
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
        let to = self.user.shielded_address()?;
        deposit(
            &self.net,
            &payer,
            SprProgramTest::wallet_sol_shield_data(2 * SOL, &to)?,
        )?;
        deposit(
            &self.net,
            &payer,
            SprProgramTest::wallet_spl_shield_data(
                50 * USD,
                &to,
                &self.tusd,
                &fixture::payer_token_account(),
            )?,
        )?;
        Ok(json!({"wallet": self.user_key.pubkey().to_string()}))
    }

    /// The test wallet deposits 0.5 SOL into a new position; the keeper opens it.
    fn open(&mut self) -> Result<Value> {
        let (tree, tree_id) = (self.net.tree, self.net.tree_id);
        let indexer = self.net.client.indexer();
        let wallet = TestWallet::scan(self.user.clone(), self.assets()?, &self.net.client)?;
        let sol = Address::default();
        let source = wallet
            .spendable_utxo(sol, SOL / 2)
            .context("no SOL note of 0.5")?;
        let position_id = 7 + self.positions.len() as u64;
        let p = builders::intent(
            &self.user,
            source,
            &sol,
            SOL / 2,
            purpose::DEPOSIT,
            position_id,
            self.now()? + 600,
            self.keeper.viewing_pubkey(),
            tree_id,
        )?;
        let out = p.outputs[INTENT_SLOT].clone();
        let ix = p.prove(indexer, tree, &self.user)?;
        let sig = send(self.net.client.rpc(), &[&self.user_key], ix)?;
        self.net
            .client
            .confirm_private_transaction_sync(sig)
            .map_err(err)?;
        let pda = user_pda(&self.user_key.pubkey()).owner;
        let leaf =
            wait_for_merkle_proof(indexer, tree, out.hash(tree_id).map_err(err)?)?.leaf_index;
        let intent = pda.input(&out, tree_id, leaf).map_err(err)?;

        let owner = self.user.shielded_address()?;
        let p = vault::deposit_open(&self.keeper, &owner, intent, VAULT, self.now()?, tree_id)?;
        let position = p.outputs[DEPOSIT_POSITION_SLOT].clone();
        let ix = p.prove(indexer, tree, &self.keeper)?;
        let sig = send(self.net.client.rpc(), &[&self.keeper_key], ix)?;
        self.net
            .client
            .confirm_private_transaction_sync(sig)
            .map_err(err)?;
        wait_for_merkle_proof(indexer, tree, position.hash(tree_id).map_err(err)?)?;
        // The keeper's account view: amount u64 LE ‖ blinding ‖ the Borsh state.
        let state = position
            .data
            .utxo_data()
            .context("the position has no state")?;
        let mut message = position.amount.to_le_bytes().to_vec();
        message.extend(position.blinding);
        message.extend(state);
        let entry = json!({
            "vault": VAULT,
            "position": position_id.to_string(),
            "collateral": position.amount.to_string(),
            "debtShares": "0",
            "message": base64(&message),
        });
        self.positions.push(entry.clone());
        Ok(json!({"position": entry, "signature": sig.to_string()}))
    }

    /// A keeper ticket for the page's intent (docs/lend-api.md, "Ticket bytes").
    fn ticket(&self, body: &Value) -> Result<Value> {
        static COUNT: AtomicU64 = AtomicU64::new(0);
        let s = |k: &str| body[k].as_str().with_context(|| format!("no {k}"));
        let wallet: Address = s("wallet")?.parse().map_err(err)?;
        let amount: u64 = s("amount")?.parse()?;
        let (purpose, target) = match s("purpose")? {
            "supply" => (1u8, keeler_lend::pool::TUSD),
            "deposit" => (2, s("position")?.parse()?),
            "repay" => (3, s("position")?.parse()?),
            p => bail!("purpose {p}"),
        };
        let expiry = self.now()? + TICKET_SECS;
        let n = COUNT.fetch_add(1, Ordering::Relaxed);
        let id: [u8; 16] = Sha256::digest(
            [
                wallet.to_bytes().as_slice(),
                &n.to_le_bytes(),
                &expiry.to_le_bytes(),
            ]
            .concat(),
        )[..16]
            .try_into()?;
        let mut bytes = b"keeler-lend-ticket-v1".to_vec();
        bytes.extend(keeler_lend_sdk::ID.to_bytes());
        bytes.extend(wallet.to_bytes());
        bytes.extend(id);
        bytes.push(purpose);
        bytes.extend(target.to_le_bytes());
        bytes.extend(amount.to_le_bytes());
        bytes.extend(expiry.to_le_bytes());
        let signature = self.keeper_key.sign_message(&bytes);
        Ok(json!({"id": hex(&id), "expiry": expiry, "signature": hex(signature.as_ref())}))
    }

    fn route(&mut self, method: &str, path: &str, body: &Value) -> Result<Value> {
        match (method, path.split('?').next().unwrap_or("")) {
            ("GET", "/health") => Ok(json!({"ok": true})),
            ("GET", "/lend/pools") => Ok(json!({"pools": []})),
            ("GET", "/lend/account") => {
                Ok(json!({"receipts": [], "positions": self.positions, "intents": []}))
            }
            ("POST", "/lend/ticket") => self.ticket(body),
            ("POST", "/test/fund") => self.fund(),
            ("POST", "/test/open") => self.open(),
            ("POST", "/test/stop") => {
                self.stop = true;
                Ok(json!({"stopping": true}))
            }
            _ => bail!("no route {method} {path}"),
        }
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn base64(b: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in b.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            out.push(if i <= c.len() {
                A[(n >> (18 - 6 * i) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
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
    anyhow::ensure!(
        keeper_key.pubkey() == KEEPER,
        "build the program with the default keeper"
    );
    let keeper = ShieldedKeypair::from_keypair(&keeper_key)?;
    user_registry::ensure_registered(net.client.rpc(), &keeper_key, &keeper).map_err(err)?;
    let user_key = Keypair::new_from_array(WALLET_SEED);
    let user = ShieldedKeypair::from_keypair(&user_key)?;
    let tusd = fixture::spl_mint();
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let prover =
        std::env::var("ZOLANA_PROVER_URL").unwrap_or_else(|_| "http://127.0.0.1:3001".into());
    let info = json!({
        "rpcUrl": ports.rpc_url(),
        "indexerUrl": ports.photon_url(),
        "proverUrl": prover,
        "programId": keeler_lend_sdk::ID.to_string(),
        "keeper": KEEPER.to_string(),
        "keeperUrl": format!("http://127.0.0.1:{port}"),
        "tusd": tusd.to_string(),
        "walletSeed": hex(&WALLET_SEED),
        "wallet": user_key.pubkey().to_string(),
    });
    std::fs::write(
        dir.join("stack.json"),
        serde_json::to_string_pretty(&info)? + "\n",
    )?;
    eprintln!("lend web stack up: {info}");
    let mut stack = Stack {
        net,
        keeper_key,
        keeper,
        user_key,
        user,
        tusd,
        positions: Vec::new(),
        stop: false,
    };
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
