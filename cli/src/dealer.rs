//! The dealer: quote from cached note sizes, settle only orders that match a
//! remembered quote exactly, and only while a price read just before the
//! take still covers it. Quotes are signed and kept, with the sell and
//! faucet counters, in a state file across restarts. Quotes go out over
//! line-delimited TCP and, when configured, over HTTP with the same handler.
use std::{
    collections::{HashMap, HashSet},
    io::{self, BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Mutex, OnceLock,
    },
    thread::{self, sleep, Scope},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, ensure, Result};
use keeler_core::{
    price::{apply_spread, fair_buy_amount},
    rfq::{serve, Quote, Rfq},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use solana_address::Address;
use solana_keypair::Keypair;
use solana_signer::Signer;
use spr_client::Rpc;
use spr_keypair::ShieldedKeypair;
use swap_sdk::{index::TakerOrder, state::OrderUtxo};

use crate::{
    chain::{hex, shielded, Chain, Cluster, Status},
    config::{parse_address, DealerConfig, Pricer, Prover, SellLimit},
    faucet::{Claims, Faucet, Limits},
    keeper::LendRoutes,
    trader::{fair_amount, sign_quote, signed_by},
};

const MAX_BOOK: usize = 1024;
/// Largest HTTP request body accepted.
const MAX_BODY: usize = 4096;
/// Largest HTTP request line plus headers accepted.
const MAX_HEAD: usize = 8192;
const HTTP_POLL: Duration = Duration::from_millis(100);
/// Time one HTTP request may take from accept to reply.
const HTTP_DEADLINE: Duration = Duration::from_secs(3);
/// Largest `/lend/*` body: signed requests carry a session authorisation.
const MAX_LEND_BODY: usize = 16_384;
/// A `/lend/build` proves a transaction; the Worker waits 30 s.
const LEND_DEADLINE: Duration = Duration::from_secs(30);
/// `/lend/*` requests served at once, each on its own thread.
const MAX_LEND_THREADS: usize = 8;
const HOUR: u64 = 3_600;
const DEFAULT_AUTO_PAUSE: u32 = 3;
const DEFAULT_MIN_FEE: u64 = 10_000_000;

#[derive(Default)]
struct Cache {
    /// Quotes given and not yet bound to an order.
    book: Vec<(Rfq, Quote)>,
    /// The quote each bound order matched, by order hash, until the order is
    /// done with.
    bound: HashMap<[u8; 32], (Rfq, Quote)>,
    /// (chain time, sell mint, sell amount) of orders bound in the last hour.
    bound_sells: Vec<(u64, Address, u64)>,
    /// The largest single note per mint: `take` spends one note of exactly
    /// the quoted amount, split from one note that covers it.
    largest_note: HashMap<Address, u64>,
    /// The dealer's public SOL at the last refresh; it pays every fee.
    fee_lamports: u64,
    failed_settles: u32,
    faucet: Option<Limits>,
}

/// What the state file holds.
#[derive(Default, Serialize, Deserialize)]
struct State {
    book: Vec<(Rfq, Quote)>,
    bound: Vec<(Rfq, Quote)>,
    #[serde(default)]
    bound_sells: Vec<(u64, String, u64)>,
    #[serde(default)]
    faucet: Option<Claims>,
}

/// Writes the quotes and counters to `path` atomically, readable by this
/// user only.
fn save_state(path: &Path, cache: &Cache) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let state = State {
        book: cache.book.clone(),
        bound: cache.bound.values().cloned().collect(),
        bound_sells: cache
            .bound_sells
            .iter()
            .map(|(t, m, a)| (*t, m.to_string(), *a))
            .collect(),
        faucet: cache.faucet.as_ref().map(|l| l.claims.clone()),
    };
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(&serde_json::to_vec(&state)?)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

struct Loaded {
    /// Unexpired quotes this dealer signed, bound ones included: the poll
    /// matches a bound order to its quote again, so it still settles after a
    /// restart.
    quotes: Vec<(Rfq, Quote)>,
    bound_sells: Vec<(u64, Address, u64)>,
    faucet: Option<Claims>,
}

fn load_state(path: &Path, now: u64, dealer: &Address, network: Cluster) -> Result<Loaded> {
    let text = match std::fs::read(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(Loaded {
                quotes: Vec::new(),
                bound_sells: Vec::new(),
                faucet: None,
            })
        }
        Err(e) => return Err(e.into()),
    };
    let state: State = serde_json::from_slice(&text)
        .map_err(|_| anyhow!("state file {} is not valid", path.display()))?;
    // A live bound quote binds its order again after the restart, which
    // counts its sell again, so its old count goes.
    let rebinds: Vec<(String, u64)> = state
        .bound
        .iter()
        .filter(|(r, q)| q.expiry > now && signed_by(dealer, network, r, q))
        .map(|(r, _)| (r.sell_mint.clone(), r.sell_amount))
        .collect();
    let live: Vec<_> = state
        .book
        .into_iter()
        .chain(state.bound)
        .filter(|(_, q)| q.expiry > now)
        .collect();
    let count = live.len();
    // A quote it did not sign never binds an order.
    let quotes: Vec<_> = live
        .into_iter()
        .filter(|(r, q)| signed_by(dealer, network, r, q))
        .collect();
    if quotes.len() < count {
        log!(
            "dealer: dropped {} quotes from the state file: not signed by this dealer",
            count - quotes.len()
        );
    }
    let mut bound_sells: Vec<(u64, Address, u64)> = state
        .bound_sells
        .into_iter()
        .filter(|(t, _, _)| t.saturating_add(HOUR) > now)
        .map(|(t, m, a)| Ok((t, parse_address(&m)?, a)))
        .collect::<Result<_>>()?;
    for (mint, amount) in rebinds {
        if let Some(i) = bound_sells
            .iter()
            .position(|(_, m, a)| m.to_string() == mint && *a == amount)
        {
            bound_sells.remove(i);
        }
    }
    Ok(Loaded {
        quotes,
        bound_sells,
        faucet: state.faucet,
    })
}

fn state_path(cfg: &DealerConfig, dealer: &Address) -> Result<PathBuf> {
    if let Some(p) = &cfg.state_file {
        return Ok(p.clone());
    }
    let dir = PathBuf::from(std::env::var("HOME")?).join(".local/state/keeler");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("dealer-{dealer}.json")))
}

/// The file whose presence means quoting paused itself after failed settles.
fn auto_pause_path(state: &Path) -> PathBuf {
    state.with_extension("autopaused")
}

fn check_pause(file: Option<&Path>) -> Result<()> {
    ensure!(!file.is_some_and(|f| f.exists()), "paused");
    Ok(())
}

/// Fails if selling `amount` of `mint` breaks a configured limit, given the
/// sells bound so far.
fn within_limits(
    limits: &[SellLimit],
    sells: &[(u64, Address, u64)],
    mint: Address,
    amount: u64,
    now: u64,
) -> Result<()> {
    for l in limits
        .iter()
        .filter(|l| parse_address(&l.mint).is_ok_and(|m| m == mint))
    {
        if let Some(max) = l.max_sell_amount {
            ensure!(amount <= max, "{amount} of {mint} is above max_sell_amount");
        }
        if let Some(cap) = l.hourly_sell_cap {
            let used: u128 = sells
                .iter()
                .filter(|(t, m, _)| *m == mint && t.saturating_add(HOUR) > now)
                .map(|(_, _, a)| u128::from(*a))
                .sum();
            ensure!(
                used + u128::from(amount) <= u128::from(cap),
                "hourly sell cap of {mint} reached"
            );
        }
    }
    Ok(())
}

/// Last look: settle only while `buy_amount` is within `tolerance_bps` above
/// the fair amount at the current price. No current price, no settlement.
fn last_look(buy_amount: u64, fair_now: Option<u64>, tolerance_bps: u16) -> bool {
    fair_now.is_some_and(|fair| {
        u128::from(buy_amount) * 10_000 <= u128::from(fair) * (10_000 + u128::from(tolerance_bps))
    })
}

