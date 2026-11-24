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
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, MutexGuard,
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
/// Failed builds of one ticket before it is given up (the maker refunds).
const MAX_ATTEMPTS: u32 = 3;
/// How long an HTTP build waits for the lane before answering busy.
const LANE_WAIT: Duration = Duration::from_secs(5);
/// A price older than this is not used for a draw (the program allows 90 s).
const PRICE_FRESH: u64 = 60;
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
    pub tickets_per_wallet_hour: u32,
    /// Open (unfilled, unexpired) tickets across all wallets.
    pub max_open_intents: u32,
    pub reservation_secs: u64,
    /// Ticket and intent lifetime; the page accepts 60 to 900 s.
    pub ticket_secs: u64,
    /// Pool figures are refreshed at most once per bucket.
    pub bucket_secs: u64,
    /// The draw bound sits this far under `price - confidence`, the
    /// liquidation bound this far over `price + confidence`.
    pub bound_margin_bps: u64,
}

impl Default for LendLimits {
    fn default() -> Self {
        Self {
            min_supply: 0,
            min_collateral: 0,
            min_deposit_add: 1,
            min_repay: 10_000,
            tickets_per_wallet_hour: 30,
            max_open_intents: 64,
            reservation_secs: 20,
            ticket_secs: 600,
            bucket_secs: 600,
            bound_margin_bps: 50,
        }
    }
}

/// The `lend` section of a dealer config, or of a keeper config.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lend {
    /// The lend program; must be the one this binary was built for.
    pub program_id: String,
    /// The name in the session text the wallet signs: `devnet`, `localnet`.
    pub network: String,
    /// The keeper key (the program's KEEPER); default: the config's key.
    #[serde(default)]
    pub key: Option<KeySource>,
    /// The admin key, only for `keeler lend-init-reserve`.
    #[serde(default)]
    pub admin_key: Option<KeySource>,
    /// Default `~/.local/state/keeler/keeper-<pubkey>.json`.
    #[serde(default)]
    pub state_file: Option<PathBuf>,
    /// While it exists: no supply, no new positions, no borrowing.
    #[serde(default)]
    pub pause_file: Option<PathBuf>,
    #[serde(default)]
    pub price: LendPrice,
    #[serde(default)]
    pub limits: LendLimits,
    /// Seconds between passes of the queue; default 2.
    #[serde(default)]
    pub poll_secs: Option<u64>,
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
            !self.network.is_empty() && !self.network.contains('\n'),
            "lend.network must be a plain name"
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
            self.price == LendPrice::Pyth || self.network != "mainnet",
            "the test price is for local networks and devnet"
        );
        Ok(())
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
        self.wallet.as_deref() == Some(&wallet.to_string())
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

/// The state file, with tickets this keeper did not sign dropped.
pub fn load_book(path: &Path, keeper: &Address, program: &Address) -> Result<Book> {
    let text = match std::fs::read(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Book::default()),
        Err(e) => return Err(e.into()),
    };
    let mut book: Book = serde_json::from_slice(&text)
        .map_err(|_| anyhow!("keeper state file {} is not valid", path.display()))?;
    let before = book.tickets.len();
    book.tickets.retain(|t| ticket_valid(t, keeper, program));
    if book.tickets.len() < before {
        log!(
            "lend: dropped {} tickets not signed by this keeper",
            before - book.tickets.len()
        );
    }
    Ok(book)
}

fn ticket_valid(t: &Ticket, keeper: &Address, program: &Address) -> bool {
    let (Ok(wallet), Some(id), Some(sig)) = (
        parse_address(&t.wallet),
        hex_bytes::<16>(&t.id),
        hex_bytes::<64>(&t.signature),
    ) else {
        return false;
    };
    let m = lend_auth::ticket_bytes(
        program, &wallet, &id, t.purpose, t.target, t.amount, t.expiry,
    );
    lend_auth::ticket_signed_by(keeper, &m, &sig)
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
    /// unspent.
    fn intents(&self, wallet: &Address) -> Result<Vec<Intent>>;
    fn quote(&self, vault: u64) -> Result<Quote>;
    /// Builds and proves `job` against `reserve` at chain time `now`.
    fn build(&self, job: &Job, reserve: Option<&Note>, now: u64) -> Result<Built>;
    /// Sends a keeper-built transaction; the signature.
    fn send(&self, tx: &[u8]) -> Result<String>;
    /// Every reserve, receipt and position the keeper's own transactions
    /// created, recovered from the chain with the pool viewing key and the
    /// writer key; spent ones included.
    fn history(&self) -> Result<Vec<Note>>;
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
}

