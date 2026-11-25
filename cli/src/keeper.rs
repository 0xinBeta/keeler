//! The Keeler Lend keeper (`docs/plan-lend.md`, "The keeper";
//! `docs/lend-keeper.md`): it holds the pool's note openings, issues signed
//! tickets, fills ticketed intents, builds owner-signed `redeem` and `draw`,
//! and liquidates. Every built transaction is recorded in the state file
//! before it is sent or handed out, keyed by the notes it spends, and only the
//! chain says which of several builds on one reserve landed.
//!
//! Everything chain-facing goes through [`Ledger`], so the HTTP routes,
//! admission, queue, reservation, pause and records are tested without one.
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, MutexGuard, OnceLock,
    },
    thread::sleep,
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use borsh::BorshDeserialize;
use keeler_core::lend as core;
use keeler_lend_sdk::{purpose, PositionState, ReceiptState, ReserveState};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use solana_address::Address;
use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::{
    config::{parse_address, KeySource},
    lend_auth::{self, hex_bytes, to_hex, Nonces},
};

/// The program refuses a `now` more than this behind the clock, so a build
/// not landed by `now + DEAD_AFTER` never will.
const DEAD_AFTER: u64 = keeler_lend::NOW_WINDOW as u64 + 5;
/// An intent this close to its expiry is left for the maker to refund.
const FILL_MARGIN: u64 = 15;
/// The Worker shows at most this many receipts, positions and intents.
const MAX_LISTED: usize = 8;
/// Failed builds of one ticket before it is given up (the maker refunds);
/// repays and deposits into a position are never given up.
const MAX_ATTEMPTS: u32 = 5;
/// A failed fill waits `RETRY_SECS << attempts` before the next try, so a
/// short outage gives up nothing.
const RETRY_SECS: u64 = 5;
/// How long an HTTP build waits for the lane before answering busy.
const LANE_WAIT: Duration = Duration::from_secs(5);
/// A build done later than this after its request is not handed out: the
/// Worker stopped waiting at 30 s.
const BUILD_DEADLINE: Duration = Duration::from_secs(25);
/// A price older than this is not used for a draw (the program allows 90 s;
/// the Pyth push account updates about once a minute).
const PRICE_FRESH: u64 = 65;
/// Nor for a liquidation, which must land before the price turns 90 s old.
const PRICE_LIQUIDATE: u64 = 80;
/// A liquidation that could not be built or sent waits this long.
const LIQUIDATION_BACKOFF: u64 = 60;
/// The keeper's own debt-token balance is checked this often.
const FUNDS_CHECK_SECS: u64 = 60;
/// After a position of a wallet left the book with no successor in it, no
/// new position of that id is opened for this long: the peer's note may
/// still be on its way.
const DROPPED_SECS: u64 = 120;
/// Indexer pages of one wallet's intents read per pass.
const INTENT_PAGES: usize = 4;
const HOUR: u64 = 3_600;

pub const KIND_RESERVE: u8 = 1;
pub const KIND_RECEIPT: u8 = 2;
pub const KIND_POSITION: u8 = 3;
pub const KIND_INTENT: u8 = 4;

// ---- config ----

/// Where the collateral price comes from.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LendPrice {
    /// The vault's test price account (local networks and devnet).
    #[default]
    Test,
    /// The vault's Pyth account compiled into the program, read over RPC.
    Pyth,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LendLimits {
    /// Smallest supply ticket (the program's minimum is the floor).
    pub min_supply: u64,
    /// Smallest deposit that opens a position (the vault's minimum is the floor).
    pub min_collateral: u64,
    /// Smallest deposit into an existing position.
    pub min_deposit_add: u64,
    pub min_repay: u64,
    /// Smallest redeem, unless it empties the receipt.
    pub min_redeem: u64,
    /// Smallest borrow.
    pub min_borrow: u64,
    /// Smallest collateral withdrawal, unless it empties the position.
    pub min_withdraw: u64,
    pub tickets_per_wallet_hour: u32,
    /// Open tickets of one wallet.
    pub open_per_wallet: u32,
    /// Open tickets across all wallets whose intent is on chain, repays and
    /// deposits into a position not counted.
    pub max_open_intents: u32,
    /// `/lend/build` requests of one wallet that reach the pool, per hour.
    pub builds_per_wallet_hour: u32,
    pub reservation_secs: u64,
    /// Ticket and intent lifetime; the page accepts 60 to 900 s.
    pub ticket_secs: u64,
    /// Pool figures are refreshed at most once per bucket.
    pub bucket_secs: u64,
    /// The draw bound sits this far under `price - confidence`, the
    /// liquidation bound this far over `price + confidence`.
    pub bound_margin_bps: u64,
    /// Caps, 0 for none (mainnet refuses 0). The pool's supplied total,
    /// open supply tickets included, at ticket time; the total alone at fill.
    pub max_pool_size: u64,
    /// Collateral across every position, open deposit tickets included.
    pub max_total_collateral: u64,
    pub max_position_collateral: u64,
    pub max_position_debt: u64,
    /// One wallet's receipts' worth plus its open supply tickets.
    pub max_supply_per_wallet: u64,
    /// A draw leaves no debt or at least this.
    pub min_position_debt: u64,
}

impl Default for LendLimits {
    fn default() -> Self {
        Self {
            min_supply: 0,
            min_collateral: 0,
            min_deposit_add: 1,
            min_repay: 10_000,
            min_redeem: 10_000,
            min_borrow: 10_000,
            min_withdraw: 10_000_000,
            tickets_per_wallet_hour: 30,
            open_per_wallet: 4,
            max_open_intents: 64,
            builds_per_wallet_hour: 60,
            reservation_secs: 20,
            ticket_secs: 600,
            bucket_secs: 600,
            bound_margin_bps: 50,
            max_pool_size: 0,
            max_total_collateral: 0,
            max_position_collateral: 0,
            max_position_debt: 0,
            max_supply_per_wallet: 0,
            min_position_debt: 0,
        }
    }
}

/// Whether `total` passes the cap `max` (0: none).
fn within(max: u64, total: u128) -> bool {
    max == 0 || total <= u128::from(max)
}

/// A keeper's part in a set of keepers.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    #[default]
    Primary,
    /// Liquidates only after `standby_grace_secs`, and re-posts the test
    /// price only well after the primary would have.
    Standby,
}

/// The `lend` section of a dealer config, or of a keeper config.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lend {
    /// The lend program; must be the one this binary was built for.
    pub program_id: String,
    /// The name in the session text the wallet signs: `devnet`, `localnet`.
    pub network: String,
    /// The keeper key (one of the program's KEEPERS); default: the config's key.
    #[serde(default)]
    pub key: Option<KeySource>,
    /// The pool key: the pool viewing key derives from it and must be the
    /// program's POOL_VIEWING. Default: the keeper key.
    #[serde(default)]
    pub pool_key: Option<KeySource>,
    #[serde(default)]
    pub role: Role,
    /// A standby liquidates a position past the threshold this long; default 45.
    #[serde(default)]
    pub standby_grace_secs: Option<u64>,
    /// The page origins a session may name (`Origin:` in its text).
    #[serde(default)]
    pub origins: Vec<String>,
    /// Micro-lamports per compute unit on liquidations; default
    /// `network.priority_fee`.
    #[serde(default)]
    pub liquidation_priority_fee: Option<u64>,
    /// The admin key, only for `keeler lend-init-reserve`.
    #[serde(default)]
    pub admin_key: Option<KeySource>,
    /// Default `~/.local/state/keeler/keeper-<pubkey>.json`. A leading
    /// `~/` is the home directory, here and in `pause_file`.
    #[serde(default, deserialize_with = "home_path")]
    pub state_file: Option<PathBuf>,
    /// While it exists: no supply, no new positions, no borrowing.
    #[serde(default, deserialize_with = "home_path")]
    pub pause_file: Option<PathBuf>,
    #[serde(default)]
    pub price: LendPrice,
    #[serde(default)]
    pub limits: LendLimits,
    /// Seconds between passes of the queue; default 2.
    #[serde(default)]
    pub poll_secs: Option<u64>,
    /// With the test price: re-post it at least this often (the program
    /// refuses one older than 90 s); default 45.
    #[serde(default)]
    pub price_refresh_secs: Option<u64>,
}

impl Lend {
    pub fn check(&self) -> Result<()> {
        ensure!(
            self.program_id == keeler_lend::ID.to_string(),
            "this binary is built for lend program {}, the config is for {}; rebuild with \
             KEELER_LEND_PROGRAM_ID set",
            keeler_lend::ID,
            self.program_id
        );
        ensure!(
            !self.network.is_empty()
                && self
                    .network
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "lend.network must be a plain lowercase name"
        );
        ensure!(
            (1..=60).contains(&self.poll_secs.unwrap_or(2)),
            "lend.poll_secs must be within 1..=60"
        );
        let l = &self.limits;
        ensure!(
            (120..=900).contains(&l.ticket_secs),
            "lend.limits.ticket_secs must be within 120..=900"
        );
        ensure!(
            (5..=120).contains(&l.reservation_secs),
            "lend.limits.reservation_secs must be within 5..=120"
        );
        ensure!(
            l.bucket_secs >= 60,
            "lend.limits.bucket_secs must be at least 60"
        );
        ensure!(
            l.bound_margin_bps < 1_000,
            "lend.limits.bound_margin_bps must be below 1000"
        );
        ensure!(
            self.price == LendPrice::Pyth || !self.mainnet(),
            "the test price is for local networks and devnet"
        );
        ensure!(
            (5..=80).contains(&self.price_refresh()),
            "lend.price_refresh_secs must be within 5..=80"
        );
        ensure!(
            self.grace() <= 600,
            "lend.standby_grace_secs must be at most 600"
        );
        ensure!(
            !self.origins.is_empty() && self.origins.iter().all(|o| origin_ok(o)),
            "lend.origins must list the page origins, each like https://host or http://127.0.0.1:5300"
        );
        ensure!(
            self.liquidation_priority_fee
                .is_none_or(|f| f <= crate::config::MAX_PRIORITY_FEE),
            "lend.liquidation_priority_fee must be at most {}",
            crate::config::MAX_PRIORITY_FEE
        );
        // A standby re-posts the test price 15 s late; a liquidation needs it
        // under PRICE_LIQUIDATE.
        ensure!(
            self.role == Role::Primary || self.price_refresh() + 15 <= 75,
            "a standby's lend.price_refresh_secs must be at most 60"
        );
        if self.mainnet() {
            ensure!(
                self.origins.iter().all(|o| o.starts_with("https://")),
                "on mainnet every lend.origins entry must be https://"
            );
            ensure!(
                l.min_position_debt > 0,
                "on mainnet lend.limits.min_position_debt must be set"
            );
            let caps = [
                l.max_pool_size,
                l.max_total_collateral,
                l.max_position_collateral,
                l.max_position_debt,
                l.max_supply_per_wallet,
            ];
            ensure!(
                caps.iter().all(|c| *c > 0),
                "on mainnet every lend.limits.max_* cap must be set"
            );
        }
        Ok(())
    }

    fn grace(&self) -> u64 {
        self.standby_grace_secs.unwrap_or(45)
    }

    /// Any label naming mainnet (`mainnet-beta` too).
    fn mainnet(&self) -> bool {
        self.network.contains("mainnet")
    }

    fn price_refresh(&self) -> u64 {
        self.price_refresh_secs.unwrap_or(45)
    }

    pub fn state_path(&self, keeper: &Address) -> Result<PathBuf> {
        if let Some(p) = &self.state_file {
            return Ok(p.clone());
        }
        let dir = PathBuf::from(std::env::var("HOME")?).join(".local/state/keeler");
        std::fs::create_dir_all(&dir)?;
        Ok(dir.join(format!("keeper-{keeper}.json")))
    }

    fn paused(&self) -> bool {
        self.pause_file.as_deref().is_some_and(Path::exists)
    }

    /// The pause for one request: the pause file, or the Worker's
    /// `x-keeler-paused` header.
    fn paused_for(&self, header: bool) -> bool {
        header || self.paused()
    }

    fn min_supply(&self) -> u64 {
        self.limits
            .min_supply
            .max(keeler_lend::circuits::TUSD_MIN_SUPPLY)
    }

    fn min_collateral(&self) -> u64 {
        self.limits
            .min_collateral
            .max(keeler_lend::vaults::TSOL_TUSD.min_collateral)
    }
}

// ---- records ----

/// A note opening: everything needed to rebuild its hash and spend it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub kind: u8,
    /// The wallet whose user PDA owns it; `None` for the pool PDA.
    pub wallet: Option<String>,
    /// The owner's registered shielded address (receipts and positions).
    #[serde(default)]
    pub owner: Option<String>,
    pub mint: String,
    pub asset_id: u64,
    pub amount: u64,
    /// Hex.
    pub blinding: String,
    /// The note's data record, hex: the Borsh state (after the 32-byte
    /// address for the reserve).
    pub data: String,
    pub data_hash: String,
    /// The output hash in the tree.
    pub hash: String,
    pub nullifier: String,
    #[serde(default)]
    pub leaf: Option<u64>,
}

fn unhex(s: &str) -> Result<Vec<u8>> {
    ensure!(s.len().is_multiple_of(2), "odd hex");
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(Into::into))
        .collect()
}

fn hex32(s: &str) -> Result<[u8; 32]> {
    hex_bytes(s).ok_or_else(|| anyhow!("not 32 bytes of hex"))
}

impl Note {
    pub fn state_bytes(&self) -> Result<Vec<u8>> {
        let data = unhex(&self.data)?;
        Ok(if self.kind == KIND_RESERVE {
            data.get(32..).context("short reserve data")?.to_vec()
        } else {
            data
        })
    }

    pub fn state<T: BorshDeserialize>(&self) -> Result<T> {
        Ok(borsh::from_slice(&self.state_bytes()?)?)
    }