/// The last look at prices read now, with the mints' `decimals`.
fn fresh_last_look(
    pricer: &Pricer,
    rpc: &impl Rpc,
    order: &OrderUtxo,
    decimals: (u8, u8),
    tolerance_bps: u16,
) -> bool {
    let (sell, buy) = (order.source_mint.asset, order.terms.destination_mint.asset);
    let fair = match pricer.prices(rpc, &[sell, buy]) {
        Ok(p) => fair_buy_amount(order.source_amount, p[0], p[1], decimals.0, decimals.1),
        Err(e) => {
            log!("dealer: prices: {e:#}");
            None
        }
    };
    let ok = last_look(order.terms.destination_amount, fair, tolerance_bps);
    if !ok {
        log!(
            "dealer: declined at last look: quoted {}, fair now {fair:?}",
            order.terms.destination_amount
        );
    }
    ok
}

/// What a dealer on `cluster` refuses to start with: a lend section for
/// another network, and on mainnet a faucet or a key read from a file.
fn start_checks(cfg: &DealerConfig, cluster: Cluster) -> Result<()> {
    if let Some(l) = &cfg.lend {
        crate::keeper::check_cluster(l, cluster)?;
    }
    if cluster == Cluster::Mainnet {
        ensure!(cfg.faucet.is_none(), "no faucet on mainnet");
        let files = cfg.common.key.is_file() || cfg.http.as_ref().is_some_and(|h| h.key.is_file());
        ensure!(!files, "on mainnet keys come from the Keychain, not a file");
    }
    Ok(())
}

/// Serves quotes and settles until `stop` is set, or fails when a server
/// stops on its own.
pub fn run(cfg: &DealerConfig, stop: &AtomicBool) -> Result<()> {
    ensure!(
        cfg.program_id == swap_program::ID.to_string(),
        "this binary is built for swap program {}, the config is for {}; rebuild with \
         `KEELER_SWAP_PROGRAM_ID={} scripts/setup.sh` and `cargo build --release -p keeler`",
        swap_program::ID,
        cfg.program_id,
        cfg.program_id
    );
    let c = &cfg.common;
    let signer = c.keypair()?;
    if let Some(p) = &cfg.pubkey {
        ensure!(
            signer.pubkey().to_string() == *p,
            "the dealer key is {}, the config says {p}",
            signer.pubkey()
        );
    }
    let pricer = c.pricer()?;
    let chain = Chain::connect(&c.network, &c.mints()?)?;
    start_checks(cfg, chain.cluster)?;
    let kp = shielded(&signer)?;
    chain.register(&kp)?;
    let state = state_path(cfg, &signer.pubkey())?;
    let loaded = load_state(&state, chain.now()?, &signer.pubkey(), chain.cluster)?;
    let cache = Mutex::new(Cache {
        book: loaded.quotes,
        bound_sells: loaded.bound_sells,
        faucet: cfg.faucet.as_ref().map(|f| {
            let mut l = Limits::new(f.per_wallet_daily, f.daily_cap);
            l.claims = loaded.faucet.unwrap_or_default();
            l
        }),
        ..Cache::default()
    });
    refresh(cfg, &chain, &kp, &signer.pubkey(), &cache)?;
    let faucet = cfg.faucet.as_ref().map(Faucet::new).transpose()?;
    let (payouts, queue) = mpsc::channel();
    let api = DealerApi {
        payouts: Mutex::new(payouts),
        cfg,
        pricer: &pricer,
        chain: &chain,
        cache: &cache,
        faucet: faucet.as_ref(),
        signer: &signer,
        state: &state,
    };
    let poll_every = Duration::from_secs(cfg.poll_secs.unwrap_or(match c.network.prover {
        Prover::Local => 1,
        Prover::Helius { .. } => 5,
    }));
    let listener = TcpListener::bind(cfg.listen)?;
    let http = match &cfg.http {
        Some(h) => {
            let key = h.key.secret()?;
            ensure!(
                key.len() >= 16,
                "the HTTP key must be at least 16 characters"
            );
            Some((TcpListener::bind(h.listen)?, key))
        }
        None => None,
    };
    // The keeper starts in the background once the swap servers are up, and
    // a keeper that cannot start leaves them serving.
    let parts = OnceLock::new();
    let keeper = OnceLock::new();
    let slot = crate::keeper::Slot(&keeper);
    let lend = cfg.lend.as_ref().map(|_| &slot as &dyn LendRoutes);
    let api = &api;
    thread::scope(|s| {
        let server = s.spawn(|| serve(listener, stop, |rfq| api.quote(rfq)));
        s.spawn(|| api.pay_claims(queue, stop));
        if let Some(l) = &cfg.lend {
            let (parts, keeper) = (&parts, &keeper);
            s.spawn(move || {
                crate::keeper::run_in_dealer(l, &c.network, &c.key, parts, keeper, stop)
            });
        }
        let web = http
            .map(|(l, key)| s.spawn(move || serve_http(l, &key, stop, api, lend, HTTP_DEADLINE)));
        let mut seen = HashSet::new();
        let mut bound = HashMap::new();
        while !stop.load(Ordering::Relaxed)
            && !server.is_finished()
            && !web.as_ref().is_some_and(|w| w.is_finished())
        {
            poll(
                cfg,
                &pricer,
                &chain,
                &kp,
                &signer.pubkey(),
                &cache,
                &state,
                &mut seen,
                &mut bound,
            );
            sleep(poll_every);
        }
        let stopped = stop.load(Ordering::Relaxed);
        if let Some(web) = web {
            match web.join() {
                Err(_) => bail!("HTTP server panicked"),
                Ok(Err(e)) => bail!("HTTP server failed: {e:#}"),
                Ok(Ok(())) if !stopped => bail!("HTTP server stopped"),
                Ok(Ok(())) => {}
            }
        }
        match server.join() {
            Err(_) => bail!("quote server panicked"),
            Ok(Err(e)) => Err(anyhow!("quote server failed: {e}")),
            Ok(Ok(())) if !stopped => bail!("quote server stopped"),
            Ok(Ok(())) => Ok(()),
        }
    })
}

/// What the HTTP API asks of the dealer.
pub trait Api {
    fn quote(&self, rfq: Rfq) -> Result<Quote, String>;
    /// `Err((status, message))`.
    fn faucet(&self, wallet: Address) -> Result<(), (u16, String)>;
    fn now(&self) -> Result<u64, String>;
}

struct DealerApi<'a> {
    cfg: &'a DealerConfig,
    pricer: &'a Pricer,
    chain: &'a Chain,
    cache: &'a Mutex<Cache>,
    faucet: Option<&'a Faucet>,
    signer: &'a Keypair,
    state: &'a Path,
    payouts: Mutex<mpsc::Sender<Address>>,
}

type HttpError = (u16, String);

impl DealerApi<'_> {
    fn give_back(&self, wallet: Address) {
        let _ = self.count(|l| {
            l.undo(wallet);
            Ok(())
        });
    }

    /// Pays queued faucet claims until `stop`.
    fn pay_claims(&self, queue: mpsc::Receiver<Address>, stop: &AtomicBool) {
        let Some(faucet) = self.faucet else { return };
        while !stop.load(Ordering::Relaxed) {
            let Ok(wallet) = queue.recv_timeout(Duration::from_millis(200)) else {
                continue;
            };
            match faucet.pay(self.chain, wallet) {
                Ok(()) => log!("dealer: faucet paid {wallet}"),
                Err((sent, e)) => {
                    log!("dealer: faucet for {wallet} (sent: {sent}): {e:#}");
                    // A send that may land keeps the claim: no second payout.
                    if !sent {
                        self.give_back(wallet);
                    }
                }
            }
        }
    }

    /// Applies `f` to the faucet counts and saves them.
    fn count(&self, f: impl FnOnce(&mut Limits) -> Result<(), HttpError>) -> Result<(), HttpError> {
        let mut c = self
            .cache
            .lock()
            .map_err(|_| (500, "cache poisoned".to_string()))?;
        f(c.faucet
            .as_mut()
            .ok_or((404, "the faucet is off".to_string()))?)?;
        save_state(self.state, &c).map_err(|e| {
            log!("dealer: save state: {e:#}");
            (500, "faucet unavailable".to_string())
        })
    }
}