type Refusal = (u16, String);

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

fn vault1() -> &'static keeler_lend::vaults::Vault {
    &keeler_lend::vaults::TSOL_TUSD
}

impl<'a, L: Ledger> Keeper<'a, L> {
    /// Loads the state file and reconciles it with the chain.
    pub fn open(cfg: &'a Lend, ledger: &'a L, signer: &'a Keypair, state: PathBuf) -> Result<Self> {
        let program = keeler_lend::ID;
        let book = load_book(&state, &signer.pubkey(), &program)?;
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
        };
        k.recover()?;
        Ok(k)
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
        let found = self.ledger.history().context("rebuild from the chain")?;
        let mut book = self.book()?;
        let mut all: Vec<Note> = book
            .reserve
            .iter()
            .chain(&book.receipts)
            .chain(&book.positions)
            .cloned()
            .collect();
        for n in found {
            if !all.iter().any(|m| m.nullifier == n.nullifier) {
                all.push(n);
            }
        }
        // Notes a still-pending build spends stay, whatever the chain says
        // now: resolution settles them.
        let nullifiers: Vec<[u8; 32]> = all.iter().map(Note::nullifier).collect::<Result<_>>()?;
        let spent = self.ledger.spent(&nullifiers)?;
        let live: Vec<Note> = all
            .into_iter()
            .zip(spent)
            .filter(|(_, s)| !s)
            .map(|(n, _)| n)
            .collect();
        let reserves: Vec<&Note> = live.iter().filter(|n| n.kind == KIND_RESERVE).collect();
        ensure!(
            reserves.len() <= 1,
            "more than one unspent reserve; the state file and the chain disagree"
        );
        book.reserve = reserves.first().map(|n| (*n).clone());
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
        if book.reserve.is_none() {
            log!("lend: no reserve found; run `keeler lend-init-reserve` once");
        }
        self.save(&book)?;
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