    pub fn nullifier(&self) -> Result<[u8; 32]> {
        hex32(&self.nullifier)
    }

    pub fn hash(&self) -> Result<[u8; 32]> {
        hex32(&self.hash)
    }

    /// `amount u64 LE ‖ blinding ‖ state`, base64: the account view's opening.
    pub fn message(&self) -> Result<String> {
        let mut m = self.amount.to_le_bytes().to_vec();
        m.extend(unhex(&self.blinding)?);
        m.extend(self.state_bytes()?);
        Ok(B64.encode(m))
    }

    fn owned_by(&self, wallet: &Address) -> bool {
        self.owned_by_str(&wallet.to_string())
    }

    fn owned_by_str(&self, wallet: &str) -> bool {
        self.wallet.as_deref() == Some(wallet)
    }
}

/// An unspent intent the keeper's viewing key opened under a user PDA.
#[derive(Clone, Debug)]
pub struct Intent {
    pub note: Note,
    pub purpose: u8,
    pub target: u64,
    pub expiry: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Supply,
    Redeem,
    DepositOpen,
    DepositAdd,
    Draw,
    Repay,
    Liquidate,
}

/// A built transaction, recorded before it is sent or handed out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub op: Op,
    pub wallet: String,
    /// The reserve nullifier it spends, when it spends the reserve.
    pub spends: Option<String>,
    /// Every input nullifier, hex.
    pub inputs: Vec<String>,
    /// Every output hash in slot order, hex.
    pub outputs: Vec<String>,
    /// The new reserve, receipt and position openings.
    pub notes: Vec<Note>,
    /// The `now` in its args: it cannot land after `now + DEAD_AFTER`.
    pub now: u64,
    #[serde(default)]
    pub ticket: Option<String>,
    pub owner_signed: bool,
    #[serde(default)]
    pub signature: Option<String>,
}

impl Pending {
    fn key(&self) -> Result<[u8; 32]> {
        hex32(
            self.spends
                .as_deref()
                .or(self.inputs.first().map(String::as_str))
                .context("no inputs")?,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TicketStatus {
    Open,
    Filled,
    /// Given up after failed builds; the maker refunds after expiry.
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ticket {
    pub id: String,
    pub wallet: String,
    pub purpose: u8,
    pub target: u64,
    pub amount: u64,
    pub expiry: u64,
    pub issued: u64,
    pub signature: String,
    pub status: TicketStatus,
    #[serde(default)]
    pub attempts: u32,
    /// Not tried again before this chain time.
    #[serde(default)]
    pub retry_at: u64,
    /// Its intent was seen on chain.
    #[serde(default)]
    pub seen: bool,
    /// The keeper that signed it, when not this one: a ticket adopted from
    /// an account read.
    #[serde(default)]
    pub keeper: Option<String>,
}

impl Ticket {
    fn open(&self, now: u64) -> bool {
        self.status == TicketStatus::Open && self.expiry > now
    }

    /// The intent this ticket was issued for.
    fn matches(&self, i: &Intent) -> bool {
        let mint = match self.purpose {
            purpose::DEPOSIT => vault1().collateral_mint,
            _ => vault1().debt_mint,
        };
        i.purpose == self.purpose
            && i.target == self.target
            && i.expiry == self.expiry
            && i.note.amount == self.amount
            && i.note.mint == mint.to_string()
            && i.note.owned_by_str(&self.wallet)
    }

    /// A failed or dropped fill: tried again later, and given up after
    /// `MAX_ATTEMPTS` unless it repays or tops up a position, which is
    /// tried until `FILL_MARGIN`.
    fn retry_later(&mut self, op: Op, now: u64) {
        self.attempts += 1;
        self.retry_at = now + (RETRY_SECS << self.attempts.min(6));
        if self.attempts >= MAX_ATTEMPTS
            && self.status == TicketStatus::Open
            && !matches!(op, Op::Repay | Op::DepositAdd)
        {
            self.status = TicketStatus::Failed;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reservation {
    pub wallet: String,
    pub until: u64,
}

/// The state file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Book {
    pub reserve: Option<Note>,
    pub receipts: Vec<Note>,
    pub positions: Vec<Note>,
    pub tickets: Vec<Ticket>,
    pub pending: Vec<Pending>,
    pub reservation: Option<Reservation>,
    /// Per keeper address, the newest transaction it paid for that the
    /// history scan has read. A file from before the keeper set holds one
    /// mark: this keeper's.
    #[serde(default, deserialize_with = "scan_marks")]
    pub scanned: BTreeMap<String, String>,
}

/// The key an old single scan mark loads under, until [`load_book`] names
/// it.
const OWN_MARK: &str = "";

fn scan_marks<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<String, String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Marks {
        One(String),
        Many(BTreeMap<String, String>),
    }
    Ok(match Option::<Marks>::deserialize(d)? {
        None => BTreeMap::new(),
        Some(Marks::One(s)) => BTreeMap::from([(OWN_MARK.to_owned(), s)]),
        Some(Marks::Many(m)) => m,
    })
}

impl Book {
    fn reserved_for_other(&self, wallet: &str, now: u64) -> bool {
        self.reservation
            .as_ref()
            .is_some_and(|r| r.until > now && r.wallet != wallet)
    }

    fn reserved(&self, now: u64) -> bool {
        self.reservation.as_ref().is_some_and(|r| r.until > now)
    }

    /// A keeper-sent transaction may still land: the lane waits for it.
    fn in_flight(&self) -> bool {
        self.pending.iter().any(|p| !p.owner_signed)
    }

    fn ticket_mut(&mut self, id: &Option<String>) -> Option<&mut Ticket> {
        let id = id.as_deref()?;
        self.tickets.iter_mut().find(|t| t.id == id)
    }
}

fn home_path<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<PathBuf>, D::Error> {
    Ok(Option::<PathBuf>::deserialize(d)?.map(|p| {
        match (p.strip_prefix("~"), std::env::var_os("HOME")) {
            (Ok(rest), Some(home)) => PathBuf::from(home).join(rest),
            _ => p,
        }
    }))
}

/// Writes `bytes` to `path` atomically, readable by this user only.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// What `keeler lend-set-price` last set; a running keeper keeps posting it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestPrice {
    pub vault: u64,
    pub price: u64,
    pub confidence: u64,
}

/// The test price's control file, next to the state file.
pub fn price_file(state: &Path) -> PathBuf {
    state.with_extension("price.json")
}

pub fn read_price_file(path: &Path) -> Result<Option<TestPrice>> {
    match std::fs::read(path) {
        Ok(t) => Ok(Some(serde_json::from_slice(&t).map_err(|_| {
            anyhow!("test price file {} is not valid", path.display())
        })?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// The state file, with tickets no keeper of `keepers` signed dropped
/// (`keepers[0]` is this keeper).
pub fn load_book(path: &Path, keepers: &[Address], program: &Address) -> Result<Book> {
    let text = match std::fs::read(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Book::default()),
        Err(e) => return Err(e.into()),
    };
    let mut book: Book = serde_json::from_slice(&text)
        .map_err(|_| anyhow!("keeper state file {} is not valid", path.display()))?;
    if let Some(m) = book.scanned.remove(OWN_MARK) {
        book.scanned.insert(keepers[0].to_string(), m);
    }
    let before = book.tickets.len();
    book.tickets.retain(|t| ticket_valid(t, keepers, program));
    if book.tickets.len() < before {
        log!(
            "lend: dropped {} tickets no keeper of the set signed",
            before - book.tickets.len()
        );
    }
    Ok(book)
}

/// Signed by its keeper (`keepers[0]` when it names none), one of `keepers`.
fn ticket_valid(t: &Ticket, keepers: &[Address], program: &Address) -> bool {
    let signer = match &t.keeper {
        None => Some(keepers[0]),
        Some(k) => parse_address(k).ok().filter(|k| keepers.contains(k)),
    };
    let (Some(signer), Ok(wallet), Some(id), Some(sig)) = (
        signer,
        parse_address(&t.wallet),
        hex_bytes::<16>(&t.id),
        hex_bytes::<64>(&t.signature),
    ) else {
        return false;
    };
    let m = lend_auth::ticket_bytes(
        program, &wallet, &id, t.purpose, t.target, t.amount, t.expiry,
    );
    lend_auth::ticket_signed_by(&signer, &m, &sig)
}

// ---- the chain seam ----

/// The collateral price as the program reads it, x 1e12: `low` is `price -
/// confidence`, `high` is `price + confidence`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quote {
    pub low: u64,
    pub high: u64,
    pub published: i64,
}

pub enum Job {
    Supply {
        intent: Intent,
    },
    DepositOpen {
        intent: Intent,
    },
    DepositAdd {
        position: Note,
        intent: Intent,
    },
    Repay {
        position: Note,
        intent: Intent,
    },
    Liquidate {
        position: Note,
        bound: u64,
    },
    Redeem {
        receipt: Note,
        amount: u64,
    },
    Draw {
        position: Note,
        borrow: u64,
        withdraw: u64,
        bound: u64,
    },
}

impl Job {
    fn op(&self) -> Op {
        match self {
            Job::Supply { .. } => Op::Supply,
            Job::DepositOpen { .. } => Op::DepositOpen,
            Job::DepositAdd { .. } => Op::DepositAdd,
            Job::Repay { .. } => Op::Repay,
            Job::Liquidate { .. } => Op::Liquidate,
            Job::Redeem { .. } => Op::Redeem,
            Job::Draw { .. } => Op::Draw,
        }
    }
}

/// A send the cluster refused for good (preflight, failed on chain, or its
/// blockhash expired): the transaction can never land.
#[derive(Debug)]
pub struct Rejected(pub String);

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the cluster refused the transaction: {}", self.0)
    }
}

impl std::error::Error for Rejected {}

/// A proved transaction, signed by the keeper as fee payer.
pub struct Built {
    pub pending: Pending,
    /// The wire transaction.
    pub tx: Vec<u8>,
    /// The price index the transaction used (supply for redeem, borrow for
    /// draw), 1e12-scaled.
    pub index: Option<u64>,
}

/// What the keeper needs from the chain.
pub trait Ledger: Sync {
    /// Chain time.
    fn now(&self) -> Result<u64>;
    fn registered(&self, wallet: &Address) -> Result<bool>;
    /// Whether each nullifier is spent, as of now (not the indexer).
    fn spent(&self, nullifiers: &[[u8; 32]]) -> Result<Vec<bool>>;
    /// The output hashes and leaf indices of the transaction that spent
    /// `nullifier`; `None` until the indexer has it.
    #[allow(clippy::type_complexity)]
    fn spender(&self, nullifier: &[u8; 32]) -> Result<Option<Vec<([u8; 32], u64)>>>;
    /// Intents under `wallet`'s user PDA the keeper's viewing key opens,
    /// spent or not, from transactions after cursor `after`, reading at most
    /// `pages` pages; with the cursor to pass next time.
    fn intents(
        &self,
        wallet: &Address,
        after: Option<&str>,
        pages: usize,
    ) -> Result<(Vec<Intent>, Option<String>)>;
    fn quote(&self, vault: u64) -> Result<Quote>;
    /// Writes the vault's test price (`set_test_price`), published at chain
    /// time; the signature.
    fn set_price(&self, vault: u64, price: u64, confidence: u64) -> Result<String>;
    /// Builds and proves `job` against `reserve` at chain time `now`.
    fn build(&self, job: &Job, reserve: Option<&Note>, now: u64) -> Result<Built>;
    /// Sends a keeper-built transaction; the signature. A [`Rejected`] error
    /// means it can never land; any other may still.
    fn send(&self, tx: &[u8]) -> Result<String>;
    /// The debt token the keeper holds privately at its lend address: what
    /// its liquidations pay from.
    fn debt_held(&self) -> Result<u64>;
    /// Every reserve, receipt and position the transactions `payer` (a
    /// keeper of the set) paid for, newer than signature `after`, created,
    /// recovered from the chain with the pool viewing key and the writer
    /// key; spent ones included. With the newest signature read, to pass
    /// next time.
    fn history(&self, payer: &Address, after: Option<&str>) -> Result<(Vec<Note>, Option<String>)>;
}

// ---- the keeper ----

pub struct Keeper<'a, L> {
    pub cfg: &'a Lend,
    ledger: &'a L,
    signer: &'a Keypair,
    program: Address,
    state: PathBuf,
    book: Mutex<Book>,
    /// Held while building and sending: one operation at a time per pool.
    lane: Mutex<()>,
    nonces: Mutex<Nonces>,
    /// (bucket start, figures).
    figures: Mutex<Option<(u64, Value)>>,
    /// Set when liquidations, repays or deposits wait: owner builds yield.
    priority_waiting: AtomicBool,
    /// Positions (by nullifier) whose liquidation failed, until a chain time.
    skip: Mutex<HashMap<String, u64>>,
    /// Per wallet, when each of its reservations of the last hour lapsed
    /// with no build of it landed: each one doubles its wait for the next.
    strikes: Mutex<HashMap<String, Vec<u64>>>,
    /// A reservation lapsed unused since the last fill ran: every waiting
    /// fill, supply included, goes before the next owner build.
    lapsed_unused: AtomicBool,
    /// The last pass found a fill to run.
    fills_waiting: AtomicBool,
    /// Chain times of each wallet's builds in the last hour.
    builds: Mutex<HashMap<String, Vec<u64>>>,
    /// Per wallet: the intent scan's cursor and the unspent intents seen
    /// that an open ticket matches.
    seen: Mutex<HashMap<String, Scan>>,
    /// The keeper set, this keeper first: tickets any of them signed are
    /// valid, and their transactions are followed.
    keepers: Vec<Address>,
    /// Standby: positions (wallet and id) past the threshold, since when.
    past: Mutex<HashMap<(String, u64), u64>>,
    /// Chain time of the last "no price" log of the liquidation watch.
    price_logged: Mutex<u64>,
    /// The last (surplus, bad debt) logged, and whether borrowing halts.
    solvency: Mutex<Option<Solvency>>,
    /// Chain time of the last check of the keeper's own debt-token balance.
    funds_checked: Mutex<u64>,
    /// The receipts and positions do not add up to the reserve's shares:
    /// the book may be missing a note, so nothing new is let in.
    incomplete: AtomicBool,
    /// Positions (wallet and id) that left the book with no successor, since
    /// when.
    dropped: Mutex<HashMap<(String, u64), Instant>>,
    /// Held while the keeper runs: one keeper per state file.
    _lock: std::fs::File,
}

/// The pool's cover, each pass: `surplus` is idle plus debt less what
/// suppliers are owed (0 when short), `bad_debt` the debt of positions whose
/// collateral is worth less than it at `price - confidence`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Solvency {
    pub surplus: u64,
    pub bad_debt: u64,
}

impl Solvency {
    fn halts(&self) -> bool {
        self.bad_debt > self.surplus
    }
}

type Refusal = (u16, String);

const BORROWING_HALTED: &str = "borrowing is halted while the pool's bad debt exceeds its surplus";
/// An intent scan's cursor and what it kept.
type Scan = (Option<String>, Vec<Intent>);

fn refuse<T>(status: u16, msg: &str) -> Result<T, Refusal> {
    Err((status, msg.to_owned()))
}

fn internal(e: anyhow::Error) -> Refusal {
    log!("lend: {e:#}");
    (503, "the keeper could not do this right now".to_owned())
}

fn reserve_of(note: &Note) -> Result<core::Reserve> {
    let s: ReserveState = note.state()?;
    Ok(core::Reserve {
        supply_shares: s.supply_shares,
        debt_shares: s.debt_shares,
        supply_price: s.supply_price,
        borrow_price: s.borrow_price,
        rate_bps: u32::try_from(s.rate_bps)?,
        last_update: s.last_update,
    })
}

/// The reserve accrued to `now` (never before its last update).
fn accrued(note: &Note, now: u64) -> Result<core::Reserve> {
    let r = reserve_of(note)?;
    core::accrue(&r, now.max(r.last_update), core::TUSD_POOL.fee_bps).map_err(|e| anyhow!("{e}"))
}

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0; N];
    getrandom::getrandom(&mut b).map_err(|e| anyhow!("randomness: {e}"))?;
    Ok(b)
}