impl Api for DealerApi<'_> {
    fn quote(&self, rfq: Rfq) -> Result<Quote, String> {
        quote(self, rfq).map_err(|e| {
            log!("dealer: no quote: {e:#}");
            "no quote".to_string()
        })
    }

    fn faucet(&self, wallet: Address) -> Result<(), (u16, String)> {
        let faucet = self.faucet.ok_or((404, "the faucet is off".to_string()))?;
        match faucet.check_funds(self.chain) {
            Ok(true) => {}
            Ok(false) => return Err((409, "the faucet is out of SOL".to_string())),
            Err(e) => {
                log!("dealer: faucet balance: {e:#}");
                return Err((502, "faucet transfer failed".to_string()));
            }
        }
        let day = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() / 86_400);
        self.count(|l| l.take(wallet, day).map_err(|m| (429, m.to_string())))?;
        // Paid off the request thread: a confirmation takes longer than the
        // Worker waits, and the HTTP server answers one request at a time.
        let queued = self.payouts.lock().ok().and_then(|tx| tx.send(wallet).ok());
        if queued.is_none() {
            self.give_back(wallet);
            return Err((502, "faucet transfer failed".to_string()));
        }
        Ok(())
    }

    fn now(&self) -> Result<u64, String> {
        self.chain.now().map_err(|e| {
            log!("dealer: chain time: {e:#}");
            "no chain time".to_string()
        })
    }
}

/// A read that fails once `until` passes, however slowly bytes arrive.
struct Deadline {
    stream: TcpStream,
    until: Instant,
}

impl Read for Deadline {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.stream.set_read_timeout(Some(left))?;
        (&self.stream).read(buf)
    }
}

#[derive(Default)]
struct Head {
    method: String,
    path: String,
    key: Option<String>,
    length: Option<usize>,
    chunked: bool,
    /// `x-keeler-paused: 1`: the Worker paused lending for every keeper.
    paused: bool,
}

/// The request line and headers, or `None` when they are malformed or
/// longer than `MAX_HEAD`.
fn read_head(r: &mut impl BufRead) -> io::Result<Option<Head>> {
    let mut head = Head::default();
    let mut used = 0;
    loop {
        let mut line = String::new();
        let n = r.take((MAX_HEAD - used) as u64).read_line(&mut line)?;
        used += n;
        if n == 0 || !line.ends_with('\n') {
            return Ok(None);
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if head.method.is_empty() {
            let mut parts = line.split(' ');
            let (Some(m), Some(p), Some(v), None) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                return Ok(None);
            };
            if m.is_empty() || !v.starts_with("HTTP/1.") {
                return Ok(None);
            }
            (head.method, head.path) = (m.to_owned(), p.to_owned());
            continue;
        }
        if line.is_empty() {
            return Ok(Some(head));
        }
        let Some((name, value)) = line.split_once(':') else {
            return Ok(None);
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => {
                if head.length.is_some()
                    || value.is_empty()
                    || !value.bytes().all(|b| b.is_ascii_digit())
                {
                    return Ok(None);
                }
                // Too large to parse is too large to accept.
                head.length = Some(value.parse().unwrap_or(usize::MAX));
            }
            "x-keeler-key" => head.key = Some(value.to_owned()),
            "x-keeler-paused" => head.paused = value == "1",
            "transfer-encoding" => head.chunked = true,
            _ => {}
        }
    }
}

/// Serves the HTTP API until `stop` is set: swap routes one connection at a
/// time, `/lend/*` (when `lend` is set) on threads of their own. Each
/// connection gets `deadline` from accept to its head and swap reply (lend:
/// `LEND_DEADLINE`), no body is read before the key and the declared length
/// pass, and every reply closes.
pub fn serve_http(
    listener: TcpListener,
    key: &str,
    stop: &AtomicBool,
    api: &impl Api,
    lend: Option<&dyn LendRoutes>,
    deadline: Duration,
) -> Result<()> {
    listener.set_nonblocking(true)?;
    let busy = AtomicUsize::new(0);
    thread::scope(|s| {
        while !stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, remote)) => {
                    one_request(s, stream, remote, key, api, lend, &busy, deadline)
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => sleep(HTTP_POLL),
                Err(e) if e.kind() == io::ErrorKind::ConnectionAborted => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    })
}

#[allow(clippy::too_many_arguments)]
fn one_request<'s>(
    scope: &'s Scope<'s, '_>,
    stream: TcpStream,
    remote: SocketAddr,
    key: &'s str,
    api: &impl Api,
    lend: Option<&'s dyn LendRoutes>,
    busy: &'s AtomicUsize,
    deadline: Duration,
) {
    if stream.set_nonblocking(false).is_err() || stream.set_write_timeout(Some(deadline)).is_err() {
        return;
    }
    let Ok(clone) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(Deadline {
        stream: clone,
        until: Instant::now() + deadline,
    });
    let (status, body) = match read_head(&mut reader) {
        Ok(Some(h)) if h.chunked => error(400, "bad request"),
        Ok(Some(h)) if h.path.starts_with("/lend/") && lend.is_some() => {
            let Some(lend) = lend else { return };
            if busy.fetch_add(1, Ordering::SeqCst) >= MAX_LEND_THREADS {
                busy.fetch_sub(1, Ordering::SeqCst);
                return reply(&stream, error(503, "busy; try again shortly"));
            }
            reader.get_mut().until = Instant::now() + LEND_DEADLINE;
            let _ = stream.set_write_timeout(Some(LEND_DEADLINE));
            scope.spawn(move || {
                let mut body = (&mut reader).take(h.length.unwrap_or(0) as u64);
                let out = match read_body(
                    key,
                    Some(remote),
                    h.key.as_deref(),
                    h.length,
                    &mut body,
                    MAX_LEND_BODY,
                ) {
                    Ok(buf) => lend.lend(&h.method, &h.path, &buf, h.paused),
                    Err(e) => e,
                };
                reply(&stream, out);
                busy.fetch_sub(1, Ordering::SeqCst);
            });
            return;
        }
        Ok(Some(h)) => {
            let mut body = (&mut reader).take(h.length.unwrap_or(0) as u64);
            handle(
                api,
                key,
                Some(remote),
                &h.method,
                &h.path,
                h.key.as_deref(),
                h.length,
                &mut body,
            )
        }
        Ok(None) => error(400, "bad request"),
        Err(_) => error(408, "request timeout"),
    };
    reply(&stream, (status, body));
}

fn reply(stream: &TcpStream, (status, body): (u16, Value)) {
    let text = body.to_string();
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        409 => "Conflict",
        410 => "Gone",
        412 => "Precondition Failed",
        413 => "Payload Too Large",
        422 => "Unprocessable Content",
        423 => "Locked",
        429 => "Too Many Requests",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let reply = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
        text.len()
    );
    // A client that hung up only loses its own reply.
    let _ = (&mut &*stream).write_all(reply.as_bytes());
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpRfq {
    sell_mint: String,
    buy_mint: String,
    sell_amount: String,
    trader: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FaucetRequest {
    wallet: String,
}

/// A base-10 `u64` with digits only: no sign, no spaces.
fn parse_amount(s: &str) -> Option<u64> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

fn same_key(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len()
        && std::hint::black_box(a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y))) == 0
}

fn error(status: u16, msg: &str) -> (u16, Value) {
    (status, json!({ "error": msg }))
}

/// The body, once the remote is loopback, the key matches and the declared
/// length is at most `max`.
fn read_body(
    key: &str,
    remote: Option<SocketAddr>,
    given_key: Option<&str>,
    length: Option<usize>,
    body: &mut dyn Read,
    max: usize,
) -> Result<Vec<u8>, (u16, Value)> {
    if !remote.is_some_and(|r| r.ip().is_loopback()) {
        return Err(error(403, "loopback only"));
    }
    if !given_key.is_some_and(|g| same_key(g.as_bytes(), key.as_bytes())) {
        return Err(error(401, "bad key"));
    }
    if length.is_some_and(|l| l > max) {
        return Err(error(413, "body too large"));
    }
    let mut buf = Vec::new();
    if body.take(max as u64 + 1).read_to_end(&mut buf).is_err() {
        return Err(error(400, "bad request"));
    }
    if buf.len() > max {
        return Err(error(413, "body too large"));
    }
    Ok(buf)
}