    /// Settles every pending build the chain has decided: the one whose
    /// notes the spending transaction appended landed; the others on those
    /// inputs, or past `now + DEAD_AFTER` with the input still unspent, are
    /// dropped. Spent notes nobody here spent (a close, a lost record) leave
    /// the book; a reserve spent that way is found again from the chain.
    pub fn resolve(&self) -> Result<()> {
        let now = self.ledger.now()?;
        let snapshot = self.snapshot()?;
        let keys: Vec<[u8; 32]> = snapshot
            .pending
            .iter()
            .map(Pending::key)
            .collect::<Result<_>>()?;
        let spent = if keys.is_empty() {
            Vec::new()
        } else {
            self.ledger.spent(&keys)?
        };
        let mut landed = Vec::new();
        let mut dead = Vec::new();
        for ((p, key), s) in snapshot.pending.iter().zip(&keys).zip(spent) {
            if !s {
                if now > p.now + DEAD_AFTER {
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
        for (p, outs) in landed {
            if book.pending.contains(&p) {
                apply(&mut book, &p, &outs);
            }
        }
        for p in dead {
            if let Some(i) = book.pending.iter().position(|q| *q == p) {
                book.pending.remove(i);
                if let Some(t) = book.ticket_mut(&p.ticket) {
                    t.attempts += 1;
                    if t.attempts >= MAX_ATTEMPTS && t.status == TicketStatus::Open {
                        t.status = TicketStatus::Failed;
                    }
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
        self.save(&book)?;
        drop(book);
        if lost_reserve {
            log!("lend: the reserve was spent outside any record; rebuilding from the chain");
            self.recover_reserve()?;
        }
        Ok(())
    }

    fn recover_reserve(&self) -> Result<()> {
        let found = self.ledger.history()?;
        let reserves: Vec<Note> = found
            .into_iter()
            .filter(|n| n.kind == KIND_RESERVE)
            .collect();
        let nullifiers: Vec<[u8; 32]> = reserves
            .iter()
            .map(Note::nullifier)
            .collect::<Result<_>>()?;
        let spent = if nullifiers.is_empty() {
            Vec::new()
        } else {
            self.ledger.spent(&nullifiers)?
        };
        let live: Vec<Note> = reserves
            .into_iter()
            .zip(spent)
            .filter(|(_, s)| !s)
            .map(|(n, _)| n)
            .collect();
        ensure!(live.len() <= 1, "more than one unspent reserve on chain");
        let mut book = self.book()?;
        book.reserve = live.into_iter().next();
        self.save(&book)
    }

    /// One pass: settle, then run the most urgent job, if any. True when a
    /// job ran.
    pub fn step(&self) -> Result<bool> {
        self.resolve()?;
        let now = self.ledger.now()?;
        let book = self.snapshot()?;
        let Some(reserve) = book.reserve.clone() else {
            return Ok(false);
        };
        if book.in_flight() {
            return Ok(false);
        }
        let r = accrued(&reserve, now)?;
        // 1. Liquidations, even while a wallet holds the reservation.
        if let Some(job) = self.liquidation(&book, &r, now)? {
            self.priority_waiting.store(true, Ordering::Relaxed);
            return self.execute(job, None).map(|_| true);
        }
        // 2. Fills, in order: repay, deposit, supply.
        let fills = self.fills(&book, now)?;
        self.priority_waiting.store(
            fills.iter().any(|(_, j)| !matches!(j, Job::Supply { .. })),
            Ordering::Relaxed,
        );
        if book.reserved(now) {
            return Ok(false);
        }
        for (ticket, job) in fills {
            match self.execute(job, Some(ticket.clone())) {
                Ok(_) => return Ok(true),
                Err(e) => {
                    log!("lend: fill of ticket {ticket} failed: {e:#}");
                    let mut b = self.book()?;
                    if let Some(t) = b.ticket_mut(&Some(ticket)) {
                        t.attempts += 1;
                        if t.attempts >= MAX_ATTEMPTS {
                            t.status = TicketStatus::Failed;
                        }
                    }
                    self.save(&b)?;
                }
            }
        }
        Ok(false)
    }

    /// The first position past the liquidation threshold at `price +
    /// confidence` plus the margin.
    fn liquidation(&self, book: &Book, r: &core::Reserve, now: u64) -> Result<Option<Job>> {
        let indebted: Vec<&Note> = book
            .positions
            .iter()
            .filter(|p| p.state::<PositionState>().is_ok_and(|s| s.debt_shares > 0))
            .collect();
        if indebted.is_empty() {
            return Ok(None);
        }
        let q = match self.ledger.quote(vault1().id) {
            Ok(q) => q,
            Err(e) => {
                log!("lend: no price for the liquidation watch: {e:#}");
                return Ok(None);
            }
        };
        if (now as i64).abs_diff(q.published) > keeler_lend::vaults::PRICE_MAX_AGE as u64 {
            log!("lend: the price is stale; no liquidation is possible");
            return Ok(None);
        }
        let m = u128::from(self.cfg.limits.bound_margin_bps);
        let bound = u64::try_from((u128::from(q.high) * (10_000 + m)).div_ceil(10_000))?;
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
            if worth {
                return Ok(Some(Job::Liquidate {
                    position: p.clone(),
                    bound,
                }));
            }
        }
        Ok(None)
    }

    /// Ticketed intents found on chain, most urgent first.
    fn fills(&self, book: &Book, now: u64) -> Result<Vec<(String, Job)>> {
        let open: Vec<&Ticket> = book
            .tickets
            .iter()
            .filter(|t| t.status == TicketStatus::Open && now + FILL_MARGIN < t.expiry)
            .collect();
        let mut wallets: Vec<&str> = open.iter().map(|t| t.wallet.as_str()).collect();
        wallets.sort_unstable();
        wallets.dedup();
        let paused = self.cfg.paused();
        let mut jobs = Vec::new();
        for w in wallets {
            let wallet = parse_address(w)?;
            let intents = match self.ledger.intents(&wallet) {
                Ok(i) => i,
                Err(e) => {
                    log!("lend: intents of {wallet}: {e:#}");
                    continue;
                }
            };
            for t in open.iter().filter(|t| t.wallet == w) {
                let mint = match t.purpose {
                    purpose::DEPOSIT => vault1().collateral_mint,
                    _ => vault1().debt_mint,
                };
                let Some(intent) = intents.iter().find(|i| {
                    i.purpose == t.purpose
                        && i.target == t.target
                        && i.expiry == t.expiry
                        && i.note.amount == t.amount
                        && i.note.mint == mint.to_string()
                }) else {
                    continue;
                };
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
                    (purpose::DEPOSIT, Some(position)) => Job::DepositAdd { position, intent },
                    (purpose::DEPOSIT, None) if !paused => Job::DepositOpen { intent },
                    (purpose::SUPPLY, _) if !paused => Job::Supply { intent },
                    _ => continue,
                };
                jobs.push((t.id.clone(), job));
            }
        }
        let rank = |j: &Job| match j {
            Job::Repay { .. } => 0,
            Job::DepositAdd { .. } | Job::DepositOpen { .. } => 1,
            _ => 2,
        };
        jobs.sort_by_key(|(_, j)| rank(j));
        Ok(jobs)
    }

    /// Builds, records, then sends a keeper-signed job.
    fn execute(&self, job: Job, ticket: Option<String>) -> Result<()> {
        let _lane = self.lane.lock().map_err(|_| anyhow!("lane poisoned"))?;
        let now = self.ledger.now()?;
        let reserve = self.snapshot()?.reserve;
        let mut built = self.ledger.build(&job, reserve.as_ref(), now)?;
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
            // It may still land; the record settles it either way.
            Err(e) => log!("lend: send of a {:?}: {e:#}", built.pending.op),
        }
        Ok(())
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

    /// Runs the queue until `stop`.
    pub fn run(&self, stop: &AtomicBool) {
        let every = Duration::from_secs(self.cfg.poll_secs.unwrap_or(2));
        while !stop.load(Ordering::Relaxed) {
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

    /// One `/lend/*` request whose `x-keeler-key` and size already passed.
    pub fn http(&self, method: &str, url: &str, body: &[u8]) -> (u16, Value) {
        let path = url.split('?').next().unwrap_or_default();
        let out = match (method, path) {
            ("GET", "/lend/pools") => self.pools().map_err(internal),
            ("GET", "/lend/account") => lend_auth::fields_from_query(url)
                .ok_or((400, "bad request".to_owned()))
                .and_then(|f| self.account(f)),
            ("POST", "/lend/ticket") => lend_auth::fields_from_json(body)
                .ok_or((400, "bad request".to_owned()))
                .and_then(|f| self.ticket(f)),
            ("POST", "/lend/build") => lend_auth::fields_from_json(body)
                .ok_or((400, "bad request".to_owned()))
                .and_then(|f| self.build(f)),
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
        let wallet = lend_auth::authenticate(
            fields,
            method,
            path,
            now,
            &self.cfg.network,
            &self.program,
            &mut nonces,
        )
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
                vec![json!({
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

    fn account(&self, fields: BTreeMap<String, String>) -> Result<Value, Refusal> {
        let (wallet, now) = self.authenticate(&fields, "GET", "/lend/account", &[])?;
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
        Ok(json!({ "receipts": receipts, "positions": positions, "intents": intents }))
    }

    fn ticket(&self, fields: BTreeMap<String, String>) -> Result<Value, Refusal> {
        let (wallet, now) = self.authenticate(
            &fields,
            "POST",
            "/lend/ticket",
            &["purpose", "amount", "pool", "vault", "position"],
        )?;
        let amount = decimal(fields.get("amount")).ok_or((400, "bad amount".to_owned()))?;
        let paused = self.cfg.paused();
        let l = &self.cfg.limits;
        let book = self.snapshot().map_err(internal)?;
        let w = wallet.to_string();
        let my_position = |id: u64| {
            book.positions.iter().find(|p| {
                p.owned_by(&wallet)
                    && p.state::<PositionState>()
                        .is_ok_and(|s| s.position_id == id)
            })
        };
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
                if book.receipts.iter().filter(|n| n.owned_by(&wallet)).count() >= MAX_LISTED {
                    return refuse(429, "this wallet holds the most receipts allowed");
                }
                (purpose::SUPPLY, keeler_lend::pool::TUSD)
            }
            Some("deposit") => {
                let id = position_field()?;
                if my_position(id).is_some() {
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
                    if book
                        .positions
                        .iter()
                        .filter(|n| n.owned_by(&wallet))
                        .count()
                        >= MAX_LISTED
                    {
                        return refuse(429, "this wallet holds the most positions allowed");
                    }
                }
                (purpose::DEPOSIT, id)
            }
            Some("repay") => {
                let id = position_field()?;
                let has_debt = my_position(id)
                    .and_then(|p| p.state::<PositionState>().ok())
                    .is_some_and(|s| s.debt_shares > 0);
                if !has_debt {
                    return refuse(404, "no such position with debt");
                }
                if amount < l.min_repay {
                    return refuse(422, "below the minimum repayment");
                }
                (purpose::REPAY, id)
            }
            _ => return refuse(400, "bad purpose"),
        };
        let recent = book
            .tickets
            .iter()
            .filter(|t| t.wallet == w && t.issued + HOUR > now)
            .count();
        if recent >= l.tickets_per_wallet_hour as usize {
            return refuse(429, "too many requests from this wallet this hour");
        }
        let open = book
            .tickets
            .iter()
            .filter(|t| t.status == TicketStatus::Open && t.expiry > now)
            .count();
        if open >= l.max_open_intents as usize {
            return refuse(429, "the keeper is busy; try again shortly");
        }
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
        };
        let mut b = self.book().map_err(internal)?;
        b.tickets.push(t.clone());
        if let Err(e) = self.save(&b) {
            b.tickets.pop();
            return Err(internal(e));
        }
        Ok(json!({ "id": t.id, "expiry": expiry, "signature": signature }))
    }

    fn build(&self, fields: BTreeMap<String, String>) -> Result<Value, Refusal> {
        let (wallet, _) = self.authenticate(
            &fields,
            "POST",
            "/lend/build",
            &[
                "op", "pool", "vault", "position", "amount", "shares", "borrow", "withdraw",
            ],
        )?;
        let op = fields.get("op").map(String::as_str);
        let num = |k: &str| decimal(fields.get(k)).ok_or((400, format!("bad {k}")));
        let mut shares = None;
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
        if borrow > 0 && self.cfg.paused() {
            return refuse(403, "lending is paused: no new borrowing");
        }
        // Wait for the lane, briefly.
        let started = Instant::now();
        let _lane = loop {
            match self.lane.try_lock() {
                Ok(g) => break g,
                Err(_) if started.elapsed() < LANE_WAIT => sleep(Duration::from_millis(50)),
                Err(_) => return refuse(409, "the pool is in use; try again shortly"),
            }
        };
        let now = self.ledger.now().map_err(internal)?;
        let book = self.snapshot().map_err(internal)?;
        let w = wallet.to_string();
        if book.in_flight()
            || book.reserved_for_other(&w, now)
            || self.priority_waiting.load(Ordering::Relaxed)
        {
            return refuse(409, "the pool is in use; try again shortly");
        }
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
        let (job, index, bound) = if op == Some("redeem") {
            // The smallest receipt that covers the amount.
            let mut held: Vec<(u64, &Note)> = book
                .receipts
                .iter()
                .filter(|n| n.owned_by(&wallet))
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
            let receipt = match shares {
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
                    receipt
                }
                None => {
                    held.iter()
                        .find(|(a, _)| *a >= amount)
                        .ok_or((422, "more than one receipt holds".to_owned()))?
                        .1
                }
            };
            let s: ReceiptState = receipt.state().map_err(internal)?;
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
            let id = decimal(fields.get("position")).ok_or((400, "bad position".to_owned()))?;
            let position = book
                .positions
                .iter()
                .find(|p| {
                    p.owned_by(&wallet)
                        && p.state::<PositionState>()
                            .is_ok_and(|s| s.position_id == id)
                })
                .ok_or((404, "no such position".to_owned()))?;
            if borrow == 0 && withdraw == 0 {
                return refuse(422, "nothing to draw");
            }
            let q = self.ledger.quote(vault1().id).map_err(internal)?;
            if (now as i64).abs_diff(q.published) > PRICE_FRESH {
                return refuse(412, "the oracle price is too old");
            }
            let m = u128::from(self.cfg.limits.bound_margin_bps);
            let bound = u64::try_from(u128::from(q.low) * (10_000 - m) / 10_000)
                .map_err(|e| internal(e.into()))?;
            let s: PositionState = position.state().map_err(internal)?;
            let mut shares = s.debt_shares;
            if borrow > 0 {
                let (_, added) = core::borrow(&r, reserve.amount, borrow, &core::TUSD_POOL.curve)
                    .map_err(cerr)?;
                shares += added;
            }
            let debt = core::debt_of(shares, r.borrow_price).map_err(cerr)?;
            let left = position
                .amount
                .checked_sub(withdraw)
                .ok_or((422, "more than the position holds".to_owned()))?;
            let v = vault1();
            if left != 0 && left < v.min_collateral {
                return refuse(422, "below the vault's minimum collateral");
            }
            if debt != 0 && debt < v.min_debt {
                return refuse(422, "below the vault's minimum debt");
            }
            let value = core::collateral_value(left, bound).map_err(cerr)?;
            if !core::can_draw(debt, value, &core::TSOL_TUSD_VAULT) {
                return refuse(423, "the position would be unhealthy");
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
        let mut pending = built.pending;
        pending.owner_signed = true;
        let mut b = self.book().map_err(internal)?;
        b.pending.push(pending.clone());
        let before = b.reservation.replace(Reservation {
            wallet: w,
            until: now + self.cfg.limits.reservation_secs,
        });
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
        });
        if let Some(bound) = bound {
            reply["bound"] = json!(bound.to_string());
        }
        Ok(reply)
    }
}

/// Applies a landed build: its inputs leave the book, its notes join it.
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
        match n.kind {
            KIND_RESERVE => book.reserve = Some(n),
            // A receipt redeemed in full holds nothing.
            KIND_RECEIPT if n.state::<ReceiptState>().is_ok_and(|s| s.supply_shares > 0) => {
                book.receipts.push(n)
            }
            // A position left with nothing in it is closed by its owner, if at all.
            KIND_POSITION
                if n.amount > 0 || n.state::<PositionState>().is_ok_and(|s| s.debt_shares > 0) =>
            {
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
        cfg.lend.check()?;
        Ok(cfg)
    }
}

/// What a running keeper borrows: its key, its chain, its state file.
pub struct Parts {
    pub signer: Keypair,
    pub chain: crate::lend_chain::LendChain,
    pub state: PathBuf,
}

/// Loads the keeper key (`lend.key`, else `default`), checks it is the
/// program's KEEPER and connects.
pub fn connect(
    lend: &Lend,
    network: &crate::config::Network,
    default: &KeySource,
) -> Result<Parts> {
    let signer = lend.key.as_ref().unwrap_or(default).keypair()?;
    ensure!(
        signer.pubkey() == keeler_lend::KEEPER,
        "the keeper key is {}, the program's keeper is {}",
        signer.pubkey(),
        keeler_lend::KEEPER
    );
    let chain = crate::lend_chain::LendChain::connect(network, &signer, lend.price)?;
    let state = lend.state_path(&signer.pubkey())?;
    Ok(Parts {
        signer,
        chain,
        state,
    })
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
    fn lend(&self, method: &str, url: &str, body: &[u8]) -> (u16, Value);
}

impl<L: Ledger> LendRoutes for Keeper<'_, L> {
    fn lend(&self, method: &str, url: &str, body: &[u8]) -> (u16, Value) {
        self.http(method, url, body)
    }
}

/// Fails unless the wallet is on a network where the test price exists.
pub fn require_test_network(lend: &Lend) -> Result<()> {
    if lend.network == "mainnet" {
        bail!("set_test_price is for local networks and devnet only");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