/// `x` rounded to two significant figures.
pub fn coarse(x: u64) -> u64 {
    let mut unit = 1u64;
    while x / unit >= 100 {
        unit *= 10;
    }
    if unit == 1 {
        return x;
    }
    let q = (u128::from(x) + u128::from(unit / 2)) / u128::from(unit);
    u64::try_from(q * u128::from(unit)).unwrap_or(x - x % unit)
}

/// How long a wallet waits after the last of `strikes` reservations it let
/// lapse unused within an hour: `reservation_secs`, doubled for each earlier
/// one, at most an hour.
pub fn cooldown_secs(reservation_secs: u64, strikes: usize) -> u64 {
    let doublings = u32::try_from(strikes.saturating_sub(1)).map_or(12, |d| d.min(12));
    reservation_secs.saturating_mul(1 << doublings).min(HOUR)
}

/// The smallest number of two significant figures at or above `x`.
pub fn coarse_up(x: u64) -> u64 {
    let mut unit = 1u64;
    while x / unit >= 100 {
        unit *= 10;
    }
    x.div_ceil(unit).saturating_mul(unit)
}

/// The pool's [`Solvency`] with `idle` in the reserve, the reserve `r`
/// accrued, and collateral valued at `low` (x 1e12).
pub fn solvency(book: &Book, idle: u64, r: &core::Reserve, low: u64) -> Result<Solvency> {
    let e = |e: core::LendError| anyhow!("{e}");
    let debt = core::total_debt(r).map_err(e)?;
    let owed = core::total_supply(r).map_err(e)?;
    let surplus = (u128::from(idle) + u128::from(debt)).saturating_sub(u128::from(owed));
    let mut bad = 0u128;
    for p in &book.positions {
        let s: PositionState = p.state()?;
        if s.debt_shares == 0 {
            continue;
        }
        let d = core::debt_of(s.debt_shares, r.borrow_price).map_err(e)?;
        if core::collateral_value(p.amount, low).map_err(e)? < d {
            bad += u128::from(d);
        }
    }
    let cut = |x: u128| u64::try_from(x).unwrap_or(u64::MAX);
    Ok(Solvency {
        surplus: cut(surplus),
        bad_debt: cut(bad),
    })
}

/// The largest debt of a position in `book`, at the reserve `r` accrued.
pub fn largest_debt(book: &Book, r: &core::Reserve) -> Result<u64> {
    let mut most = 0;
    for p in &book.positions {
        let s: PositionState = p.state()?;
        most = most.max(core::debt_of(s.debt_shares, r.borrow_price).map_err(|e| anyhow!("{e}"))?);
    }
    Ok(most)
}

/// The largest position's debt in the state file at `path` of the keeper
/// `keeper`, at chain time `now`; `None` without a reserve.
pub fn largest_debt_in(path: &Path, keeper: &Address, now: u64) -> Result<Option<u64>> {
    let mut keepers = vec![*keeper];
    keepers.extend(keeler_lend::KEEPERS.iter().filter(|k| *k != keeper));
    let book = load_book(path, &keepers, &keeler_lend::ID)?;
    let Some(r) = &book.reserve else {
        return Ok(None);
    };
    Ok(Some(largest_debt(&book, &accrued(r, now)?)?))
}

/// Whether the receipts' shares sum to the reserve's supply shares and the
/// positions' debt shares to its debt shares; `None` without a reserve.
fn shares_add_up(book: &Book) -> Result<Option<bool>> {
    let Some(reserve) = &book.reserve else {
        return Ok(None);
    };
    let r: ReserveState = reserve.state()?;
    let mut supply = 0u128;
    for n in &book.receipts {
        supply += u128::from(n.state::<ReceiptState>()?.supply_shares);
    }
    let mut debt = 0u128;
    for n in &book.positions {
        debt += u128::from(n.state::<PositionState>()?.debt_shares);
    }
    Ok(Some(
        supply == u128::from(r.supply_shares) && debt == u128::from(r.debt_shares),
    ))
}

fn purpose_name(p: u8) -> &'static str {
    match p {
        purpose::SUPPLY => "supply",
        purpose::DEPOSIT => "deposit",
        _ => "repay",
    }
}

fn decimal(s: Option<&String>) -> Option<u64> {
    let s = s?;
    (!s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// An account read's body: its signed fields, and the tickets the page holds
/// (at most 8; not signed, each carries its keeper's signature).
fn account_body(body: &[u8]) -> Option<(BTreeMap<String, String>, Vec<Value>)> {
    let Value::Object(mut map) = serde_json::from_slice::<Value>(body).ok()? else {
        return None;
    };
    let held = match map.remove("tickets") {
        None => Vec::new(),
        Some(Value::Array(a)) if a.len() <= 8 && a.iter().all(Value::is_object) => a,
        Some(_) => return None,
    };
    Some((lend_auth::fields_from_map(map)?, held))
}

/// The Worker's form of a page origin: `http(s)://` and a host, at most 200
/// characters.
fn origin_ok(o: &str) -> bool {
    let host = o
        .strip_prefix("https://")
        .or_else(|| o.strip_prefix("http://"));
    o.len() <= 200
        && host.is_some_and(|h| {
            !h.is_empty()
                && h.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".:[]-".contains(&b))
        })
}

fn vault1() -> &'static keeler_lend::vaults::Vault {
    &keeler_lend::vaults::TSOL_TUSD
}

impl<'a, L: Ledger> Keeper<'a, L> {
    /// Loads the state file and reconciles it with the chain.
    pub fn open(cfg: &'a Lend, ledger: &'a L, signer: &'a Keypair, state: PathBuf) -> Result<Self> {
        // `connect` checked the key is one of KEEPERS; the unit tests' is not.
        Self::open_in(cfg, ledger, signer, state, keeler_lend::KEEPERS)
    }

    /// [`Keeper::open`] in the keeper set `set` (this keeper need not be in it).
    pub fn open_in(
        cfg: &'a Lend,
        ledger: &'a L,
        signer: &'a Keypair,
        state: PathBuf,
        set: &[Address],
    ) -> Result<Self> {
        let program = keeler_lend::ID;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(state.with_extension("lock"))?;
        lock.try_lock()
            .map_err(|_| anyhow!("another keeper is using {}", state.display()))?;
        let mut keepers = vec![signer.pubkey()];
        keepers.extend(set.iter().filter(|k| **k != signer.pubkey()));
        let book = load_book(&state, &keepers, &program)?;
        let k = Self {
            cfg,
            ledger,
            signer,
            program,
            state,
            book: Mutex::new(book),
            lane: Mutex::new(()),
            nonces: Mutex::default(),
            figures: Mutex::new(None),
            priority_waiting: AtomicBool::new(false),
            skip: Mutex::default(),
            strikes: Mutex::default(),
            lapsed_unused: AtomicBool::new(false),
            fills_waiting: AtomicBool::new(false),
            builds: Mutex::default(),
            seen: Mutex::default(),
            keepers,
            past: Mutex::default(),
            price_logged: Mutex::new(0),
            solvency: Mutex::new(None),
            funds_checked: Mutex::new(0),
            incomplete: AtomicBool::new(false),
            dropped: Mutex::default(),
            _lock: lock,
        };
        k.recover()?;
        Ok(k)
    }

    /// The other keepers of the set.
    fn peers(&self) -> &[Address] {
        &self.keepers[1..]
    }

