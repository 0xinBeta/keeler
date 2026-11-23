//! The dealer: quote from cached note sizes, settle only orders that match a
//! remembered quote exactly, and only while the current price still covers
//! it. Quotes are signed, kept in a state file across restarts, and served
//! over line-delimited TCP and, when configured, over HTTP with the same
//! handler.
use std::{
    collections::{HashMap, HashSet},
    io::{Read, Write},
    net::{SocketAddr, TcpListener},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    thread::{self, sleep},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, ensure, Result};
use keeler_core::{
    price::{apply_spread, Price},
    rfq::{serve, Quote, Rfq},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use solana_address::Address;
use solana_keypair::Keypair;
use solana_signer::Signer;
use spr_keypair::ShieldedKeypair;
use swap_sdk::{index::TakerOrder, state::OrderUtxo};

use crate::{
    chain::{hex, shielded, Chain, Status},
    config::{parse_address, DealerConfig, Pricer, Prover, SellLimit},
    faucet::{Faucet, Limits},
    trader::{fair_amount, fair_at, sign_quote},
};

const DISCOVER_WAIT: Duration = Duration::from_secs(1);
const MAX_BOOK: usize = 1024;
/// Largest HTTP request body accepted.
const MAX_BODY: usize = 4096;
const HTTP_POLL: Duration = Duration::from_millis(100);
const HOUR: u64 = 3_600;

#[derive(Default)]
struct Cache {
    /// Quotes given and not yet bound to an order.
    book: Vec<(Rfq, Quote)>,
    /// The quote each bound order matched, by order hash, until the order is
    /// done with.
    bound: HashMap<[u8; 32], (Rfq, Quote)>,
    /// (chain time, sell mint, sell amount) of orders bound in the last hour.
    // shortcut: not in the state file, so a restart resets the hourly window;
    // persist it if the cap must hold across restarts.
    bound_sells: Vec<(u64, Address, u64)>,
    /// The largest single note per mint: `take` spends one note of exactly
    /// the quoted amount, split from one note that covers it.
    largest_note: HashMap<Address, u64>,
}

/// What the state file holds.
#[derive(Default, Serialize, Deserialize)]
struct State {
    book: Vec<(Rfq, Quote)>,
    bound: Vec<(Rfq, Quote)>,
}

/// Writes the book and the bound quotes to `path` atomically, readable by
/// this user only.
fn save_state(path: &Path, cache: &Cache) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let state = State {
        book: cache.book.clone(),
        bound: cache.bound.values().cloned().collect(),
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

/// The unexpired quotes from `path`, bound ones included: the poll matches a
/// bound order to its quote again, so it still settles after a restart.
fn load_state(path: &Path, now: u64) -> Result<Vec<(Rfq, Quote)>> {
    let text = match std::fs::read(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let state: State = serde_json::from_slice(&text)
        .map_err(|_| anyhow!("state file {} is not valid", path.display()))?;
    Ok(state
        .book
        .into_iter()
        .chain(state.bound)
        .filter(|(_, q)| q.expiry > now)
        .collect())
}

fn state_path(cfg: &DealerConfig, dealer: &Address) -> Result<PathBuf> {
    if let Some(p) = &cfg.state_file {
        return Ok(p.clone());
    }
    let dir = PathBuf::from(std::env::var("HOME")?).join(".local/state/keeler");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("dealer-{dealer}.json")))
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

/// Serves quotes and settles until `stop` is set, or fails when the quote
/// server stops on its own.
pub fn run(cfg: &DealerConfig, stop: &AtomicBool) -> Result<()> {
    let c = &cfg.common;
    let pricer = c.pricer()?;
    let chain = Chain::connect(&c.network, &c.mints()?)?;
    let signer = c.keypair()?;
    let kp = shielded(&signer)?;
    chain.register(&kp)?;
    let state = state_path(cfg, &signer.pubkey())?;
    let cache = Mutex::new(Cache {
        book: load_state(&state, chain.now()?)?,
        ..Cache::default()
    });
    refresh(cfg, &chain, &kp, &cache)?;
    let faucet = match &cfg.faucet {
        Some(f) => Some((
            Faucet::new(f)?,
            Mutex::new(Limits::new(f.per_wallet_daily, f.daily_cap)),
        )),
        None => None,
    };
    let api = DealerApi {
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
            let server = tiny_http::Server::http(h.listen)
                .map_err(|e| anyhow!("listen on {}: {e}", h.listen))?;
            Some((server, key))
        }
        None => None,
    };
    thread::scope(|s| {
        let server = s.spawn(|| serve(listener, stop, |rfq| api.quote(rfq)));
        let web = http
            .as_ref()
            .map(|(server, key)| s.spawn(|| serve_http(server, key, stop, &api)));
        let mut seen = HashSet::new();
        let mut bound = HashMap::new();
        while !stop.load(Ordering::Relaxed)
            && !server.is_finished()
            && !web.as_ref().is_some_and(|w| w.is_finished())
        {
            poll(
                cfg, &pricer, &chain, &kp, &cache, &state, &mut seen, &mut bound,
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
        let served = server.join();
        match served {
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
    faucet: Option<&'a (Faucet, Mutex<Limits>)>,
    signer: &'a Keypair,
    state: &'a Path,
}

impl Api for DealerApi<'_> {
    fn quote(&self, rfq: Rfq) -> Result<Quote, String> {
        quote(self, rfq).map_err(|e| {
            log!("dealer: no quote: {e:#}");
            "no quote".to_string()
        })
    }

    fn faucet(&self, wallet: Address) -> Result<(), (u16, String)> {
        let (faucet, limits) = self.faucet.ok_or((404, "the faucet is off".to_string()))?;
        let day = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() / 86_400);
        let lock = || {
            limits
                .lock()
                .map_err(|_| (500, "faucet poisoned".to_string()))
        };
        lock()?
            .take(wallet, day)
            .map_err(|m| (429, m.to_string()))?;
        faucet.pay(self.chain, wallet).map_err(|e| {
            log!("dealer: faucet for {wallet}: {e:#}");
            if let Ok(mut l) = lock() {
                l.undo(wallet);
            }
            (502, "faucet transfer failed".to_string())
        })
    }

    fn now(&self) -> Result<u64, String> {
        self.chain.now().map_err(|e| {
            log!("dealer: chain time: {e:#}");
            "no chain time".to_string()
        })
    }
}

/// Serves the HTTP API one request at a time until `stop` is set.
fn serve_http(
    server: &tiny_http::Server,
    key: &str,
    stop: &AtomicBool,
    api: &impl Api,
) -> Result<()> {
    let json = tiny_http::Header::from_bytes("Content-Type", "application/json")
        .map_err(|_| anyhow!("bad header"))?;
    while !stop.load(Ordering::Relaxed) {
        let Some(mut req) = server.recv_timeout(HTTP_POLL)? else {
            continue;
        };
        let given = req
            .headers()
            .iter()
            .find(|h| h.field.equiv("x-keeler-key"))
            .map(|h| h.value.as_str().to_owned());
        let method = req.method().as_str().to_owned();
        let url = req.url().to_owned();
        let (remote, length) = (req.remote_addr().copied(), req.body_length());
        let (status, body) = handle(
            api,
            key,
            remote,
            &method,
            &url,
            given.as_deref(),
            length,
            req.as_reader(),
        );
        let reply = tiny_http::Response::from_string(body.to_string())
            .with_status_code(status)
            .with_header(json.clone());
        // A client that hung up only loses its own reply.
        let _ = req.respond(reply);
    }
    Ok(())
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

/// One HTTP request. The key is checked before any body byte is read.
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
    if !remote.is_some_and(|r| r.ip().is_loopback()) {
        return error(403, "loopback only");
    }
    if !given_key.is_some_and(|g| same_key(g.as_bytes(), key.as_bytes())) {
        return error(401, "bad key");
    }
    if length.is_some_and(|l| l > MAX_BODY) {
        return error(413, "body too large");
    }
    let mut buf = Vec::new();
    if body
        .take(MAX_BODY as u64 + 1)
        .read_to_end(&mut buf)
        .is_err()
    {
        return error(400, "bad request");
    }
    if buf.len() > MAX_BODY {
        return error(413, "body too large");
    }
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
    let cfg = api.cfg;
    check_pause(cfg.pause_file.as_deref())?;
    let sell = parse_address(&rfq.sell_mint)?;
    let buy = parse_address(&rfq.buy_mint)?;
    parse_address(&rfq.trader)?;
    let fair = fair_amount(api.pricer, api.chain, sell, buy, rfq.sell_amount)?;
    let buy_amount = apply_spread(fair, cfg.spread_bps).ok_or_else(|| anyhow!("bad spread"))?;
    ensure!(buy_amount > 0, "amount too small");
    let now = api.chain.now()?;
    let mut cache = api.cache.lock().map_err(|_| anyhow!("cache poisoned"))?;
    let largest = cache.largest_note.get(&buy).copied().unwrap_or(0);
    ensure!(largest >= buy_amount, "no single note covers {buy_amount}");
    within_limits(&cfg.limits, &cache.bound_sells, sell, rfq.sell_amount, now)?;
    cache.book.retain(|(_, q)| q.expiry > now);
    ensure!(cache.book.len() < MAX_BOOK, "quote book full");
    let q = sign_quote(
        api.signer,
        &rfq,
        buy_amount,
        now.saturating_add(cfg.quote_ttl_secs),
    )?;
    cache.book.push((rfq, q));
    // A quote the state file does not hold would be forgotten on restart.
    if let Err(e) = save_state(api.state, &cache) {
        cache.book.pop();
        return Err(e.context("save the dealer state"));
    }
    Ok(q)
}

fn refresh(
    cfg: &DealerConfig,
    chain: &Chain,
    kp: &ShieldedKeypair,
    cache: &Mutex<Cache>,
) -> Result<()> {
    let mut largest = HashMap::new();
    for mint in cfg.common.mints()? {
        let notes = chain.notes(kp, mint)?;
        largest.insert(mint, notes.iter().map(|u| u.utxo.amount).max().unwrap_or(0));
    }
    cache
        .lock()
        .map_err(|_| anyhow!("cache poisoned"))?
        .largest_note = largest;
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

/// One pass. Errors are logged per step and per order, so one bad order or a
/// failed discovery never stops settling orders already bound.
#[allow(clippy::too_many_arguments)]
fn poll(
    cfg: &DealerConfig,
    pricer: &Pricer,
    chain: &Chain,
    kp: &ShieldedKeypair,
    cache: &Mutex<Cache>,
    state: &Path,
    seen: &mut HashSet<[u8; 32]>,
    bound: &mut HashMap<[u8; 32], OrderUtxo>,
) {
    if let Err(e) = refresh(cfg, chain, kp, cache) {
        log!("dealer: refresh notes: {e:#}");
    }
    // Once per poll; a failed fetch settles nothing this pass.
    let prices = cfg.common.mints().and_then(|mints| {
        let p = pricer.prices(&mints)?;
        Ok(mints.into_iter().zip(p).collect::<HashMap<_, _>>())
    });
    let prices = match prices {
        Ok(p) => Some(p),
        Err(e) => {
            log!("dealer: prices: {e:#}; not settling this pass");
            None
        }
    };
    match chain.taker_orders(kp, DISCOVER_WAIT) {
        Ok(orders) => {
            for o in orders {
                if let Err(e) = consider(cfg, chain, cache, seen, bound, o) {
                    log!("dealer: new order: {e:#}");
                }
            }
        }
        Err(e) => log!("dealer: discover orders: {e:#}"),
    }
    for (hash, order) in bound.clone() {
        match settle(cfg, chain, kp, &order, prices.as_ref()) {
            Ok(true) => {
                bound.remove(&hash);
                seen.insert(hash);
                if let Ok(mut c) = cache.lock() {
                    c.bound.remove(&hash);
                    if let Err(e) = save_state(state, &c) {
                        log!("dealer: save state: {e:#}");
                    }
                }
            }
            Ok(false) => {}
            Err(e) => log!("dealer: settle {} failed, will retry: {e:#}", hex(&hash)),
        }
    }
}

/// Binds an unseen open order to the quote it matches, or marks it seen.
fn consider(
    cfg: &DealerConfig,
    chain: &Chain,
    cache: &Mutex<Cache>,
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
    cache
        .bound_sells
        .retain(|(t, _, _)| t.saturating_add(HOUR) > now);
    let (sell, amount) = (o.order_utxo.source_mint.asset, o.order_utxo.source_amount);
    match cache.book.iter().position(|(r, q)| matches(r, q, &o)) {
        Some(_) if within_limits(&cfg.limits, &cache.bound_sells, sell, amount, now).is_err() => {
            seen.insert(hash);
            log!(
                "dealer: order {} is over a sell limit; not settling",
                hex(&hash)
            );
        }
        Some(i) if now.saturating_add(cfg.settle_margin_secs) < o.order_utxo.terms.expiry => {
            let quote = cache.book.remove(i);
            cache.bound.insert(hash, quote);
            cache.bound_sells.push((now, sell, amount));
            bound.insert(hash, o.order_utxo);
        }
        Some(_) => {
            seen.insert(hash);
            log!("dealer: order {} is past the settle margin", hex(&hash));
        }
        None => {
            seen.insert(hash);
            log!(
                "dealer: order {} matches no quote; not settling",
                hex(&hash)
            );
        }
    }
    Ok(())
}

/// Settles a bound order. `Ok(true)` when it is done with: settled, already
/// spent, or past the settle margin.
fn settle(
    cfg: &DealerConfig,
    chain: &Chain,
    kp: &ShieldedKeypair,
    order: &OrderUtxo,
    prices: Option<&HashMap<Address, Price>>,
) -> Result<bool> {
    let hash = hex(&order.hash(0)?);
    // A take may have landed even though its send reported an error.
    let status = chain.status(order)?;
    if status != Status::Open {
        log!("dealer: order {hash} is {status:?}");
        return Ok(true);
    }
    if chain.now()?.saturating_add(cfg.settle_margin_secs) >= order.terms.expiry {
        log!("dealer: order {hash} passed the settle margin unsettled");
        return Ok(true);
    }
    let t = &order.terms;
    let (sell, buy) = (order.source_mint.asset, t.destination_mint.asset);
    let fair_now = prices.and_then(|p| {
        fair_at(
            *p.get(&sell)?,
            *p.get(&buy)?,
            chain,
            sell,
            buy,
            order.source_amount,
        )
        .ok()
    });
    if !last_look(t.destination_amount, fair_now, cfg.settle_tolerance_bps) {
        log!("dealer: order {hash} declined at last look (fair now {fair_now:?})");
        return Ok(false);
    }
    let note = chain.exact_note(kp, buy, t.destination_amount)?;
    chain.take(kp, order, note)?;
    log!("dealer: settled order {hash}");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use solana_keypair::Keypair;
    use solana_signer::Signer;
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

    fn call(
        remote: &str,
        method: &str,
        path: &str,
        key: Option<&str>,
        body: &str,
    ) -> (u16, String) {
        let api = Fake {
            limits: Mutex::new(Limits::new(1, 2)),
        };
        call_on(&api, remote, method, path, key, body)
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
        // A body longer than its declared length is still capped.
        let api = Fake {
            limits: Mutex::new(Limits::new(1, 2)),
        };
        let (status, _) = handle(
            &api,
            KEY,
            Some(LOCAL.parse().unwrap()),
            "POST",
            "/quote",
            Some(KEY),
            None,
            &mut big.as_bytes(),
        );
        assert_eq!(status, 413);
    }

    #[test]
    fn http_faucet_caps() {
        let api = Fake {
            limits: Mutex::new(Limits::new(1, 2)),
        };
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

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("keeler-{}-{name}", std::process::id()))
    }

    fn rfq_for(sell: Address, amount: u64, buy: Address, trader: Address) -> Rfq {
        Rfq {
            sell_mint: sell.to_string(),
            buy_mint: buy.to_string(),
            sell_amount: amount,
            trader: trader.to_string(),
        }
    }

    #[test]
    fn state_reload_keeps_live_quotes_and_bound_orders() {
        let path = temp("state.json");
        let trader = addr(9);
        let q = |buy_amount, expiry| Quote {
            buy_amount,
            expiry,
            signature: [1; 64],
        };
        let mut cache = Cache::default();
        cache
            .book
            .push((rfq_for(addr(1), 100, addr(2), trader), q(50, 1_000)));
        cache
            .book
            .push((rfq_for(addr(1), 100, addr(2), trader), q(51, 500)));
        cache.bound.insert(
            [7; 32],
            (rfq_for(addr(1), 200, addr(2), trader), q(90, 1_000)),
        );
        save_state(&path, &cache).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        // A restart at chain time 600: the quote that expired at 500 is gone.
        let book = load_state(&path, 600).unwrap();
        assert_eq!(book.len(), 2);
        assert!(book.iter().all(|(_, q)| q.expiry == 1_000));
        // The bound order matches its quote again, so the poll binds and
        // settles it.
        let o = order(trader, addr(1), 200, addr(2), 90, 1_000);
        assert!(book.iter().any(|(r, q)| matches(r, q, &o)));
        assert!(load_state(&temp("missing.json"), 0).unwrap().is_empty());
        std::fs::remove_file(&path).unwrap();
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