/// One HTTP request. The key and the declared length are checked before any
/// body byte is read.
#[allow(clippy::too_many_arguments)]
fn handle(
    api: &impl Api,
    key: &str,
    remote: Option<SocketAddr>,
    method: &str,
    url: &str,
    given_key: Option<&str>,
    length: Option<usize>,
    body: &mut dyn Read,
) -> (u16, Value) {
    let buf = match read_body(key, remote, given_key, length, body, MAX_BODY) {
        Ok(b) => b,
        Err(e) => return e,
    };
    let path = url.split('?').next().unwrap_or_default();
    match (method, path) {
        ("GET", "/health") => match api.now() {
            Ok(now) => (200, json!({ "ok": true, "now": now })),
            Err(e) => error(503, &e),
        },
        ("POST", "/quote") => {
            let Some(rfq) = serde_json::from_slice::<HttpRfq>(&buf).ok().and_then(|r| {
                Some(Rfq {
                    sell_amount: parse_amount(&r.sell_amount)?,
                    sell_mint: r.sell_mint,
                    buy_mint: r.buy_mint,
                    trader: r.trader,
                })
            }) else {
                return error(400, "bad request");
            };
            match api.quote(rfq) {
                Ok(q) => {
                    let mut wire = json!(q);
                    wire["buy_amount"] = json!(q.buy_amount.to_string());
                    (200, json!({ "quote": wire }))
                }
                Err(_) => error(200, "no quote"),
            }
        }
        ("POST", "/faucet") => {
            let Some(wallet) = serde_json::from_slice::<FaucetRequest>(&buf)
                .ok()
                .and_then(|r| parse_address(&r.wallet).ok())
            else {
                return error(400, "bad request");
            };
            match api.faucet(wallet) {
                Ok(()) => (200, json!({ "ok": true })),
                Err((status, msg)) => error(status, &msg),
            }
        }
        _ => error(404, "not found"),
    }
}

fn quote(api: &DealerApi, rfq: Rfq) -> Result<Quote> {
    let sell = parse_address(&rfq.sell_mint)?;
    let buy = parse_address(&rfq.buy_mint)?;
    parse_address(&rfq.trader)?;
    let fair = fair_amount(api.pricer, api.chain, sell, buy, rfq.sell_amount)?;
    let buy_amount = apply_spread(fair, api.cfg.spread_bps).ok_or_else(|| anyhow!("bad spread"))?;
    let now = api.chain.now()?;
    let mut cache = api.cache.lock().map_err(|_| anyhow!("cache poisoned"))?;
    admit(
        api.cfg,
        &mut cache,
        api.state,
        (api.signer, api.chain.cluster),
        rfq,
        buy_amount,
        now,
    )
}

/// Every check a priced quote must pass, then signs it into the book and
/// the state file.
fn admit(
    cfg: &DealerConfig,
    cache: &mut Cache,
    state: &Path,
    (signer, network): (&Keypair, Cluster),
    rfq: Rfq,
    buy_amount: u64,
    now: u64,
) -> Result<Quote> {
    check_pause(cfg.pause_file.as_deref())?;
    ensure!(
        !auto_pause_path(state).exists(),
        "paused after failed settles"
    );
    let floor = cfg.min_fee_lamports.unwrap_or(DEFAULT_MIN_FEE);
    ensure!(
        cache.fee_lamports >= floor,
        "fee SOL {} is below the floor {floor}",
        cache.fee_lamports
    );
    ensure!(buy_amount > 0, "amount too small");
    let (sell, buy) = (
        parse_address(&rfq.sell_mint)?,
        parse_address(&rfq.buy_mint)?,
    );
    let largest = cache.largest_note.get(&buy).copied().unwrap_or(0);
    ensure!(largest >= buy_amount, "no single note covers {buy_amount}");
    within_limits(&cfg.limits, &cache.bound_sells, sell, rfq.sell_amount, now)?;
    cache.book.retain(|(_, q)| q.expiry > now);
    ensure!(cache.book.len() < MAX_BOOK, "quote book full");
    let q = sign_quote(
        signer,
        network,
        &rfq,
        buy_amount,
        now.saturating_add(cfg.quote_ttl_secs),
    )?;
    cache.book.push((rfq, q));
    // A quote the state file does not hold would be forgotten on restart.
    if let Err(e) = save_state(state, cache) {
        cache.book.pop();
        return Err(e.context("save the dealer state"));
    }
    Ok(q)
}

fn refresh(
    cfg: &DealerConfig,
    chain: &Chain,
    kp: &ShieldedKeypair,
    dealer: &Address,
    cache: &Mutex<Cache>,
) -> Result<()> {
    let mut largest = HashMap::new();
    for mint in cfg.common.mints()? {
        let notes = chain.notes(kp, mint)?;
        largest.insert(mint, notes.iter().map(|u| u.utxo.amount).max().unwrap_or(0));
    }
    let fee = chain.lamports(*dealer)?;
    let mut c = cache.lock().map_err(|_| anyhow!("cache poisoned"))?;
    c.largest_note = largest;
    c.fee_lamports = fee;
    Ok(())
}

fn matches(rfq: &Rfq, q: &Quote, o: &TakerOrder) -> bool {
    let t = &o.order_utxo;
    rfq.trader == o.maker_pubkey.to_string()
        && rfq.sell_mint == t.source_mint.asset.to_string()
        && rfq.sell_amount == t.source_amount
        && rfq.buy_mint == t.terms.destination_mint.asset.to_string()
        && q.buy_amount == t.terms.destination_amount
        && q.expiry == t.terms.expiry
}

/// What became of one settle attempt.
#[derive(Debug, PartialEq, Eq)]
enum Settle {
    /// The dealer's take landed.
    Took,
    /// Done with: settled or spent elsewhere, or past the settle margin.
    Gone,
    /// Declined at the last look; tried again next poll.
    Wait,
}

/// Counts a settle attempt toward the auto-pause: `limit` failures in a row
/// pause quoting, a successful take resumes it.
fn after_settle(cache: &mut Cache, auto_pause: &Path, limit: u32, outcome: &Result<Settle>) {
    match outcome {
        Ok(Settle::Took) => {
            cache.failed_settles = 0;
            if auto_pause.exists() && std::fs::remove_file(auto_pause).is_ok() {
                log!("dealer: a settle succeeded; quoting resumed");
            }
        }
        Ok(_) => {}
        Err(_) => {
            cache.failed_settles += 1;
            if cache.failed_settles >= limit {
                cache.failed_settles = 0;
                match std::fs::write(auto_pause, "") {
                    Ok(()) => log!(
                        "dealer: {limit} settles failed in a row; quoting is PAUSED until a settle \
                         succeeds or you delete {}",
                        auto_pause.display()
                    ),
                    Err(e) => log!("dealer: could not write {}: {e}", auto_pause.display()),
                }
            }
        }
    }
}

/// With no bound order left to settle, failed settles no longer count and
/// an auto-pause ends.
fn clear_when_idle(cache: &mut Cache, auto_pause: &Path) {
    cache.failed_settles = 0;
    if auto_pause.exists() && std::fs::remove_file(auto_pause).is_ok() {
        log!("dealer: no bound order left to settle; quoting resumed");
    }
}

/// One pass. Errors are logged per step and per order, so one bad order or a
/// failed discovery never stops settling orders already bound.
#[allow(clippy::too_many_arguments)]
fn poll(
    cfg: &DealerConfig,
    pricer: &Pricer,
    chain: &Chain,
    kp: &ShieldedKeypair,
    dealer: &Address,
    cache: &Mutex<Cache>,
    state: &Path,
    seen: &mut HashSet<[u8; 32]>,
    bound: &mut HashMap<[u8; 32], OrderUtxo>,
) {
    if let Err(e) = refresh(cfg, chain, kp, dealer, cache) {
        log!("dealer: refresh: {e:#}");
    }
    let skip: HashSet<[u8; 32]> = seen.iter().chain(bound.keys()).copied().collect();
    match chain.taker_orders(kp, &skip) {
        Ok(orders) => {
            for o in orders {
                if let Err(e) = consider(cfg, chain, cache, state, seen, bound, o) {
                    log!("dealer: new order: {e:#}");
                }
            }
        }
        Err(e) => log!("dealer: discover orders: {e:#}"),
    }
    let limit = cfg.auto_pause_after.unwrap_or(DEFAULT_AUTO_PAUSE);
    for (hash, order) in bound.clone() {
        let outcome = settle(cfg, pricer, chain, kp, &order);
        if let Err(e) = &outcome {
            log!("dealer: settle {} failed, will retry: {e:#}", hex(&hash));
        }
        let Ok(mut c) = cache.lock() else { continue };
        after_settle(&mut c, &auto_pause_path(state), limit, &outcome);
        if matches!(outcome, Ok(Settle::Took | Settle::Gone)) {
            bound.remove(&hash);
            seen.insert(hash);
            c.bound.remove(&hash);
            if let Err(e) = save_state(state, &c) {
                log!("dealer: save state: {e:#}");
            }
        }
    }
    if bound.is_empty() {
        if let Ok(mut c) = cache.lock() {
            clear_when_idle(&mut c, &auto_pause_path(state));
        }
    }
}