    fn book(&self) -> Result<MutexGuard<'_, Book>> {
        self.book
            .lock()
            .map_err(|_| anyhow!("keeper state poisoned"))
    }

    pub fn snapshot(&self) -> Result<Book> {
        Ok(self.book()?.clone())
    }

    fn save(&self, book: &Book) -> Result<()> {
        write_private(&self.state, &serde_json::to_vec(book)?).context("save the keeper state")
    }

    /// Settles the pending builds, then takes every reserve, receipt and
    /// position the chain shows the keeper made, keeps the unspent ones, and
    /// saves. Losing the reserve opening would freeze the pool, so a state
    /// with none, or two, unspent reserves is an error unless the pool was
    /// never initialised.
    pub fn recover(&self) -> Result<()> {
        self.resolve()?;
        self.merge_history(&self.keepers, true)?;
        let book = self.snapshot()?;
        if book.reserve.is_none() {
            log!("lend: no reserve found; run `keeler lend-init-reserve` once");
        }
        log!(
            "lend: recovered reserve {}, {} receipts, {} positions, {} pending",
            if book.reserve.is_some() {
                "present"
            } else {
                "absent"
            },
            book.receipts.len(),
            book.positions.len(),
            book.pending.len()
        );
        Ok(())
    }

    /// Adds what the transactions of the keepers `who` paid for since their
    /// last scan made, keeps the unspent notes, and saves with the new scan
    /// marks. Unless `force`, a scan that found nothing new changes nothing.
    fn merge_history(&self, who: &[Address], force: bool) -> Result<()> {
        let snapshot = self.snapshot()?;
        let marks = snapshot.scanned.clone();
        let mut found = Vec::new();
        let mut next = marks.clone();
        for k in who {
            let key = k.to_string();
            if !marks.contains_key(&key) {
                // One request per transaction the key ever paid for.
                log!("lend: reading the whole history of keeper {key}; a first start can take minutes");
            }
            let (notes, newest) = self
                .ledger
                .history(k, marks.get(&key).map(String::as_str))
                .context("rebuild from the chain")?;
            found.extend(notes);
            if let Some(n) = newest {
                next.insert(key, n);
            }
        }
        if !force && found.is_empty() && next == marks {
            return Ok(());
        }
        // Only this thread changes the notes, so the chain is read before
        // the lock, as `resolve` does.
        let mut all: Vec<Note> = snapshot
            .reserve
            .iter()
            .chain(&snapshot.receipts)
            .chain(&snapshot.positions)
            .cloned()
            .collect();
        for n in found {
            if !all.iter().any(|m| m.nullifier == n.nullifier) {
                all.push(n);
            }
        }
        // Spent notes leave, a pending build's inputs too: when it resolves
        // as landed it brings its outputs in.
        let nullifiers: Vec<[u8; 32]> = all.iter().map(Note::nullifier).collect::<Result<_>>()?;
        let spent = self.ledger.spent(&nullifiers)?;
        let live: Vec<Note> = all
            .into_iter()
            .zip(spent)
            .filter(|(n, s)| !s && holds_something(n))
            .map(|(n, _)| n)
            .collect();
        let reserves: Vec<&Note> = live.iter().filter(|n| n.kind == KIND_RESERVE).collect();
        ensure!(
            reserves.len() <= 1,
            "more than one unspent reserve; the state file and the chain disagree"
        );
        let reserve = reserves.first().map(|n| (*n).clone());
        let mut book = self.book()?;
        book.reserve = reserve;
        book.receipts = live
            .iter()
            .filter(|n| n.kind == KIND_RECEIPT)
            .cloned()
            .collect();
        book.positions = live
            .iter()
            .filter(|n| n.kind == KIND_POSITION)
            .cloned()
            .collect();
        book.scanned = next;
        self.note_dropped(&snapshot.positions, &book.positions);
        self.save(&book)?;
        self.check_shares(&book);
        Ok(())
    }

    /// Records the positions of `before` with no position of the same
    /// wallet and id in `after`.
    fn note_dropped(&self, before: &[Note], after: &[Note]) {
        let key = |p: &Note| {
            p.state::<PositionState>()
                .ok()
                .map(|s| (p.wallet.clone().unwrap_or_default(), s.position_id))
        };
        let left: Vec<(String, u64)> = after.iter().filter_map(key).collect();
        if let Ok(mut d) = self.dropped.lock() {
            d.retain(|_, at| at.elapsed() < Duration::from_secs(DROPPED_SECS));
            for k in before.iter().filter_map(key) {
                if !left.contains(&k) {
                    d.insert(k, Instant::now());
                }
            }
        }
    }

    /// Whether a position of `wallet` with id `id` left the book lately with
    /// no successor: a peer may have spent it and its note not be here yet.
    fn dropped_lately(&self, wallet: &str, id: u64) -> bool {
        self.dropped.lock().is_ok_and(|d| {
            d.get(&(wallet.to_owned(), id))
                .is_some_and(|at| at.elapsed() < Duration::from_secs(DROPPED_SECS))
        })
    }

    /// Sets [`Keeper::incomplete`] from whether the book's shares add up to
    /// the reserve's, logging each change. Not while a build is pending: its
    /// spent inputs may have left the book before its outputs joined.
    fn check_shares(&self, book: &Book) {
        if !book.pending.is_empty() {
            return;
        }
        let ok = match shares_add_up(book) {
            Ok(Some(ok)) => ok,
            Ok(None) => return,
            Err(e) => {
                log!("lend: the book's shares cannot be read: {e:#}");
                false
            }
        };
        let was = self.incomplete.swap(!ok, Ordering::Relaxed);
        if !ok && !was {
            log!(
                "lend: WARNING: the receipts and positions do not add up to the reserve's shares; \
                 the book may be missing a note. New supply, new positions and new borrowing \
                 are refused until it adds up (repays, top-ups, redeems and withdrawals go on)"
            );
        } else if ok && was {
            log!("lend: the book adds up to the reserve again; lending is open");
        }
    }

    /// The pause for one request: the pause file, the Worker's header, or a
    /// book that does not add up.
    fn paused_now(&self, header: bool) -> bool {
        self.cfg.paused_for(header) || self.incomplete.load(Ordering::Relaxed)
    }

    /// Settles every pending build the chain has decided: the one whose
    /// notes the spending transaction appended landed; the others on those
    /// inputs, those with any input spent by another transaction, and those
    /// past `now + DEAD_AFTER` with the input still unspent, are dropped.
    /// Spent notes nobody here spent (a close, a lost record) leave the
    /// book; a reserve spent that way is found again from the chain.
    pub fn resolve(&self) -> Result<()> {
        let now = self.ledger.now()?;
        let snapshot = self.snapshot()?;
        let keys: Vec<[u8; 32]> = snapshot
            .pending
            .iter()
            .map(Pending::key)
            .collect::<Result<_>>()?;
        // The other inputs are read before the keys: a key unspent after
        // another input was seen spent means some other transaction took it.
        let mut others: Vec<[u8; 32]> = Vec::new();
        for (p, key) in snapshot.pending.iter().zip(&keys) {
            for i in &p.inputs {
                let n = hex32(i)?;
                if n != *key && !others.contains(&n) {
                    others.push(n);
                }
            }
        }
        let taken: Vec<[u8; 32]> = if others.is_empty() {
            Vec::new()
        } else {
            self.ledger
                .spent(&others)?
                .into_iter()
                .zip(&others)
                .filter(|(s, _)| *s)
                .map(|(_, n)| *n)
                .collect()
        };
        let spent = if keys.is_empty() {
            Vec::new()
        } else {
            self.ledger.spent(&keys)?
        };
        let mut landed = Vec::new();
        let mut dead = Vec::new();
        for ((p, key), s) in snapshot.pending.iter().zip(&keys).zip(spent) {
            if !s {
                let lost_input = p
                    .inputs
                    .iter()
                    .any(|i| hex32(i).is_ok_and(|n| taken.contains(&n)));
                if lost_input || now > p.now + DEAD_AFTER {
                    dead.push(p.clone());
                }
                continue;
            }
            let Some(outs) = self.ledger.spender(key)? else {
                continue;
            };
            let mine = p
                .notes
                .iter()
                .all(|n| n.hash().is_ok_and(|h| outs.iter().any(|(o, _)| *o == h)));
            if mine && !p.notes.is_empty() {
                landed.push((p.clone(), outs));
            } else {
                dead.push(p.clone());
            }
        }
        let mut book = self.book()?;
        let before = book.clone();
        let positions_before = book.positions.clone();
        for (p, outs) in landed {
            if book.pending.contains(&p) {
                apply(&mut book, &p, &outs);
                if p.owner_signed {
                    self.forgive(&p);
                }
            }
        }
        for p in dead {
            if let Some(i) = book.pending.iter().position(|q| *q == p) {
                book.pending.remove(i);
                if let Some(t) = book.ticket_mut(&p.ticket) {
                    t.retry_later(p.op, now);
                }
                log!(
                    "lend: a {:?} build for {} did not land; dropped",
                    p.op,
                    p.wallet
                );
            }
        }
        // Notes spent outside any record.
        let held: Vec<Note> = book
            .reserve
            .iter()
            .chain(&book.receipts)
            .chain(&book.positions)
            .filter(|n| !book.pending.iter().any(|p| p.inputs.contains(&n.nullifier)))
            .cloned()
            .collect();
        drop(book);
        let nullifiers: Vec<[u8; 32]> = held.iter().map(Note::nullifier).collect::<Result<_>>()?;
        let gone: Vec<String> = if nullifiers.is_empty() {
            Vec::new()
        } else {
            self.ledger
                .spent(&nullifiers)?
                .into_iter()
                .zip(&held)
                .filter(|(s, _)| *s)
                .map(|(_, n)| n.nullifier.clone())
                .collect()
        };
        let mut book = self.book()?;
        let lost_reserve = book
            .reserve
            .as_ref()
            .is_some_and(|r| gone.contains(&r.nullifier));
        book.receipts.retain(|n| !gone.contains(&n.nullifier));
        book.positions.retain(|n| !gone.contains(&n.nullifier));
        // Tickets long past their expiry go.
        book.tickets.retain(|t| t.expiry + 7 * 86_400 > now);
        if lost_reserve {
            book.reserve = None;
        }
        self.note_dropped(&positions_before, &book.positions);
        if *book != before {
            self.save(&book)?;
        }
        self.check_shares(&book);
        drop(book);
        if lost_reserve {
            log!("lend: the reserve was spent outside any record; rebuilding from the chain");
            self.merge_history(&self.keepers, true)?;
            if self.snapshot()?.reserve.is_none() {
                // The marks went past the reserve's spender: read it all again.
                log!("lend: no reserve past the scan marks; reading every keeper's history again");
                let mut b = self.book()?;
                b.scanned.clear();
                self.save(&b)?;
                drop(b);
                self.merge_history(&self.keepers, true)?;
            }
        }
        Ok(())
    }

    /// One pass: settle, then run the most urgent job, if any. True when a
    /// job ran.
    pub fn step(&self) -> Result<bool> {
        self.resolve()?;
        // The peers' new transactions, every pass: a deposit by one of them
        // spends no reserve, so nothing else would show it.
        // One that fails (an RPC without a just-landed transaction, a slow
        // indexer) is read again next pass; liquidations do not wait for it.
        if !self.peers().is_empty() {
            if let Err(e) = self.merge_history(self.peers(), false) {
                log!("lend: the peers' history: {e:#}; the book stands as it is");
            }
        }
        let now = self.ledger.now()?;
        let book = self.snapshot()?;
        let Some(reserve) = book.reserve.clone() else {
            // Not initialised yet, or lost: look again (cheap past the mark).
            self.merge_history(&self.keepers, false)?;
            return Ok(false);
        };
        self.strike_lapsed(book.reservation.as_ref(), now);
        if book.in_flight() {
            return Ok(false);
        }
        let r = accrued(&reserve, now)?;
        self.check_funds(&book, &r, now);
        let indebted = book
            .positions
            .iter()
            .any(|p| p.state::<PositionState>().is_ok_and(|s| s.debt_shares > 0));
        let quote = if indebted {
            self.watched_quote(now)
        } else {
            None
        };
        // Without a price the last figures stand.
        if !indebted || quote.is_some() {
            self.check_solvency(&book, &reserve, &r, quote.map_or(0, |q| q.low))?;
        }
        // 1. Liquidations, even while a wallet holds the reservation.
        let job = match &quote {
            Some(q) => self.liquidation(&book, &r, q, now)?,
            None => None,
        };
        let mut liquidation_retries = false;
        if let Some(job) = job {
            let position = match &job {
                Job::Liquidate { position, .. } => position.nullifier.clone(),
                _ => String::new(),
            };
            self.priority_waiting.store(true, Ordering::Relaxed);
            match self.execute(job, None) {
                Ok(ran) => return Ok(ran),
                Err(e) if self.taken_elsewhere(&reserve, &position) => {
                    log!("lend: liquidation lost its reserve or position to another transaction; retry next pass: {e:#}");
                    liquidation_retries = true;
                }
                // That one waits; the rest of the queue goes on.
                Err(e) => {
                    log!("lend: liquidation failed; retry in {LIQUIDATION_BACKOFF} s: {e:#}");
                    self.skip
                        .lock()
                        .map_err(|_| anyhow!("poisoned"))?
                        .insert(position, now + LIQUIDATION_BACKOFF);
                }
            }
        }
        // 2. Fills, in order: repay, deposit, supply.
        let fills = self.fills(&book, now)?;
        if fills.is_empty() {
            self.lapsed_unused.store(false, Ordering::Relaxed);
        }
        self.fills_waiting
            .store(!fills.is_empty(), Ordering::Relaxed);
        self.priority_waiting.store(
            liquidation_retries || fills.iter().any(|(_, j)| !matches!(j, Job::Supply { .. })),
            Ordering::Relaxed,
        );
        if book.reserved(now) {
            return Ok(false);
        }
        for (ticket, job) in fills {
            let op = job.op();
            match self.execute(job, Some(ticket.clone())) {
                Ok(ran) => {
                    if ran {
                        self.lapsed_unused.store(false, Ordering::Relaxed);
                    }
                    return Ok(ran);
                }
                Err(e) => {
                    log!("lend: fill of ticket {ticket} failed: {e:#}");
                    let mut b = self.book()?;
                    if let Some(t) = b.ticket_mut(&Some(ticket)) {
                        t.retry_later(op, now);
                    }
                    self.save(&b)?;
                }
            }
        }
        Ok(false)
    }

    /// Once a minute: warns when the keeper's own debt token would not cover
    /// a liquidation of the largest debt in the book.
    fn check_funds(&self, book: &Book, r: &core::Reserve, now: u64) {
        let Ok(mut at) = self.funds_checked.lock() else {
            return;
        };
        if now < *at + FUNDS_CHECK_SECS {
            return;
        }
        *at = now;
        drop(at);
        let largest = match largest_debt(book, r) {
            Ok(0) => return,
            Ok(d) => d,
            Err(e) => {
                log!("lend: debts unreadable: {e:#}");
                return;
            }
        };
        match self.ledger.debt_held() {
            Ok(held) if held < largest => log!(
                "lend: WARNING: the keeper's debt token at its lend address is under the \
                 largest position's debt; it cannot liquidate that position. \
                 `keeler lend-balance` shows both; fund it with `keeler lend-fund`"
            ),
            Ok(_) => {}
            Err(e) => log!("lend: the keeper's own balance: {e:#}"),
        }
    }

    /// The price for this pass's liquidation watch and figures: none when
    /// the account is missing, unverified or older than `PRICE_LIQUIDATE`
    /// (no other source is used), logged at most once a minute.
    fn watched_quote(&self, now: u64) -> Option<Quote> {
        let why = match self.ledger.quote(vault1().id) {
            Ok(q) if (now as i64).abs_diff(q.published) <= PRICE_LIQUIDATE => return Some(q),
            Ok(q) => format!(
                "the price is {} s old; no liquidation is possible",
                (now as i64).abs_diff(q.published)
            ),
            Err(e) => format!("no price for the liquidation watch: {e:#}"),
        };
        if let Ok(mut at) = self.price_logged.lock() {
            if now >= *at + 60 {
                *at = now;
                log!("lend: {why}");
            }
        }
        None
    }

    /// Surplus and bad debt at `q`, logged when they change; while the bad
    /// debt exceeds the surplus, new borrowing is refused.
    fn check_solvency(
        &self,
        book: &Book,
        reserve: &Note,
        r: &core::Reserve,
        low: u64,
    ) -> Result<()> {
        let s = solvency(book, reserve.amount, r, low)?;
        let mut last = self.solvency.lock().map_err(|_| anyhow!("poisoned"))?;
        if *last != Some(s) {
            log!(
                "lend: surplus {} and bad debt {} (debt token base units){}",
                s.surplus,
                s.bad_debt,
                if s.halts() {
                    "; new borrowing is refused"
                } else {
                    ""
                }
            );
            *last = Some(s);
        }
        Ok(())
    }

    fn halted(&self) -> bool {
        self.solvency
            .lock()
            .is_ok_and(|s| s.is_some_and(|s| s.halts()))
    }

    /// Whether another transaction spent the reserve or the position a
    /// liquidation was built on: the next pass tries again on the new notes.
    fn taken_elsewhere(&self, reserve: &Note, position: &str) -> bool {
        let (Ok(r), Ok(p)) = (reserve.nullifier(), hex32(position)) else {
            return false;
        };
        self.ledger.spent(&[r, p]).is_ok_and(|s| s.contains(&true))
    }

    /// The first position past the liquidation threshold at `price +
    /// confidence` plus the margin, not waiting out a failed attempt; for a
    /// standby, past it for `standby_grace_secs`.
    fn liquidation(
        &self,
        book: &Book,
        r: &core::Reserve,
        q: &Quote,
        now: u64,
    ) -> Result<Option<Job>> {
        let skip = self.skip.lock().map_err(|_| anyhow!("poisoned"))?.clone();
        let indebted: Vec<&Note> = book
            .positions
            .iter()
            .filter(|p| p.state::<PositionState>().is_ok_and(|s| s.debt_shares > 0))
            .filter(|p| skip.get(&p.nullifier).is_none_or(|until| *until <= now))
            .collect();
        let mut past = self.past.lock().map_err(|_| anyhow!("poisoned"))?;
        let mut still = Vec::new();
        let m = u128::from(self.cfg.limits.bound_margin_bps);
        let bound = u64::try_from((u128::from(q.high) * (10_000 + m)).div_ceil(10_000))?;
        let mut job = None;
        for p in indebted {
            let s: PositionState = p.state()?;
            // Sized as the builder will size it; a position left just past
            // the threshold by the last one is not worth another proof.
            let plan = core::liquidate(
                s.debt_shares,
                r.borrow_price,
                p.amount,
                bound,
                &core::TSOL_TUSD_VAULT,
            );
            let debt = core::debt_of(s.debt_shares, r.borrow_price).map_err(|e| anyhow!("{e}"))?;
            let value = core::collateral_value(p.amount, bound).map_err(|e| anyhow!("{e}"))?;
            let worth = plan.is_ok_and(|l| {
                l.repay >= self.cfg.limits.min_repay
                    || core::above_max_limit(debt, value, &core::TSOL_TUSD_VAULT)
            });
            if !worth {
                continue;
            }
            let key = (p.wallet.clone().unwrap_or_default(), s.position_id);
            let since = *past.entry(key.clone()).or_insert(now);
            still.push(key);
            let waited = self.cfg.role == Role::Primary || now >= since + self.cfg.grace();
            if waited && job.is_none() {
                job = Some(Job::Liquidate {
                    position: p.clone(),
                    bound,
                });
            }
        }
        // A position back above the threshold starts its grace again.
        past.retain(|k, _| still.contains(k));
        Ok(job)
    }

    /// The unspent intents of `wallet` an open ticket matches: the scan
    /// goes on from its cursor, a few pages a pass.
    fn wallet_intents(&self, wallet: &Address, now: u64) -> Result<Vec<Intent>> {
        let w = wallet.to_string();
        let (after, mut known) = self
            .seen
            .lock()
            .map_err(|_| anyhow!("poisoned"))?
            .get(&w)
            .cloned()
            .unwrap_or_default();
        let (found, cursor) = self
            .ledger
            .intents(wallet, after.as_deref(), INTENT_PAGES)?;
        for i in found {
            if !known.iter().any(|k| k.note.nullifier == i.note.nullifier) {
                known.push(i);
            }
        }
        // Tickets read after the scan: a ticket is saved before its intent
        // can exist, so none is missed.
        let tickets: Vec<Ticket> = self
            .book()?
            .tickets
            .iter()
            .filter(|t| t.wallet == w && t.open(now))
            .cloned()
            .collect();
        known.retain(|i| tickets.iter().any(|t| t.matches(i)));
        if !known.is_empty() {
            let nullifiers: Vec<[u8; 32]> = known
                .iter()
                .map(|i| i.note.nullifier())
                .collect::<Result<_>>()?;
            let spent = self.ledger.spent(&nullifiers)?;
            // A ticket whose intent went with no build of this keeper on it
            // was filled by a peer (or the intent cancelled): it is closed.
            let gone: Vec<String> = known
                .iter()
                .zip(&spent)
                .filter(|(_, s)| **s)
                .filter_map(|(i, _)| tickets.iter().find(|t| t.matches(i)))
                .map(|t| t.id.clone())
                .collect();
            if !gone.is_empty() {
                let mut b = self.book()?;
                let before = b.clone();
                let ids: Vec<String> = gone
                    .into_iter()
                    .filter(|id| !b.pending.iter().any(|p| p.ticket.as_ref() == Some(id)))
                    .collect();
                for t in b.tickets.iter_mut().filter(|t| ids.contains(&t.id)) {
                    t.status = TicketStatus::Filled;
                }
                if *b != before {
                    self.save(&b)?;
                }
            }
            let mut s = spent.into_iter();
            known.retain(|_| s.next() == Some(false));
        }
        self.seen
            .lock()
            .map_err(|_| anyhow!("poisoned"))?
            .insert(w, (cursor.or(after), known.clone()));
        Ok(known)
    }

    /// Ticketed intents found on chain, most urgent first, then oldest.
    fn fills(&self, book: &Book, now: u64) -> Result<Vec<(String, Job)>> {
        let open: Vec<&Ticket> = book
            .tickets
            .iter()
            .filter(|t| {
                t.status == TicketStatus::Open && now + FILL_MARGIN < t.expiry && t.retry_at <= now
            })
            .collect();
        let mut wallets: Vec<&str> = open.iter().map(|t| t.wallet.as_str()).collect();
        wallets.sort_unstable();
        wallets.dedup();
        let paused = self.paused_now(false);
        // The caps again at fill: a supply or deposit that no longer fits
        // waits out its expiry and is refunded.
        let pool = book
            .reserve
            .as_ref()
            .and_then(|n| accrued(n, now).ok())
            .and_then(|r| core::total_supply(&r).ok())
            .map_or(0, u128::from);
        let l = &self.cfg.limits;
        let fits = |amount: u64| within(l.max_pool_size, pool + u128::from(amount));
        let collateral: u128 = book.positions.iter().map(|p| u128::from(p.amount)).sum();
        // A top-up is never held to the vault's total.
        let opens = |amount: u64| {
            within(l.max_total_collateral, collateral + u128::from(amount))
                && within(l.max_position_collateral, u128::from(amount))
        };
        let tops = |position: &Note, amount: u64| {
            within(
                l.max_position_collateral,
                u128::from(position.amount) + u128::from(amount),
            )
        };
        let mut jobs = Vec::new();
        let mut seen = Vec::new();
        for w in wallets {
            let wallet = parse_address(w)?;
            let intents = match self.wallet_intents(&wallet, now) {
                Ok(i) => i,
                Err(e) => {
                    log!("lend: intents of {wallet}: {e:#}");
                    continue;
                }
            };
            for t in open.iter().filter(|t| t.wallet == w) {
                let Some(intent) = intents.iter().find(|i| t.matches(i)) else {
                    continue;
                };
                if !t.seen {
                    seen.push(t.id.clone());
                }
                let position = book
                    .positions
                    .iter()
                    .find(|p| {
                        p.owned_by(&wallet)
                            && p.state::<PositionState>()
                                .is_ok_and(|s| s.position_id == t.target)
                    })
                    .cloned();
                let intent = intent.clone();
                let job = match (t.purpose, position) {
                    (purpose::REPAY, Some(position)) => Job::Repay { position, intent },
                    (purpose::DEPOSIT, Some(position)) if tops(&position, t.amount) => {
                        Job::DepositAdd { position, intent }
                    }
                    // Not where a peer has just spent a position of that id:
                    // its successor would be a second position with that id.
                    (purpose::DEPOSIT, None)
                        if !paused && opens(t.amount) && !self.dropped_lately(w, t.target) =>
                    {
                        Job::DepositOpen { intent }
                    }
                    (purpose::SUPPLY, _) if !paused && fits(t.amount) => Job::Supply { intent },
                    _ => continue,
                };
                jobs.push((t.id.clone(), t.issued, job));
            }
        }
        if !seen.is_empty() {
            let mut b = self.book()?;
            for t in b.tickets.iter_mut().filter(|t| seen.contains(&t.id)) {
                t.seen = true;
            }
            self.save(&b)?;
        }
        let rank = |j: &Job| match j {
            Job::Repay { .. } => 0,
            Job::DepositAdd { .. } | Job::DepositOpen { .. } => 1,
            _ => 2,
        };
        jobs.sort_by_key(|(_, issued, j)| (rank(j), *issued));
        Ok(jobs.into_iter().map(|(id, _, j)| (id, j)).collect())
    }

    /// Builds, records, then sends a keeper-signed job. False when it did
    /// not run: the lane found a transaction in flight, or (for a fill) the
    /// reservation or the ticket's expiry.
    fn execute(&self, job: Job, ticket: Option<String>) -> Result<bool> {
        let _lane = self.lane.lock().map_err(|_| anyhow!("lane poisoned"))?;
        let now = self.ledger.now()?;
        let book = self.snapshot()?;
        if book.in_flight() {
            return Ok(false);
        }
        if let Some(id) = &ticket {
            let live =
                book.tickets.iter().find(|t| t.id == *id).is_some_and(|t| {
                    t.status == TicketStatus::Open && now + FILL_MARGIN < t.expiry
                });
            if book.reserved(now) || !live {
                return Ok(false);
            }
        }
        let mut built = self.ledger.build(&job, book.reserve.as_ref(), now)?;
        built.pending.ticket = ticket;
        self.record(&built.pending)?;
        match self.ledger.send(&built.tx) {
            Ok(sig) => {
                let mut b = self.book()?;
                if let Some(p) = b.pending.iter_mut().find(|p| **p == built.pending) {
                    p.signature = Some(sig.clone());
                }
                self.save(&b)?;
                if matches!(job, Job::Liquidate { .. }) {
                    log!(
                        "lend: liquidation of a position of {} sent: {sig}",
                        built.pending.wallet
                    );
                } else {
                    log!(
                        "lend: {:?} for {} sent: {sig}",
                        built.pending.op,
                        built.pending.wallet
                    );
                }
            }
            // It can never land: its record goes now, not after the window.
            Err(e) if e.downcast_ref::<Rejected>().is_some() => {
                let mut b = self.book()?;
                b.pending.retain(|p| *p != built.pending);
                self.save(&b)?;
                return Err(e);
            }
            // It may still land; the record settles it either way.
            Err(e) => log!("lend: send of a {:?}: {e:#}", built.pending.op),
        }
        Ok(true)
    }

    /// Saves a build before anyone can send it; on failure nothing is sent.
    fn record(&self, p: &Pending) -> Result<()> {
        let mut b = self.book()?;
        b.pending.push(p.clone());
        if let Err(e) = self.save(&b) {
            b.pending.pop();
            return Err(e);
        }
        Ok(())
    }

    /// With the test price only: posts the price `keeler lend-set-price`
    /// last named (its control file), else the one on chain again, whenever
    /// the chain's is older than `price_refresh_secs` or is not that price.
    /// True when it posted.
    pub fn refresh_price(&self) -> Result<bool> {
        if self.cfg.price != LendPrice::Test || self.cfg.mainnet() {
            return Ok(false);
        }
        let want = read_price_file(&price_file(&self.state))?;
        let vault = want.as_ref().map_or(vault1().id, |w| w.vault);
        let now = self.ledger.now()?;
        let have = self.ledger.quote(vault).ok();
        let (price, confidence) = match (&want, have) {
            (Some(w), _) => (w.price, w.confidence),
            (None, Some(q)) => {
                let conf = (q.high - q.low) / 2;
                (q.low + conf, conf)
            }
            (None, None) => return Ok(false),
        };
        // A standby posts only when the primary has not, 15 s late.
        let refresh = self.cfg.price_refresh()
            + if self.cfg.role == Role::Standby {
                15
            } else {
                0
            };
        let fresh = have.is_some_and(|q| (now as i64).saturating_sub(q.published) < refresh as i64);
        // A standby never posts over a fresh price: the primary's control
        // file, not the standby's, says what it is.
        let current = fresh
            && (self.cfg.role == Role::Standby
                || have.is_some_and(|q| {
                    q.low == price.saturating_sub(confidence)
                        && q.high == price.saturating_add(confidence)
                }));
        if current {
            return Ok(false);
        }
        let sig = self.ledger.set_price(vault, price, confidence)?;
        log!("lend: test price of vault {vault} posted at {price} +- {confidence}: {sig}");
        Ok(true)
    }

    /// Runs the queue until `stop`.
    pub fn run(&self, stop: &AtomicBool) {
        let every = Duration::from_secs(self.cfg.poll_secs.unwrap_or(2));
        while !stop.load(Ordering::Relaxed) {
            if let Err(e) = self.refresh_price() {
                log!("lend: test price refresh: {e:#}");
            }
            match self.step() {
                Ok(true) => continue,
                Ok(false) => {}
                Err(e) => log!("lend: {e:#}"),
            }
            let until = Instant::now() + every;
            while Instant::now() < until && !stop.load(Ordering::Relaxed) {
                sleep(Duration::from_millis(100));
            }
        }
    }

    // ---- HTTP ----

    /// One `/lend/*` request whose `x-keeler-key` and size already passed;
    /// `paused` is the Worker's `x-keeler-paused: 1`.
    pub fn http(&self, method: &str, url: &str, body: &[u8], paused: bool) -> (u16, Value) {
        let path = url.split('?').next().unwrap_or_default();
        let bad = || (400, "bad request".to_owned());
        let out = match (method, path) {
            ("GET", "/lend/pools") => self.pools().map_err(internal),
            ("POST", "/lend/account") => account_body(body)
                .ok_or_else(bad)
                .and_then(|(f, held)| self.account(f, &held)),
            ("POST", "/lend/ticket") => lend_auth::fields_from_json(body)
                .ok_or_else(bad)
                .and_then(|f| self.ticket(f, paused)),
            ("POST", "/lend/build") => lend_auth::fields_from_json(body)
                .ok_or_else(bad)
                .and_then(|f| self.build(f, paused)),
            _ => refuse(404, "not found"),
        };
        match out {
            Ok(v) => (200, v),
            Err((status, msg)) => (status, json!({ "error": msg })),
        }
    }

    fn authenticate(
        &self,
        fields: &BTreeMap<String, String>,
        method: &str,
        path: &str,
        allowed: &[&str],
    ) -> Result<(Address, u64), Refusal> {
        if let Some(k) = fields.keys().find(|k| {
            !lend_auth::SIGNED_FIELDS.contains(&k.as_str()) && !allowed.contains(&k.as_str())
        }) {
            return Err((400, format!("unknown field {k}")));
        }
        let now = self.ledger.now().map_err(internal)?;
        let mut nonces = self
            .nonces
            .lock()
            .map_err(|_| (500, "poisoned".to_owned()))?;
        let site = lend_auth::Site {
            network: &self.cfg.network,
            program: &self.program,
            origins: &self.cfg.origins,
        };
        let wallet = lend_auth::authenticate(fields, method, path, now, &site, &mut nonces)
            .map_err(|m| (401, m.to_owned()))?;
        drop(nonces);
        if !self.ledger.registered(&wallet).map_err(internal)? {
            return refuse(
                400,
                "this wallet is not in the user registry; register it first",
            );
        }
        Ok((wallet, now))
    }

    fn pools(&self) -> Result<Value> {
        let now = self.ledger.now()?;
        let bucket = now - now % self.cfg.limits.bucket_secs;
        let mut cache = self.figures.lock().map_err(|_| anyhow!("poisoned"))?;
        if let Some((at, v)) = &*cache {
            if *at == bucket {
                return Ok(v.clone());
            }
        }
        let reserve = self.snapshot()?.reserve;
        let pools = match reserve {
            None => Vec::new(),
            Some(n) => {
                let r = accrued(&n, bucket)?;
                let e = |e: core::LendError| anyhow!("{e}");
                let util = core::utilisation_bps(&r).map_err(e)?;
                let apr = core::apr_bps(r.rate_bps, util, core::TUSD_POOL.fee_bps).map_err(e)?;
                // The last pass's figures (none yet: zero).
                let s = self
                    .solvency
                    .lock()
                    .map_err(|_| anyhow!("poisoned"))?
                    .unwrap_or_default();
                vec![json!({
                    "surplus": coarse(s.surplus).to_string(),
                    "badDebt": coarse(s.bad_debt).to_string(),
                    "mint": vault1().debt_mint.to_string(),
                    "at": bucket,
                    "size": coarse(core::total_supply(&r).map_err(e)?).to_string(),
                    "utilisationBps": coarse(u64::from(util)).min(10_000),
                    "supplyRateBps": coarse(u64::from(apr.supply_bps)),
                    "borrowRateBps": coarse(u64::from(apr.borrow_bps)),
                })]
            }
        };
        let v = json!({ "pools": pools });
        *cache = Some((bucket, v.clone()));
        Ok(v)
    }

    /// Peer tickets the page holds that this keeper does not know: each one
    /// valid and live is taken on, so its intent fills with its keeper gone.
    fn adopt(&self, wallet: &Address, held: &[Value], now: u64) -> Result<()> {
        let w = wallet.to_string();
        let mut b = self.book()?;
        let before = b.tickets.len();
        for h in held {
            if let Some(t) = self.held_ticket(&w, h, now) {
                if !b.tickets.iter().any(|k| k.id == t.id) {
                    log!(
                        "lend: took on ticket {} of {w} from keeper {}",
                        t.id,
                        t.keeper.as_deref().unwrap_or("self")
                    );
                    b.tickets.push(t);
                }
            }
        }
        if b.tickets.len() > before {
            if let Err(e) = self.save(&b) {
                b.tickets.truncate(before);
                return Err(e);
            }
        }
        Ok(())
    }

    /// A ticket as the page holds it, when a keeper of the set signed it for
    /// `wallet` and it can still be filled.
    fn held_ticket(&self, wallet: &str, h: &Value, now: u64) -> Option<Ticket> {
        let s = |k: &str| h.get(k).and_then(Value::as_str);
        let num = |k: &str| decimal(s(k).map(str::to_owned).as_ref());
        let keeper = parse_address(s("keeper")?).ok()?;
        let (purpose, target) = match s("purpose")? {
            "supply" if s("pool") == Some(&vault1().debt_mint.to_string()) => {
                (purpose::SUPPLY, keeler_lend::pool::TUSD)
            }
            p @ ("deposit" | "repay") if h.get("vault").and_then(Value::as_u64) == Some(1) => (
                if p == "deposit" {
                    purpose::DEPOSIT
                } else {
                    purpose::REPAY
                },
                num("position").filter(|p| *p < 1 << 63)?,
            ),
            _ => return None,
        };
        let expiry = h.get("expiry").and_then(Value::as_u64)?;
        if expiry <= now + FILL_MARGIN || expiry > now + 900 {
            return None;
        }
        let t = Ticket {
            id: s("id")?.to_owned(),
            wallet: wallet.to_owned(),
            purpose,
            target,
            amount: num("amount")?,
            expiry,
            issued: now,
            signature: s("signature")?.to_owned(),
            status: TicketStatus::Open,
            attempts: 0,
            retry_at: 0,
            seen: false,
            keeper: (keeper != self.keepers[0]).then(|| keeper.to_string()),
        };
        ticket_valid(&t, &self.keepers, &self.program).then_some(t)
    }

    fn account(&self, fields: BTreeMap<String, String>, held: &[Value]) -> Result<Value, Refusal> {
        let (wallet, now) = self.authenticate(&fields, "POST", "/lend/account", &[])?;
        self.adopt(&wallet, held, now).map_err(internal)?;
        let book = self.snapshot().map_err(internal)?;
        let mine = |list: &[Note]| -> Vec<Note> {
            let mut v: Vec<Note> = list
                .iter()
                .filter(|n| n.owned_by(&wallet))
                .cloned()
                .collect();
            v.sort_by_key(|n| std::cmp::Reverse(n.amount));
            v.truncate(MAX_LISTED);
            v
        };
        let mut receipts = Vec::new();
        for n in mine(&book.receipts) {
            let s: ReceiptState = n.state().map_err(internal)?;
            receipts.push(json!({
                "pool": vault1().debt_mint.to_string(),
                "shares": s.supply_shares.to_string(),
                "message": n.message().map_err(internal)?,
            }));
        }
        let mut positions = Vec::new();
        for n in mine(&book.positions) {
            let s: PositionState = n.state().map_err(internal)?;
            positions.push(json!({
                "vault": s.vault,
                "position": s.position_id.to_string(),
                "collateral": n.amount.to_string(),
                "debtShares": s.debt_shares.to_string(),
                "message": n.message().map_err(internal)?,
            }));
        }
        let mut tickets: Vec<&Ticket> = book
            .tickets
            .iter()
            .filter(|t| t.wallet == wallet.to_string())
            .collect();
        tickets.sort_by_key(|t| std::cmp::Reverse(t.issued));
        let intents: Vec<Value> = tickets
            .iter()
            .take(MAX_LISTED)
            .map(|t| {
                let status = match t.status {
                    TicketStatus::Filled => "filled",
                    _ if now > t.expiry => "refundable",
                    _ => "waiting",
                };
                json!({
                    "id": t.id,
                    "purpose": purpose_name(t.purpose),
                    "amount": t.amount.to_string(),
                    "expiry": t.expiry,
                    "status": status,
                })
            })
            .collect();
        // The pool's price indices now, so the page values receipts and debt.
        let index = match &book.reserve {
            Some(r) => {
                let r = accrued(r, now).map_err(internal)?;
                json!({
                    "supply": r.supply_price.to_string(),
                    "borrow": r.borrow_price.to_string(),
                    "at": now,
                })
            }
            None => Value::Null,
        };
        let mut out = json!({ "receipts": receipts, "positions": positions, "intents": intents });
        if !index.is_null() {
            out["index"] = index;
        }
        Ok(out)
    }

    fn ticket(&self, fields: BTreeMap<String, String>, header: bool) -> Result<Value, Refusal> {
        let (wallet, now) = self.authenticate(
            &fields,
            "POST",
            "/lend/ticket",
            &["purpose", "amount", "pool", "vault", "position"],
        )?;
        let amount = decimal(fields.get("amount")).ok_or((400, "bad amount".to_owned()))?;
        let paused = self.paused_now(header);
        let l = &self.cfg.limits;
        let w = wallet.to_string();
        // Checked and issued under the book lock: racing requests cannot
        // pass a cap together.
        let mut b = self.book().map_err(internal)?;
        let (purpose, target) = {
            let book = &*b;
            let has_position = |wallet: &str, id: u64| {
                book.positions.iter().any(|p| {
                    p.owned_by_str(wallet)
                        && p.state::<PositionState>()
                            .is_ok_and(|s| s.position_id == id)
                })
            };
            // Repays and deposits into a position wait for nobody's tickets.
            let exempt = |t: &Ticket| {
                t.purpose == purpose::REPAY
                    || (t.purpose == purpose::DEPOSIT && has_position(&t.wallet, t.target))
            };
            let mine: Vec<&Ticket> = book.tickets.iter().filter(|t| t.wallet == w).collect();
            let my_open = |p: u8| mine.iter().filter(move |t| t.open(now) && t.purpose == p);
            let position_field = || -> Result<u64, Refusal> {
                if fields.get("vault").map(String::as_str) != Some("1") {
                    return refuse(404, "no such vault");
                }
                decimal(fields.get("position"))
                    .filter(|p| *p < 1 << 63)
                    .ok_or((400, "bad position".to_owned()))
            };
            let (purpose, target) = match fields.get("purpose").map(String::as_str) {
                Some("supply") => {
                    if fields.get("pool") != Some(&vault1().debt_mint.to_string()) {
                        return refuse(404, "no such pool");
                    }
                    if paused {
                        return refuse(403, "lending is paused: no new supply");
                    }
                    if amount < self.cfg.min_supply() {
                        return refuse(422, "below the pool's minimum supply");
                    }
                    // Tickets out count too: each may become a receipt.
                    let held = book.receipts.iter().filter(|n| n.owned_by(&wallet)).count()
                        + my_open(purpose::SUPPLY).count();
                    if held >= MAX_LISTED {
                        return refuse(429, "this wallet holds the most receipts allowed");
                    }
                    (purpose::SUPPLY, keeler_lend::pool::TUSD)
                }
                Some("deposit") => {
                    let id = position_field()?;
                    if has_position(&w, id) {
                        if amount < l.min_deposit_add.max(1) {
                            return refuse(422, "below the minimum deposit");
                        }
                    } else {
                        if paused {
                            return refuse(403, "lending is paused: no new positions");
                        }
                        if amount < self.cfg.min_collateral() {
                            return refuse(422, "below the vault's minimum collateral");
                        }
                        let held = book
                            .positions
                            .iter()
                            .filter(|n| n.owned_by(&wallet))
                            .count()
                            + my_open(purpose::DEPOSIT)
                                .filter(|t| !has_position(&w, t.target))
                                .count();
                        if held >= MAX_LISTED {
                            return refuse(429, "this wallet holds the most positions allowed");
                        }
                    }
                    (purpose::DEPOSIT, id)
                }
                Some("repay") => {
                    let id = position_field()?;
                    let has_debt = book
                        .positions
                        .iter()
                        .find(|p| {
                            p.owned_by(&wallet)
                                && p.state::<PositionState>()
                                    .is_ok_and(|s| s.position_id == id)
                        })
                        .and_then(|p| p.state::<PositionState>().ok())
                        .is_some_and(|s| s.debt_shares > 0);
                    if !has_debt {
                        return refuse(404, "no such position with debt");
                    }
                    if amount < l.min_repay {
                        return refuse(422, "below the minimum repayment");
                    }
                    let r = book
                        .reserve
                        .as_ref()
                        .map(|n| accrued(n, now))
                        .transpose()
                        .map_err(internal)?
                        .ok_or((503, "the pool is not open yet".to_owned()))?;
                    let shares = book
                        .positions
                        .iter()
                        .find(|p| {
                            p.owned_by(&wallet)
                                && p.state::<PositionState>()
                                    .is_ok_and(|s| s.position_id == id)
                        })
                        .and_then(|p| p.state::<PositionState>().ok())
                        .map_or(0, |s| s.debt_shares);
                    if !core::repay_shares(shares, amount, r.borrow_price)
                        .is_ok_and(|left| left.shares < shares)
                    {
                        return refuse(422, "this repayment is worth less than one debt share");
                    }
                    // A part repayment leaves no dust debt under the limit.
                    if l.min_position_debt > 0 {
                        let debt = core::debt_of(shares, r.borrow_price)
                            .map_err(|e| internal(anyhow!("{e}")))?;
                        if amount < debt && debt - amount < l.min_position_debt {
                            return refuse(
                                422,
                                "a part repayment must leave at least the minimum debt; repay it all",
                            );
                        }
                    }
                    (purpose::REPAY, id)
                }
                _ => return refuse(400, "bad purpose"),
            };
            // Caps count the open tickets too: each may still fill. Across
            // wallets only those whose intent is on chain: unfunded tickets
            // must not fill a cap for others. A wallet's own all count.
            let open: Vec<&Ticket> = book
                .tickets
                .iter()
                .filter(|t| t.open(now) && t.purpose == purpose && (t.seen || t.wallet == w))
                .collect();
            let tickets = |f: &dyn Fn(&Ticket) -> bool| -> u128 {
                open.iter()
                    .filter(|t| f(t))
                    .map(|t| u128::from(t.amount))
                    .sum()
            };
            let a = u128::from(amount);
            if purpose == purpose::SUPPLY {
                let r = book
                    .reserve
                    .as_ref()
                    .map(|n| accrued(n, now))
                    .transpose()
                    .map_err(internal)?;
                let pool = r
                    .as_ref()
                    .map_or(Ok(0), core::total_supply)
                    .map_err(|e| internal(anyhow!("{e}")))?;
                if !within(l.max_pool_size, u128::from(pool) + tickets(&|_| true) + a) {
                    return refuse(422, "the pool is at its size limit");
                }
                let held: u128 = book
                    .receipts
                    .iter()
                    .filter(|n| n.owned_by(&wallet))
                    .filter_map(|n| {
                        let s: ReceiptState = n.state().ok()?;
                        core::assets_of(s.supply_shares, r.as_ref()?.supply_price).ok()
                    })
                    .map(u128::from)
                    .sum();
                if !within(
                    l.max_supply_per_wallet,
                    held + tickets(&|t| t.wallet == w) + a,
                ) {
                    return refuse(422, "this wallet has supplied the most allowed");
                }
            }
            if purpose == purpose::DEPOSIT {
                let total: u128 = book.positions.iter().map(|p| u128::from(p.amount)).sum();
                // A top-up may save a position: never held to the total.
                if !has_position(&w, target)
                    && !within(l.max_total_collateral, total + tickets(&|_| true) + a)
                {
                    return refuse(422, "the vault is at its collateral limit");
                }
                let this: u128 = book
                    .positions
                    .iter()
                    .filter(|p| {
                        p.owned_by(&wallet)
                            && p.state::<PositionState>()
                                .is_ok_and(|s| s.position_id == target)
                    })
                    .map(|p| u128::from(p.amount))
                    .sum();
                let more = tickets(&|t| t.wallet == w && t.target == target);
                if !within(l.max_position_collateral, this + more + a) {
                    return refuse(422, "more collateral than one position may hold");
                }
            }
            // Only this keeper's own tickets count against its rate.
            if mine
                .iter()
                .filter(|t| t.keeper.is_none() && t.issued + HOUR > now)
                .count()
                >= l.tickets_per_wallet_hour as usize
            {
                return refuse(429, "too many requests from this wallet this hour");
            }
            if mine.iter().filter(|t| t.open(now)).count() >= l.open_per_wallet as usize {
                return refuse(429, "this wallet has too many open requests");
            }
            let this_exempt = purpose == purpose::REPAY
                || (purpose == purpose::DEPOSIT && has_position(&w, target));
            // Only tickets with an intent on chain fill the queue.
            let queued = book
                .tickets
                .iter()
                .filter(|t| t.open(now) && t.seen && !exempt(t))
                .count();
            if !this_exempt && queued >= l.max_open_intents as usize {
                return refuse(429, "the keeper is busy; try again shortly");
            }
            (purpose, target)
        };
        let id: [u8; 16] = random().map_err(internal)?;
        let expiry = now + l.ticket_secs;
        let m =
            lend_auth::ticket_bytes(&self.program, &wallet, &id, purpose, target, amount, expiry);
        let signature = to_hex(self.signer.sign_message(&m).as_ref());
        let t = Ticket {
            id: to_hex(&id),
            wallet: w,
            purpose,
            target,
            amount,
            expiry,
            issued: now,
            signature: signature.clone(),
            status: TicketStatus::Open,
            attempts: 0,
            retry_at: 0,
            seen: false,
            keeper: None,
        };
        b.tickets.push(t.clone());
        if let Err(e) = self.save(&b) {
            b.tickets.pop();
            return Err(internal(e));
        }
        Ok(json!({
            "id": t.id,
            "expiry": expiry,
            "signature": signature,
            "keeper": self.keepers[0].to_string(),
        }))
    }

    /// Counts `r`, once it has lapsed, as a strike against its wallet; a
    /// first count also lets every waiting fill go before the next owner
    /// build.
    fn strike_lapsed(&self, r: Option<&Reservation>, now: u64) {
        let Some(r) = r.filter(|r| r.until <= now && r.until + HOUR > now) else {
            return;
        };
        let Ok(mut strikes) = self.strikes.lock() else {
            return;
        };
        strikes.retain(|_, at| {
            at.retain(|t| t + HOUR > now);
            !at.is_empty()
        });
        let at = strikes.entry(r.wallet.clone()).or_default();
        if !at.contains(&r.until) {
            at.push(r.until);
            self.lapsed_unused.store(true, Ordering::Relaxed);
        }
    }

    /// After an unused lapse, any waiting fill goes before owner builds.
    fn fills_go_first(&self) -> bool {
        self.lapsed_unused.load(Ordering::Relaxed) && self.fills_waiting.load(Ordering::Relaxed)
    }

    /// An owner build of `p`'s wallet landed: the reservation it was built
    /// under was used after all.
    fn forgive(&self, p: &Pending) {
        let held = self.cfg.limits.reservation_secs;
        if let Ok(mut strikes) = self.strikes.lock() {
            if let Some(at) = strikes.get_mut(&p.wallet) {
                at.retain(|t| !(p.now < *t && *t <= p.now + held));
            }
        }
    }

    /// Whether `wallet` let reservations lapse unused so recently that it
    /// may not take the pool again yet: [`cooldown_secs`] after the last.
    fn cooling(&self, wallet: &str, now: u64) -> Result<bool, Refusal> {
        let strikes = self
            .strikes
            .lock()
            .map_err(|_| (500, "poisoned".to_owned()))?;
        let recent: Vec<u64> = strikes
            .get(wallet)
            .map(|at| at.iter().copied().filter(|t| t + HOUR > now).collect())
            .unwrap_or_default();
        Ok(recent.iter().max().is_some_and(|last| {
            now < last + cooldown_secs(self.cfg.limits.reservation_secs, recent.len())
        }))
    }

    /// Refuses (429) a wallet that had `builds_per_wallet_hour` builds in
    /// the hour before `asked`; with `charge`, counts this one.
    fn hourly(&self, wallet: &str, asked: u64, charge: bool) -> Result<(), Refusal> {
        let mut builds = self
            .builds
            .lock()
            .map_err(|_| (500, "poisoned".to_owned()))?;
        builds.retain(|_, t| t.last().is_some_and(|t| t + HOUR > asked));
        let mine = builds.entry(wallet.to_owned()).or_default();
        mine.retain(|t| t + HOUR > asked);
        if mine.len() >= self.cfg.limits.builds_per_wallet_hour as usize {
            return refuse(429, "too many requests from this wallet this hour");
        }
        if charge {
            mine.push(asked);
        }
        Ok(())
    }

    fn build(&self, fields: BTreeMap<String, String>, header: bool) -> Result<Value, Refusal> {
        let started = Instant::now();
        let (wallet, asked) = self.authenticate(
            &fields,
            "POST",
            "/lend/build",
            &[
                "op", "pool", "vault", "position", "amount", "shares", "borrow", "withdraw",
                "receipt",
            ],
        )?;
        let l = &self.cfg.limits;
        let op = fields.get("op").map(String::as_str);
        let num = |k: &str| decimal(fields.get(k)).ok_or((400, format!("bad {k}")));
        let mut shares = None;
        // redeem: the receipt the page expects spent, by note hash (optional).
        let named = match fields.get("receipt") {
            Some(h) if op == Some("redeem") => {
                Some(hex32(h).map_err(|_| (400, "bad receipt".to_owned()))?)
            }
            Some(_) => return refuse(400, "receipt is for a redeem"),
            None => None,
        };
        let (borrow, withdraw, mut amount) = match op {
            Some("redeem") => {
                if fields.get("pool") != Some(&vault1().debt_mint.to_string()) {
                    return refuse(404, "no such pool");
                }
                // Exactly one of `amount` and `shares`.
                match (fields.contains_key("amount"), fields.contains_key("shares")) {
                    (true, false) => (0, 0, num("amount")?),
                    (false, true) => {
                        shares = Some(num("shares")?);
                        if shares == Some(0) {
                            return refuse(400, "bad shares");
                        }
                        (0, 0, 0)
                    }
                    _ => return refuse(400, "give amount or shares, not both"),
                }
            }
            Some("draw") => {
                if fields.get("vault").map(String::as_str) != Some("1") {
                    return refuse(404, "no such vault");
                }
                (num("borrow")?, num("withdraw")?, 0)
            }
            _ => return refuse(400, "bad op"),
        };
        let id = match op {
            Some("draw") => Some(num("position")?),
            _ => None,
        };
        if borrow > 0 && self.paused_now(header) {
            return refuse(403, "lending is paused: no new borrowing");
        }
        if borrow > 0 && self.halted() {
            return refuse(403, BORROWING_HALTED);
        }
        let w = wallet.to_string();
        let is_named = |n: &Note| named.is_none_or(|h| n.hash().is_ok_and(|m| m == h));
        let is_position = |p: &Note| {
            p.owned_by(&wallet)
                && p.state::<PositionState>()
                    .is_ok_and(|s| Some(s.position_id) == id)
        };
        // Nothing of this wallet's to spend: no lane, no proof.
        {
            let book = self.snapshot().map_err(internal)?;
            match id {
                None if !book
                    .receipts
                    .iter()
                    .any(|n| n.owned_by(&wallet) && is_named(n)) =>
                {
                    return refuse(404, "no such receipt")
                }
                Some(_) if !book.positions.iter().any(is_position) => {
                    return refuse(404, "no such position")
                }
                _ => {}
            }
        }
        self.hourly(&w, asked, false)?;
        // Wait for the lane, briefly.
        let _lane = loop {
            match self.lane.try_lock() {
                Ok(g) => break g,
                Err(_) if started.elapsed() < LANE_WAIT => sleep(Duration::from_millis(50)),
                Err(_) => return refuse(409, "the pool is in use; try again shortly"),
            }
        };
        let now = self.ledger.now().map_err(internal)?;
        let book = self.snapshot().map_err(internal)?;
        self.strike_lapsed(book.reservation.as_ref(), now);
        if book.in_flight()
            || book.reserved_for_other(&w, now)
            || self.priority_waiting.load(Ordering::Relaxed)
            || self.fills_go_first()
            || self.cooling(&w, now)?
        {
            return refuse(409, "the pool is in use; try again shortly");
        }
        self.hourly(&w, asked, true)?;
        let reserve = book
            .reserve
            .clone()
            .ok_or((503, "the pool is not open yet".to_owned()))?;
        let r = accrued(&reserve, now).map_err(internal)?;
        let cerr = |e: core::LendError| -> Refusal {
            match e {
                core::LendError::InsufficientLiquidity | core::LendError::UtilisationCap => {
                    (410, "not enough in the pool".to_owned())
                }
                _ => (422, format!("refused: {e}")),
            }
        };
        // Liquidity is checked last, at the amount rounded up to two
        // significant figures: a refusal tells little about the pool.
        let (job, index, bound) = if op == Some("redeem") {
            // The smallest receipt that covers the amount.
            let mut held: Vec<(u64, &Note)> = book
                .receipts
                .iter()
                .filter(|n| n.owned_by(&wallet) && is_named(n))
                .filter_map(|n| {
                    let s: ReceiptState = n.state().ok()?;
                    Some((core::assets_of(s.supply_shares, r.supply_price).ok()?, n))
                })
                .collect();
            if held.is_empty() {
                return refuse(404, "no such receipt");
            }
            held.sort_by_key(|(a, _)| *a);
            let shares_of = |n: &Note| n.state::<ReceiptState>().map_or(0, |s| s.supply_shares);
            let (receipt, whole) = match shares {
                // The receipt holding exactly that many shares, else the
                // smallest holding more.
                Some(want) => {
                    let mut fit: Vec<&Note> = held
                        .iter()
                        .map(|(_, n)| *n)
                        .filter(|n| shares_of(n) >= want)
                        .collect();
                    fit.sort_by_key(|n| (shares_of(n) != want, shares_of(n)));
                    let receipt = *fit
                        .first()
                        .ok_or((422, "more shares than one receipt holds".to_owned()))?;
                    let state: ReserveState = reserve.state().map_err(internal)?;
                    amount = keeler_lend_sdk::builders::redeem_amount_for_shares(&state, want, now)
                        .map_err(|e| (422, format!("refused: {e}")))?;
                    (receipt, shares_of(receipt) == want)
                }
                None => {
                    let (worth, receipt) = held
                        .iter()
                        .find(|(a, _)| *a >= amount)
                        .ok_or((422, "more than one receipt holds".to_owned()))?;
                    (*receipt, *worth == amount)
                }
            };
            if amount < l.min_redeem && !whole {
                return refuse(422, "below the minimum redeem");
            }
            let s: ReceiptState = receipt.state().map_err(internal)?;
            if coarse_up(amount) > reserve.amount {
                return refuse(410, "not enough in the pool");
            }
            core::redeem(
                &r,
                reserve.amount,
                s.supply_shares,
                amount,
                &core::TUSD_POOL.curve,
            )
            .map_err(cerr)?;
            (
                Job::Redeem {
                    receipt: receipt.clone(),
                    amount,
                },
                r.supply_price,
                None,
            )
        } else {
            let position = book
                .positions
                .iter()
                .find(|p| is_position(p))
                .ok_or((404, "no such position".to_owned()))?;
            if borrow == 0 && withdraw == 0 {
                return refuse(422, "nothing to draw");
            }
            let left = position
                .amount
                .checked_sub(withdraw)
                .ok_or((422, "more than the position holds".to_owned()))?;
            if borrow > 0 && borrow < l.min_borrow {
                return refuse(422, "below the minimum borrow");
            }
            if withdraw > 0 && withdraw < l.min_withdraw && left != 0 {
                return refuse(422, "below the minimum withdrawal");
            }
            // A missing or unverified price account is the oracle's state,
            // not the keeper's fault.
            let q = self.ledger.quote(vault1().id).map_err(|e| {
                log!("lend: no price for a draw: {e:#}");
                (412, "the oracle price is unavailable".to_owned())
            })?;
            if (now as i64).abs_diff(q.published) > PRICE_FRESH {
                return refuse(412, "the oracle price is too old");
            }
            if borrow > 0
                && solvency(&book, reserve.amount, &r, q.low)
                    .map_err(internal)?
                    .halts()
            {
                return refuse(403, BORROWING_HALTED);
            }
            let m = u128::from(l.bound_margin_bps);
            let bound = u64::try_from(u128::from(q.low) * (10_000 - m) / 10_000)
                .map_err(|e| internal(e.into()))?;
            let s: PositionState = position.state().map_err(internal)?;
            let mut shares = s.debt_shares;
            if borrow > 0 {
                shares += core::borrow_shares(borrow, r.borrow_price).map_err(cerr)?;
            }
            let debt = core::debt_of(shares, r.borrow_price).map_err(cerr)?;
            let v = vault1();
            if left != 0 && left < v.min_collateral {
                return refuse(422, "below the vault's minimum collateral");
            }
            // The keeper's own floor binds a borrow only: a withdrawal from a
            // position left under it (a part liquidation, a raised limit) goes on.
            let floor = if borrow > 0 {
                v.min_debt.max(l.min_position_debt)
            } else {
                v.min_debt
            };
            if debt != 0 && debt < floor {
                return refuse(422, "below the vault's minimum debt");
            }
            if borrow > 0 && !within(l.max_position_debt, u128::from(debt)) {
                return refuse(422, "more debt than one position may hold");
            }
            let value = core::collateral_value(left, bound).map_err(cerr)?;
            if !core::can_draw(debt, value, &core::TSOL_TUSD_VAULT) {
                return refuse(423, "the position would be unhealthy");
            }
            if borrow > 0 {
                let curve = &core::TUSD_POOL.curve;
                core::borrow(&r, reserve.amount, coarse_up(borrow), curve).map_err(cerr)?;
                core::borrow(&r, reserve.amount, borrow, curve).map_err(cerr)?;
            }
            (
                Job::Draw {
                    position: position.clone(),
                    borrow,
                    withdraw,
                    bound,
                },
                r.borrow_price,
                Some(bound),
            )
        };
        let built = self.ledger.build(&job, Some(&reserve), now).map_err(|e| {
            log!("lend: build for {wallet}: {e:#}");
            (422, "the keeper could not build this operation".to_owned())
        })?;
        // Nobody waits for it any more: it is never handed out, so it needs
        // no record.
        if started.elapsed() > BUILD_DEADLINE {
            return Err(internal(anyhow!("a build for {wallet} took too long")));
        }
        let mut pending = built.pending;
        pending.owner_signed = true;
        let mut b = self.book().map_err(internal)?;
        b.pending.push(pending.clone());
        let until = match &b.reservation {
            Some(r) if r.wallet == w && r.until > now => r.until,
            _ => now + l.reservation_secs,
        };
        let before = b.reservation.replace(Reservation { wallet: w, until });
        if let Err(e) = self.save(&b) {
            b.pending.pop();
            b.reservation = before;
            return Err(internal(e));
        }
        let mut reply = json!({
            "transaction": B64.encode(&built.tx),
            "outputs": pending.outputs,
            "now": pending.now,
            "index": built.index.unwrap_or(index).to_string(),
            "keeper": self.keepers[0].to_string(),
        });
        if let Some(bound) = bound {
            reply["bound"] = json!(bound.to_string());
        }
        Ok(reply)
    }
}

/// Applies a landed build: its inputs leave the book, its notes join it.
/// A receipt redeemed in full holds nothing; a position left with nothing in
/// it is closed by its owner, if at all. Neither is kept in the book.
fn holds_something(n: &Note) -> bool {
    match n.kind {
        KIND_RECEIPT => n.state::<ReceiptState>().is_ok_and(|s| s.supply_shares > 0),
        KIND_POSITION => {
            n.amount > 0 || n.state::<PositionState>().is_ok_and(|s| s.debt_shares > 0)
        }
        _ => true,
    }
}

fn apply(book: &mut Book, p: &Pending, outs: &[([u8; 32], u64)]) {
    let spent = |n: &Note| p.inputs.contains(&n.nullifier);
    if book.reserve.as_ref().is_some_and(spent) {
        book.reserve = None;
    }
    book.receipts.retain(|n| !spent(n));
    book.positions.retain(|n| !spent(n));
    for n in &p.notes {
        let mut n = n.clone();
        n.leaf = n
            .hash()
            .ok()
            .and_then(|h| outs.iter().find(|(o, _)| *o == h).map(|(_, l)| *l));
        // A recovery may have found it on chain already.
        let known = |list: &[Note]| list.iter().any(|m| m.nullifier == n.nullifier);
        match n.kind {
            // A merge may have installed a newer one already (its spender's).
            KIND_RESERVE if book.reserve.is_none() => book.reserve = Some(n),
            KIND_RECEIPT if holds_something(&n) && !known(&book.receipts) => book.receipts.push(n),
            KIND_POSITION if holds_something(&n) && !known(&book.positions) => {
                book.positions.push(n)
            }
            _ => {}
        }
    }
    if let Some(t) = book.ticket_mut(&p.ticket) {
        t.status = TicketStatus::Filled;
    }
    if p.owner_signed
        && book
            .reservation
            .as_ref()
            .is_some_and(|r| r.wallet == p.wallet)
    {
        book.reservation = None;
    }
    // Every other build on these inputs can no longer land.
    book.pending
        .retain(|q| q != p && !q.inputs.iter().any(|i| p.inputs.contains(i)));
    if p.op == Op::Liquidate {
        log!("lend: liquidated a position of {} in vault 1", p.wallet);
    } else {
        log!("lend: {:?} for {} landed", p.op, p.wallet);
    }
}