const OVER_LIMIT: &str = "is over a sell limit";

/// Moves the quote an open order matches from the book to `bound`, or says
/// why the order is not settled.
fn bind(
    cfg: &DealerConfig,
    cache: &mut Cache,
    hash: [u8; 32],
    o: &TakerOrder,
    now: u64,
) -> Result<(), &'static str> {
    cache
        .bound_sells
        .retain(|(t, _, _)| t.saturating_add(HOUR) > now);
    let (sell, amount) = (o.order_utxo.source_mint.asset, o.order_utxo.source_amount);
    let i = cache
        .book
        .iter()
        .position(|(r, q)| matches(r, q, o))
        .ok_or("matches no quote")?;
    if within_limits(&cfg.limits, &cache.bound_sells, sell, amount, now).is_err() {
        return Err(OVER_LIMIT);
    }
    if now.saturating_add(cfg.settle_margin_secs) >= o.order_utxo.terms.expiry {
        return Err("is past the settle margin");
    }
    let quote = cache.book.remove(i);
    cache.bound.insert(hash, quote);
    cache.bound_sells.push((now, sell, amount));
    Ok(())
}

/// Binds an unseen open order to the quote it matches, or marks it seen.
fn consider(
    cfg: &DealerConfig,
    chain: &Chain,
    cache: &Mutex<Cache>,
    state: &Path,
    seen: &mut HashSet<[u8; 32]>,
    bound: &mut HashMap<[u8; 32], OrderUtxo>,
    o: TakerOrder,
) -> Result<()> {
    let hash = o.order_utxo.hash(0)?;
    if seen.contains(&hash) || bound.contains_key(&hash) {
        return Ok(());
    }
    if chain.status(&o.order_utxo)? != Status::Open {
        seen.insert(hash);
        return Ok(());
    }
    let now = chain.now()?;
    let mut cache = cache.lock().map_err(|_| anyhow!("cache poisoned"))?;
    match bind(cfg, &mut cache, hash, &o, now) {
        Ok(()) => {
            bound.insert(hash, o.order_utxo);
            save_state(state, &cache)?;
        }
        Err(why) => {
            // The hourly window moves on, so a later poll may take it.
            if why != OVER_LIMIT {
                seen.insert(hash);
            }
            log!("dealer: order {} {why}; not settling", hex(&hash));
        }
    }
    Ok(())
}

/// Settles a bound order once its quote still holds at prices read just
/// before the take.
fn settle(
    cfg: &DealerConfig,
    pricer: &Pricer,
    chain: &Chain,
    kp: &ShieldedKeypair,
    order: &OrderUtxo,
) -> Result<Settle> {
    let hash = hex(&order.hash(0)?);
    // A take may have landed even though its send reported an error.
    let status = chain.status(order)?;
    if status != Status::Open {
        log!("dealer: order {hash} is {status:?}");
        return Ok(Settle::Gone);
    }
    if chain.now()?.saturating_add(cfg.settle_margin_secs) >= order.terms.expiry {
        log!("dealer: order {hash} passed the settle margin unsettled");
        return Ok(Settle::Gone);
    }
    let t = &order.terms;
    let (sell, buy) = (order.source_mint.asset, t.destination_mint.asset);
    let note = chain.exact_note(kp, buy, t.destination_amount)?;
    let decimals = (chain.mint_decimals(sell)?, chain.mint_decimals(buy)?);
    if !fresh_last_look(
        pricer,
        chain.client.rpc(),
        order,
        decimals,
        cfg.settle_tolerance_bps,
    ) {
        return Ok(Settle::Wait);
    }
    chain.take(kp, order, note)?;
    log!("dealer: settled order {hash}");
    Ok(Settle::Took)
}

#[cfg(test)]
mod tests {
    use solana_account::Account;

    const L: Cluster = Cluster::Local;
    use spr_client::ClientError;
    use spr_keypair::ShieldedKeypair;
    use spr_transaction::Mint;
    use swap_sdk::state::OrderTerms;

    use super::*;

    fn addr(b: u8) -> Address {
        Address::new_from_array([b; 32])
    }

    fn order(
        trader: Address,
        src: Address,
        src_amt: u64,
        dst: Address,
        dst_amt: u64,
        exp: u64,
    ) -> TakerOrder {
        let maker = ShieldedKeypair::from_keypair(&Keypair::new_from_array([7; 32]))
            .unwrap()
            .shielded_address()
            .unwrap();
        TakerOrder {
            order_utxo: OrderUtxo {
                terms: OrderTerms {
                    destination_mint: Mint::new(dst, 0),
                    destination_amount: dst_amt,
                    maker,
                    expiry: exp,
                    taker: None,
                },
                blinding: [0; 32],
                source_mint: Mint::new(src, 2),
                source_amount: src_amt,
            },
            maker_pubkey: trader,
        }
    }

    const KEY: &str = "0123456789abcdef";

    struct Fake {
        limits: Mutex<Limits>,
    }

    impl Api for Fake {
        fn quote(&self, rfq: Rfq) -> Result<Quote, String> {
            if rfq.sell_amount == 1 {
                return Err("no quote".into());
            }
            Ok(Quote {
                buy_amount: rfq.sell_amount / 2,
                expiry: 1_000,
                signature: [0xab; 64],
            })
        }

        fn faucet(&self, wallet: Address) -> Result<(), (u16, String)> {
            let mut l = self.limits.lock().unwrap();
            l.take(wallet, 1).map_err(|m| (429, m.to_string()))
        }

        fn now(&self) -> Result<u64, String> {
            Ok(42)
        }
    }

    fn fake() -> Fake {
        Fake {
            limits: Mutex::new(Limits::new(1, 2)),
        }
    }

    fn call(
        remote: &str,
        method: &str,
        path: &str,
        key: Option<&str>,
        body: &str,
    ) -> (u16, String) {
        call_on(&fake(), remote, method, path, key, body)
    }

    fn call_on(
        api: &Fake,
        remote: &str,
        method: &str,
        path: &str,
        key: Option<&str>,
        body: &str,
    ) -> (u16, String) {
        let (status, v) = handle(
            api,
            KEY,
            Some(remote.parse().unwrap()),
            method,
            path,
            key,
            Some(body.len()),
            &mut body.as_bytes(),
        );
        (status, v.to_string())
    }

    const LOCAL: &str = "127.0.0.1:5000";