/// The keeper's own config, to run it without a swap dealer.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeeperConfig {
    pub network: crate::config::Network,
    pub key: KeySource,
    pub http: crate::config::Http,
    pub lend: Lend,
}

impl KeeperConfig {
    pub fn load(path: &str) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("read config {path}"))?;
        let cfg: Self = serde_json::from_str(&text).map_err(|e| {
            anyhow!(
                "invalid config {path} ({:?} error at line {} column {})",
                e.classify(),
                e.line(),
                e.column()
            )
        })?;
        ensure!(
            cfg.http.listen.ip().is_loopback(),
            "http.listen must be a loopback address"
        );
        cfg.check()?;
        Ok(cfg)
    }

    pub fn check(&self) -> Result<()> {
        self.network.check()?;
        self.lend.check()?;
        ensure!(
            !self.lend.mainnet() || !self.http.key.is_file(),
            "on mainnet the HTTP key comes from the Keychain, not a file"
        );
        Ok(())
    }
}

/// What a running keeper borrows: its key, its chain, its state file.
pub struct Parts {
    pub signer: Keypair,
    pub chain: crate::lend_chain::LendChain,
    pub state: PathBuf,
}

/// Loads the keeper key (`lend.key`, else `default`) and the pool key
/// (`lend.pool_key`, else the keeper key), checks them against the program's
/// KEEPERS and POOL_VIEWING and the chain against `lend.network`, and
/// connects.
pub fn connect(
    lend: &Lend,
    network: &crate::config::Network,
    default: &KeySource,
) -> Result<Parts> {
    let key_source = lend.key.as_ref().unwrap_or(default);
    let pool_source = lend.pool_key.as_ref().unwrap_or(key_source);
    if lend.mainnet() {
        let files = [Some(key_source), Some(pool_source), lend.admin_key.as_ref()];
        ensure!(
            !files.iter().flatten().any(|k| k.is_file()),
            "on mainnet the keeper, pool and admin keys come from the Keychain, not a file"
        );
    }
    let signer = key_source.keypair()?;
    ensure!(
        keeler_lend::KEEPERS.contains(&signer.pubkey()),
        "the keeper key {} is not one of the program's KEEPERS; {}",
        signer.pubkey(),
        built_with()
    );
    let pool = pool_source.keypair()?;
    let kp = keeler_lend_sdk::keeper_keypair(&signer, &pool)?;
    ensure!(
        kp.viewing_pubkey().as_bytes() == &keeler_lend::POOL_VIEWING,
        "the pool key does not give the program's POOL_VIEWING; {}",
        built_with()
    );
    ensure!(
        !lend.mainnet() || !keeler_lend::KEEPERS.contains(&pool.pubkey()),
        "on mainnet the pool key must not be a keeper key"
    );
    let liquidation_fee = lend
        .liquidation_priority_fee
        .unwrap_or(network.priority_fee);
    let chain =
        crate::lend_chain::LendChain::connect(network, &signer, kp, lend.price, liquidation_fee)?;
    check_cluster(lend, chain.chain.cluster)?;
    let state = lend.state_path(&signer.pubkey())?;
    Ok(Parts {
        signer,
        chain,
        state,
    })
}

/// What this binary was compiled with, for a refusal: the keeper builds
/// with the program's parameters (`build-params.json`) in its environment.
fn built_with() -> String {
    let keepers: Vec<String> = keeler_lend::KEEPERS
        .iter()
        .map(Address::to_string)
        .collect();
    format!(
        "this binary was built with KEELER_LEND_PROGRAM_ID={} KEELER_LEND_KEEPERS={} \
         KEELER_LEND_POOL_VIEWING={}; rebuild it with the deployed program's build parameters",
        keeler_lend::ID,
        keepers.join(","),
        to_hex(&keeler_lend::POOL_VIEWING)
    )
}

/// Refuses a chain whose genesis hash is not the network `lend.network`
/// names.
pub fn check_cluster(lend: &Lend, chain: crate::chain::Cluster) -> Result<()> {
    let named = crate::chain::Cluster::of_name(&lend.network);
    ensure!(
        named == chain,
        "lend.network is {} but the RPC serves a {chain:?} chain",
        lend.network
    );
    Ok(())
}

/// Runs the keeper and an HTTP server with only the lend routes (and
/// `/health`), until `stop`.
pub fn run_standalone(cfg: &KeeperConfig, stop: &AtomicBool) -> Result<()> {
    let key = cfg.http.key.secret()?;
    ensure!(
        key.len() >= 16,
        "the HTTP key must be at least 16 characters"
    );
    let p = connect(&cfg.lend, &cfg.network, &cfg.key)?;
    let keeper = Keeper::open(&cfg.lend, &p.chain, &p.signer, p.state.clone())?;
    let listener = std::net::TcpListener::bind(cfg.http.listen)?;
    let health = Health(&p.chain);
    log!("keeper: serving {}", cfg.http.listen);
    std::thread::scope(|s| {
        let worker = s.spawn(|| keeper.run(stop));
        let out = crate::dealer::serve_http(
            listener,
            &key,
            stop,
            &health,
            Some(&keeper),
            Duration::from_secs(3),
        );
        stop.store(true, Ordering::Relaxed);
        let _ = worker.join();
        out
    })
}