    fn rfq_json(amount: &str) -> String {
        format!(r#"{{"sell_mint":"a","buy_mint":"b","sell_amount":{amount},"trader":"c"}}"#)
    }

    #[test]
    fn http_quote_uses_string_amounts() {
        assert_eq!(
            call(
                LOCAL,
                "POST",
                "/quote",
                Some(KEY),
                &rfq_json(r#""18446744073709551614""#)
            ),
            (
                200,
                format!(
                    r#"{{"quote":{{"buy_amount":"9223372036854775807","expiry":1000,"signature":"{}"}}}}"#,
                    "ab".repeat(64)
                )
            )
        );
        assert_eq!(
            call(LOCAL, "POST", "/quote", Some(KEY), &rfq_json(r#""1""#)),
            (200, r#"{"error":"no quote"}"#.into())
        );
        for bad in [
            r#"100"#,
            r#""+100""#,
            r#""""#,
            r#""1e3""#,
            r#""18446744073709551616""#,
        ] {
            assert_eq!(
                call(LOCAL, "POST", "/quote", Some(KEY), &rfq_json(bad)).0,
                400,
                "{bad}"
            );
        }
        assert_eq!(
            call(LOCAL, "GET", "/health", Some(KEY), ""),
            (200, r#"{"ok":true,"now":42}"#.into())
        );
        assert_eq!(call(LOCAL, "GET", "/quote", Some(KEY), "").0, 404);
    }

    #[test]
    fn http_refuses_bad_key_oversize_and_non_loopback() {
        let body = rfq_json(r#""100""#);
        assert_eq!(call(LOCAL, "POST", "/quote", None, &body).0, 401);
        assert_eq!(
            call(LOCAL, "POST", "/quote", Some("0123456789abcdeX"), &body).0,
            401
        );
        assert_eq!(call(LOCAL, "POST", "/quote", Some("short"), &body).0, 401);
        assert_eq!(
            call("10.0.0.1:5000", "POST", "/quote", Some(KEY), &body).0,
            403
        );
        let big = format!("{body}{}", " ".repeat(MAX_BODY));
        assert_eq!(call(LOCAL, "POST", "/quote", Some(KEY), &big).0, 413);
    }

    #[test]
    fn http_faucet_caps() {
        let api = fake();
        let ask = |w: u8| {
            let body = format!(r#"{{"wallet":"{}"}}"#, addr(w));
            call_on(&api, LOCAL, "POST", "/faucet", Some(KEY), &body)
        };
        assert_eq!(ask(1), (200, r#"{"ok":true}"#.into()));
        assert_eq!(ask(1).0, 429);
        assert_eq!(ask(2).0, 200);
        assert_eq!(ask(3).0, 429);
        assert_eq!(
            call_on(
                &api,
                LOCAL,
                "POST",
                "/faucet",
                Some(KEY),
                r#"{"wallet":"nope"}"#
            )
            .0,
            400
        );
    }

    /// The HTTP server on a real socket with a short deadline.
    fn with_server(test: impl FnOnce(SocketAddr)) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let at = listener.local_addr().unwrap();
        let stop = AtomicBool::new(false);
        let api = fake();
        thread::scope(|s| {
            let server = s
                .spawn(|| serve_http(listener, KEY, &stop, &api, None, Duration::from_millis(400)));
            test(at);
            stop.store(true, Ordering::Relaxed);
            server.join().unwrap().unwrap();
        });
    }

    /// Sends `raw`, then reads the reply until the server closes.
    fn exchange(at: SocketAddr, raw: &[u8], pause_after: Option<Duration>) -> (String, Duration) {
        let started = Instant::now();
        let mut s = TcpStream::connect(at).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let _ = s.write_all(raw);
        if let Some(p) = pause_after {
            sleep(p);
        }
        let mut reply = String::new();
        let _ = s.read_to_string(&mut reply);
        (reply, started.elapsed())
    }

    fn health(at: SocketAddr) -> String {
        let raw = format!("GET /health HTTP/1.1\r\nHost: x\r\nx-keeler-key: {KEY}\r\n\r\n");
        exchange(at, raw.as_bytes(), None).0
    }

    /// Lend routes that echo the body size, slowly for `/lend/slow`.
    struct Echo;

    impl LendRoutes for Echo {
        fn lend(&self, _: &str, url: &str, body: &[u8], paused: bool) -> (u16, Value) {
            if url == "/lend/slow" {
                sleep(Duration::from_millis(1500));
            }
            (200, json!({ "len": body.len(), "paused": paused }))
        }
    }

    #[test]
    fn lend_routes_have_their_own_threads_and_a_16k_cap() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let at = listener.local_addr().unwrap();
        let stop = AtomicBool::new(false);
        let api = fake();
        let post = |path: &str, key: &str, len: usize| {
            format!(
                "POST {path} HTTP/1.1\r\nx-keeler-key: {key}\r\nContent-Length: {len}\r\n\r\n{}",
                "x".repeat(len)
            )
        };
        thread::scope(|s| {
            let server = s.spawn(|| {
                serve_http(
                    listener,
                    KEY,
                    &stop,
                    &api,
                    Some(&Echo),
                    Duration::from_millis(400),
                )
            });
            let (r, _) = exchange(at, post("/lend/ticket", KEY, 16_384).as_bytes(), None);
            assert!(r.ends_with(r#"{"len":16384,"paused":false}"#), "{r}");
            // The Worker's pause reaches the keeper.
            let paused = post("/lend/ticket", KEY, 1).replacen(
                "\r\n\r\n",
                "\r\nx-keeler-paused: 1\r\n\r\n",
                1,
            );
            let (r, _) = exchange(at, paused.as_bytes(), None);
            assert!(r.ends_with(r#"{"len":1,"paused":true}"#), "{r}");
            let (r, _) = exchange(at, post("/lend/ticket", KEY, 16_385).as_bytes(), None);
            assert!(r.starts_with("HTTP/1.1 413"), "{r}");
            let (r, _) = exchange(at, post("/lend/ticket", "nope", 10).as_bytes(), None);
            assert!(r.starts_with("HTTP/1.1 401"), "{r}");
            // The swap cap stays 4 KiB.
            let (r, _) = exchange(at, post("/quote", KEY, 4_097).as_bytes(), None);
            assert!(r.starts_with("HTTP/1.1 413"), "{r}");
            // A slow lend request does not hold up the swap routes.
            let slow = s.spawn(move || exchange(at, post("/lend/slow", KEY, 2).as_bytes(), None));
            sleep(Duration::from_millis(100));
            let started = Instant::now();
            assert!(health(at).starts_with("HTTP/1.1 200"));
            assert!(started.elapsed() < Duration::from_millis(1000));
            let (r, took) = slow.join().unwrap();
            assert!(
                r.ends_with(r#"{"len":2,"paused":false}"#) && took >= Duration::from_millis(1500),
                "{r}"
            );
            // Eight at once; a ninth is turned away, and swaps still answer.
            let slow: Vec<_> = (0..MAX_LEND_THREADS)
                .map(|_| s.spawn(move || exchange(at, post("/lend/slow", KEY, 2).as_bytes(), None)))
                .collect();
            sleep(Duration::from_millis(400));
            let (r, _) = exchange(at, post("/lend/ticket", KEY, 2).as_bytes(), None);
            assert!(r.starts_with("HTTP/1.1 503"), "{r}");
            assert!(health(at).starts_with("HTTP/1.1 200"));
            for h in slow {
                let (r, _) = h.join().unwrap();
                assert!(r.ends_with(r#"{"len":2,"paused":false}"#), "{r}");
            }
            stop.store(true, Ordering::Relaxed);
            server.join().unwrap().unwrap();
        });
    }

    #[test]
    fn http_server_on_a_socket() {
        with_server(|at| {
            let ok = health(at);
            assert!(ok.starts_with("HTTP/1.1 200 OK\r\n"), "{ok}");
            assert!(ok.contains("Connection: close"), "{ok}");
            assert!(ok.ends_with(r#"{"ok":true,"now":42}"#), "{ok}");

            let body = rfq_json(r#""100""#);
            let quote = format!(
                "POST /quote HTTP/1.1\r\nx-keeler-key: {KEY}\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let (reply, _) = exchange(at, quote.as_bytes(), None);
            assert!(reply.contains(r#""buy_amount":"50""#), "{reply}");

            // A bad key with a declared body: refused at once, nothing read.
            let bad = "POST /quote HTTP/1.1\r\nx-keeler-key: nope\r\nContent-Length: 100\r\n\r\n";
            let (reply, took) = exchange(at, bad.as_bytes(), None);
            assert!(reply.starts_with("HTTP/1.1 401"), "{reply}");
            assert!(took < Duration::from_millis(300), "{took:?}");

            // A huge declared length: refused at once, nothing read.
            let huge = format!(
                "POST /quote HTTP/1.1\r\nx-keeler-key: {KEY}\r\nContent-Length: 99999999999999999999999\r\n\r\n"
            );
            let (reply, took) = exchange(at, huge.as_bytes(), None);
            assert!(reply.starts_with("HTTP/1.1 413"), "{reply}");
            assert!(took < Duration::from_millis(300), "{took:?}");

            // A declared body that never comes: cut off at the deadline.
            let none = format!(
                "POST /quote HTTP/1.1\r\nx-keeler-key: {KEY}\r\nContent-Length: 50\r\n\r\n"
            );
            let (reply, took) = exchange(at, none.as_bytes(), None);
            assert!(reply.starts_with("HTTP/1.1 400"), "{reply}");
            assert!(took < Duration::from_secs(2), "{took:?}");

            // Chunked bodies are not accepted.
            let chunked = format!(
                "POST /quote HTTP/1.1\r\nx-keeler-key: {KEY}\r\nTransfer-Encoding: chunked\r\n\r\n"
            );
            assert!(exchange(at, chunked.as_bytes(), None)
                .0
                .starts_with("HTTP/1.1 400"));

            // A slow client that never finishes its headers.
            let (reply, took) = exchange(
                at,
                b"GET /health HTTP/1.1\r\nx-kee",
                Some(Duration::from_millis(800)),
            );
            assert!(reply.starts_with("HTTP/1.1 408"), "{reply}");
            assert!(took < Duration::from_secs(2), "{took:?}");

            // Headers past the cap.
            let long = format!(
                "GET /health HTTP/1.1\r\nx-pad: {}\r\n\r\n",
                "a".repeat(MAX_HEAD)
            );
            assert!(exchange(at, long.as_bytes(), None)
                .0
                .starts_with("HTTP/1.1 400"));

            // None of that stalled the server.
            assert!(health(at).starts_with("HTTP/1.1 200"));
        });
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("keeler-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn rfq_for(sell: Address, amount: u64, buy: Address, trader: Address) -> Rfq {
        Rfq {
            sell_mint: sell.to_string(),
            buy_mint: buy.to_string(),
            sell_amount: amount,
            trader: trader.to_string(),
        }
    }

    fn dealer() -> Keypair {
        Keypair::new_from_array([5; 32])
    }

    #[test]
    fn state_reload_keeps_live_signed_quotes_and_counters() {
        let path = temp("state.json");
        let trader = addr(9);
        let d = dealer();
        let signed =
            |rfq: &Rfq, buy, expiry| (rfq.clone(), sign_quote(&d, L, rfq, buy, expiry).unwrap());
        let live = rfq_for(addr(1), 100, addr(2), trader);
        let bound = rfq_for(addr(1), 200, addr(2), trader);
        let mut cache = Cache::default();
        cache.book.push(signed(&live, 50, 1_000));
        cache.book.push(signed(&live, 51, 500));
        let (r, mut forged) = signed(&live, 52, 1_000);
        forged.buy_amount = 99;
        cache.book.push((r, forged));
        cache.bound.insert([7; 32], signed(&bound, 90, 1_000));
        cache.bound_sells = vec![(100, addr(1), 7), (590, addr(1), 8), (595, addr(1), 200)];
        let mut faucet = Limits::new(1, 5);
        faucet.take(addr(4), 3).unwrap();
        cache.faucet = Some(faucet);
        save_state(&path, &cache).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        // A restart at chain time 600: the quote that expired at 500 and the
        // one whose signature fails are gone.
        let loaded = load_state(&path, 600, &d.pubkey(), L).unwrap();
        assert_eq!(loaded.quotes.len(), 2);
        assert!(loaded.quotes.iter().all(|(_, q)| q.expiry == 1_000));
        let o = order(trader, addr(1), 200, addr(2), 90, 1_000);
        assert!(loaded.quotes.iter().any(|(r, q)| matches(r, q, &o)));
        // Another dealer's key verifies none of them.
        assert!(load_state(&path, 600, &addr(3), L)
            .unwrap()
            .quotes
            .is_empty());
        // The hourly window and the faucet's day survive; the live bound
        // quote's sell is left to be counted when its order binds again.
        assert_eq!(
            loaded.bound_sells,
            vec![(100, addr(1), 7), (590, addr(1), 8)]
        );
        let later = load_state(&path, 3_700, &d.pubkey(), L).unwrap();
        assert_eq!(
            later.bound_sells,
            vec![(590, addr(1), 8), (595, addr(1), 200)]
        );
        let mut again = Limits::new(1, 5);
        again.claims = loaded.faucet.unwrap();
        assert!(again.take(addr(4), 3).is_err());
        assert!(load_state(&temp("missing.json"), 0, &d.pubkey(), L)
            .unwrap()
            .quotes
            .is_empty());
        std::fs::remove_file(&path).unwrap();
    }

    fn cfg(extra: &str) -> DealerConfig {
        DealerConfig::from_json(&format!(
            r#"{{"network":{{"rpc_url":"http://127.0.0.1:8899","indexer_url":"http://127.0.0.1:8784","prover":"local"}},
            "key":{{"file":"k.json"}},"assets":[{{"mint":"{}","price":{{"price":1,"expo":0}}}}],
            "listen":"127.0.0.1:47001","spread_bps":10,"quote_ttl_secs":60,"settle_margin_secs":10,
            "program_id":"{}"{extra}}}"#,
            addr(1),
            swap_program::ID
        ))
        .unwrap()
    }

    fn ready_cache() -> Cache {
        Cache {
            largest_note: HashMap::from([(addr(2), 1_000)]),
            fee_lamports: DEFAULT_MIN_FEE,
            ..Cache::default()
        }
    }

    #[test]
    fn admit_checks_pause_fees_notes_limits_and_book() {
        let state = temp("admit.json");
        let pause = temp("admit.pause");
        let c = cfg(&format!(
            r#","pause_file":"{}","limits":[{{"mint":"{}","max_sell_amount":100}}]"#,
            pause.display(),
            addr(1)
        ));
        let d = dealer();
        let rfq = rfq_for(addr(1), 100, addr(2), addr(9));
        let mut cache = ready_cache();
        let q = admit(&c, &mut cache, &state, (&d, L), rfq.clone(), 500, 10).unwrap();
        assert_eq!(q.expiry, 70);
        assert!(signed_by(&d.pubkey(), L, &rfq, &q));
        assert_eq!(
            load_state(&state, 10, &d.pubkey(), L).unwrap().quotes.len(),
            1
        );

        let refused = |cache: &mut Cache, rfq: Rfq, buy: u64| {
            admit(&c, cache, &state, (&d, L), rfq, buy, 10)
                .unwrap_err()
                .to_string()
        };
        assert!(refused(&mut cache, rfq.clone(), 1_001).contains("no single note"));
        assert!(
            refused(&mut cache, rfq_for(addr(1), 101, addr(2), addr(9)), 5)
                .contains("max_sell_amount")
        );
        std::fs::write(&pause, "").unwrap();
        assert_eq!(refused(&mut cache, rfq.clone(), 5), "paused");
        std::fs::remove_file(&pause).unwrap();
        std::fs::write(auto_pause_path(&state), "").unwrap();
        assert!(refused(&mut cache, rfq.clone(), 5).contains("failed settles"));
        std::fs::remove_file(auto_pause_path(&state)).unwrap();
        cache.fee_lamports = DEFAULT_MIN_FEE - 1;
        assert!(refused(&mut cache, rfq.clone(), 5).contains("fee SOL"));
        cache.fee_lamports = DEFAULT_MIN_FEE;

        // A full book, then a book whose state file cannot be written.
        let mut full = ready_cache();
        full.book = vec![(rfq.clone(), q); MAX_BOOK];
        assert!(refused(&mut full, rfq.clone(), 5).contains("book full"));
        let before = cache.book.len();
        let unwritable = temp("no-such-dir").join("state.json");
        let err = admit(&c, &mut cache, &unwritable, (&d, L), rfq, 5, 10).unwrap_err();
        assert!(
            format!("{err:#}").contains("save the dealer state"),
            "{err:#}"
        );
        assert_eq!(cache.book.len(), before, "rolled back");
        std::fs::remove_file(&state).unwrap();
    }

    #[test]
    fn auto_pause_after_failed_settles_and_resume() {
        let flag = temp("auto.autopaused");
        let mut cache = Cache::default();
        let fail = || Err(anyhow!("take failed"));
        after_settle(&mut cache, &flag, 3, &fail());
        after_settle(&mut cache, &flag, 3, &Ok(Settle::Wait));
        after_settle(&mut cache, &flag, 3, &fail());
        assert!(!flag.exists());
        after_settle(&mut cache, &flag, 3, &fail());
        assert!(flag.exists(), "third failure in a row pauses");
        after_settle(&mut cache, &flag, 3, &Ok(Settle::Took));
        assert!(!flag.exists(), "a settle resumes");
        // A success in between resets the count.
        after_settle(&mut cache, &flag, 3, &fail());
        after_settle(&mut cache, &flag, 3, &fail());
        after_settle(&mut cache, &flag, 3, &Ok(Settle::Took));
        after_settle(&mut cache, &flag, 3, &fail());
        assert!(!flag.exists());
        // Once the failing order is gone, nothing is left to settle: resume.
        after_settle(&mut cache, &flag, 3, &fail());
        after_settle(&mut cache, &flag, 3, &fail());
        assert!(flag.exists());
        clear_when_idle(&mut cache, &flag);
        assert!(!flag.exists() && cache.failed_settles == 0);
    }

    #[test]
    fn bind_checks_quote_limits_and_margin() {
        let c = cfg(&format!(
            r#","limits":[{{"mint":"{}","hourly_sell_cap":250}}]"#,
            addr(1)
        ));
        let trader = addr(9);
        let d = dealer();
        let mut cache = Cache::default();
        let quoted = |cache: &mut Cache, amount| {
            let r = rfq_for(addr(1), amount, addr(2), trader);
            let q = sign_quote(&d, L, &r, 50, 10_000).unwrap();
            cache.book.push((r, q));
        };
        let o = |amount| order(trader, addr(1), amount, addr(2), 50, 10_000);
        assert_eq!(
            bind(&c, &mut cache, [1; 32], &o(100), 0),
            Err("matches no quote")
        );
        quoted(&mut cache, 100);
        assert_eq!(bind(&c, &mut cache, [1; 32], &o(100), 0), Ok(()));
        assert!(cache.book.is_empty() && cache.bound.contains_key(&[1; 32]));
        quoted(&mut cache, 200);
        assert_eq!(
            bind(&c, &mut cache, [2; 32], &o(200), 10),
            Err("is over a sell limit")
        );
        // An hour later the first sell has left the window.
        assert_eq!(bind(&c, &mut cache, [2; 32], &o(200), HOUR), Ok(()));
        quoted(&mut cache, 30);
        assert_eq!(
            bind(&c, &mut cache, [3; 32], &o(30), 9_990),
            Err("is past the settle margin")
        );
    }

    /// Answers `getMultipleAccounts` with a Clock and one Pyth account.
    struct MockRpc {
        now: i64,
        pyth_price: Option<i64>,
    }

    impl Rpc for MockRpc {
        fn get_multiple_accounts(
            &self,
            addresses: Vec<Address>,
        ) -> Result<Vec<Option<Account>>, ClientError> {
            let mut clock = vec![0; 40];
            clock[32..40].copy_from_slice(&self.now.to_le_bytes());
            let account = |data, owner| Account {
                lamports: 1,
                data,
                owner,
                executable: false,
                rent_epoch: 0,
            };
            let mut out = vec![Some(account(clock, addr(0)))];
            for _ in 1..addresses.len() {
                out.push(self.pyth_price.map(|p| {
                    account(
                        pyth_account(p, self.now),
                        Address::new_from_array(keeler_core::price::PYTH_RECEIVER),
                    )
                }));
            }
            Ok(out)
        }
    }

    const FEED: [u8; 32] = [0xef; 32];

    /// A Full Pyth price update at expo -8.
    fn pyth_account(price: i64, published: i64) -> Vec<u8> {
        let mut d = vec![34, 241, 35, 99, 157, 126, 244, 205];
        d.extend([7; 32]);
        d.push(1);
        d.extend(FEED);
        d.extend(price.to_le_bytes());
        d.extend(0u64.to_le_bytes());
        d.extend((-8i32).to_le_bytes());
        d.extend(published.to_le_bytes());
        d.extend([0; 33]);
        d
    }

    /// Sells addr(1) at a fixed 2 USD for SOL priced by Pyth.
    fn pyth_pricer() -> Pricer {
        let common: crate::config::Common = serde_json::from_value(json!({
            "network": {"rpc_url": "http://127.0.0.1:8899", "indexer_url": "http://127.0.0.1:8784", "prover": "local"},
            "key": {"file": "k.json"},
            "assets": [
                {"mint": addr(1).to_string(), "price": {"price": 2, "expo": 0}},
                {"mint": addr(2).to_string(), "price": {"pyth": {"account": addr(8).to_string(), "feed_id": hex(&FEED)}}}
            ],
            "pyth": {"max_age_secs": 60, "max_conf_bps": 100}
        }))
        .unwrap();
        common.pricer().unwrap()
    }

    #[test]
    fn pricer_reads_pyth_through_one_rpc_call() {
        let pricer = pyth_pricer();
        let rpc = MockRpc {
            now: 1_000,
            pyth_price: Some(10_000_000_000),
        };
        let p = pricer.prices(&rpc, &[addr(1), addr(2)]).unwrap();
        assert_eq!((p[0].price, p[0].expo), (2, 0));
        assert_eq!((p[1].price, p[1].expo), (10_000_000_000, -8));
        let missing = MockRpc {
            now: 1_000,
            pyth_price: None,
        };
        assert!(pricer.prices(&missing, &[addr(2)]).is_err());
        // Fixed prices need no RPC at all.
        assert!(pricer.prices(&missing, &[addr(1)]).is_ok());
    }

    #[test]
    fn settle_declines_when_the_price_moved() {
        let pricer = pyth_pricer();
        // 100 base units (2 decimals) of a 2 USD token for SOL (0 decimals
        // in this order): fair is 2 / sol_price.
        let o = order(addr(9), addr(1), 100_000, addr(2), 20, 1_000).order_utxo;
        let at = |sol_usd: i64| MockRpc {
            now: 1_000,
            pyth_price: Some(sol_usd * 100_000_000),
        };
        // 1000 tokens at 2 USD for SOL at 100 USD: 20 SOL, as quoted.
        assert!(fresh_last_look(&pricer, &at(100), &o, (2, 0), 0));
        // SOL rose: 20 SOL is now worth more than the tokens.
        assert!(!fresh_last_look(&pricer, &at(101), &o, (2, 0), 0));
        assert!(fresh_last_look(&pricer, &at(101), &o, (2, 0), 600));
        // No price, no settlement.
        let gone = MockRpc {
            now: 1_000,
            pyth_price: None,
        };
        assert!(!fresh_last_look(&pricer, &gone, &o, (2, 0), 10_000));
    }

    #[test]
    fn last_look_settles_only_within_tolerance() {
        assert!(last_look(1_000, Some(1_000), 0));
        assert!(!last_look(1_001, Some(1_000), 0), "price moved against us");
        assert!(last_look(1_010, Some(1_000), 100));
        assert!(!last_look(1_011, Some(1_000), 100));
        assert!(!last_look(1, None, 10_000), "no price, no settlement");
    }

    #[test]
    fn sell_limits_per_order_and_rolling_hour() {
        let limits = [SellLimit {
            mint: addr(1).to_string(),
            max_sell_amount: Some(100),
            hourly_sell_cap: Some(250),
        }];
        assert!(within_limits(&limits, &[], addr(1), 100, 0).is_ok());
        assert!(within_limits(&limits, &[], addr(1), 101, 0).is_err());
        assert!(
            within_limits(&limits, &[], addr(2), 1_000, 0).is_ok(),
            "other mint"
        );
        let sells = [(1_000, addr(1), 100), (2_000, addr(1), 100)];
        assert!(within_limits(&limits, &sells, addr(1), 50, 2_100).is_ok());
        assert!(within_limits(&limits, &sells, addr(1), 51, 2_100).is_err());
        // The first sell leaves the window an hour later.
        assert!(within_limits(&limits, &sells, addr(1), 100, 1_000 + HOUR).is_ok());
    }

    #[test]
    fn pause_file_stops_quotes() {
        let path = temp("pause");
        assert!(check_pause(None).is_ok());
        assert!(check_pause(Some(&path)).is_ok());
        std::fs::write(&path, "").unwrap();
        assert!(check_pause(Some(&path)).is_err());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn matches_only_the_exact_terms() {
        let trader = Keypair::new_from_array([9; 32]).pubkey();
        let (sell, buy) = (addr(1), addr(2));
        let rfq = Rfq {
            sell_mint: sell.to_string(),
            buy_mint: buy.to_string(),
            sell_amount: 100,
            trader: trader.to_string(),
        };
        let q = Quote {
            buy_amount: 50,
            expiry: 1_000,
            signature: [0; 64],
        };
        assert!(matches(&rfq, &q, &order(trader, sell, 100, buy, 50, 1_000)));
        let other = [
            ("trader key", order(addr(3), sell, 100, buy, 50, 1_000)),
            ("source mint", order(trader, addr(3), 100, buy, 50, 1_000)),
            ("source amount", order(trader, sell, 101, buy, 50, 1_000)),
            (
                "destination mint",
                order(trader, sell, 100, addr(3), 50, 1_000),
            ),
            (
                "destination amount",
                order(trader, sell, 100, buy, 49, 1_000),
            ),
            ("expiry", order(trader, sell, 100, buy, 50, 1_001)),
        ];
        for (field, o) in other {
            assert!(!matches(&rfq, &q, &o), "{field} differs but matched");
        }
    }
}