/// The swap routes of a keeper without a dealer: health only.
struct Health<'a, L>(&'a L);

impl<L: Ledger> crate::dealer::Api for Health<'_, L> {
    fn quote(&self, _: keeler_core::rfq::Rfq) -> Result<keeler_core::rfq::Quote, String> {
        Err("no quotes here".into())
    }
    fn faucet(&self, _: Address) -> Result<(), (u16, String)> {
        Err((404, "the faucet is off".into()))
    }
    fn now(&self) -> Result<u64, String> {
        self.0.now().map_err(|e| format!("{e:#}"))
    }
}

/// The lend routes as the HTTP server calls them.
pub trait LendRoutes: Sync {
    fn lend(&self, method: &str, url: &str, body: &[u8], paused: bool) -> (u16, Value);
}

impl<L: Ledger> LendRoutes for Keeper<'_, L> {
    fn lend(&self, method: &str, url: &str, body: &[u8], paused: bool) -> (u16, Value) {
        self.http(method, url, body, paused)
    }
}

/// A keeper that may still be starting: 503 until it is up, with a body the
/// Worker retries on the next keeper.
pub struct Slot<'a, L>(pub &'a OnceLock<Keeper<'a, L>>);

impl<L: Ledger> LendRoutes for Slot<'_, L> {
    fn lend(&self, method: &str, url: &str, body: &[u8], paused: bool) -> (u16, Value) {
        match self.0.get() {
            Some(k) => k.http(method, url, body, paused),
            // No `error` field: the Worker tries the next keeper.
            None => (
                503,
                json!({ "starting": "the keeper is starting or cannot start yet" }),
            ),
        }
    }
}

/// The keeper inside the dealer: connects and recovers in the background,
/// retrying with backoff, then runs the queue until `stop`. A failure here
/// leaves the swap dealer serving; lend answers 503 meanwhile.
pub fn run_in_dealer<'a>(
    lend: &'a Lend,
    network: &'a crate::config::Network,
    key: &'a KeySource,
    parts: &'a OnceLock<Parts>,
    keeper: &'a OnceLock<Keeper<'a, crate::lend_chain::LendChain>>,
    stop: &AtomicBool,
) {
    let mut delay = 5u64;
    while !stop.load(Ordering::Relaxed) {
        let up = (|| -> Result<()> {
            if parts.get().is_none() {
                let _ = parts.set(connect(lend, network, key)?);
            }
            let p = parts.get().context("no keeper parts")?;
            let k = Keeper::open(lend, &p.chain, &p.signer, p.state.clone())?;
            let _ = keeper.set(k);
            Ok(())
        })();
        match (up, keeper.get()) {
            (Ok(()), Some(k)) => {
                log!("lend: the keeper is up");
                return k.run(stop);
            }
            (up, _) => {
                let e = up.err().map(|e| format!("{e:#}")).unwrap_or_default();
                log!("lend: the keeper could not start (swaps go on); retry in {delay} s: {e}");
                let until = Instant::now() + Duration::from_secs(delay);
                while Instant::now() < until && !stop.load(Ordering::Relaxed) {
                    sleep(Duration::from_millis(100));
                }
                delay = (delay * 2).min(300);
            }
        }
    }
}

/// Fails unless the wallet is on a network where the test price exists.
pub fn require_test_network(lend: &Lend) -> Result<()> {
    if lend.mainnet() || lend.price != LendPrice::Test {
        bail!("set_test_price is for local networks and devnet, with lend.price \"test\"");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
