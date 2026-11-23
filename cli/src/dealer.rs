//! The dealer: quote from cached note sizes, settle only orders that match a
//! remembered quote exactly. Quotes over line-delimited TCP and, when
//! configured, over HTTP with the same handler.
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    net::{SocketAddr, TcpListener},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    thread::{self, sleep},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, ensure, Result};
use keeler_core::{
    price::apply_spread,
    rfq::{serve, Quote, Rfq},
};
use serde::Deserialize;
use serde_json::{json, Value};
use solana_address::Address;
use spr_keypair::ShieldedKeypair;
use swap_sdk::{index::TakerOrder, state::OrderUtxo};

use crate::{
    chain::{hex, shielded, Chain, Status},
    config::{parse_address, DealerConfig, Pricer, Prover},
    faucet::{Faucet, Limits},
    trader::fair_amount,
};

const DISCOVER_WAIT: Duration = Duration::from_secs(1);
const MAX_BOOK: usize = 1024;
/// Largest HTTP request body accepted.
const MAX_BODY: usize = 4096;
const HTTP_POLL: Duration = Duration::from_millis(100);

#[derive(Default)]
struct Cache {
    /// Quotes given and not yet bound to an order.
    book: Vec<(Rfq, Quote)>,
    /// The largest single note per mint: `take` spends one note of exactly
    /// the quoted amount, split from one note that covers it.
    largest_note: HashMap<Address, u64>,
}

/// Serves quotes and settles until `stop` is set, or fails when the quote
/// server stops on its own.
pub fn run(cfg: &DealerConfig, stop: &AtomicBool) -> Result<()> {
    let c = &cfg.common;
    let pricer = c.pricer()?;
    let chain = Chain::connect(&c.network, &c.mints()?)?;
    let kp = shielded(&c.keypair()?)?;
    chain.register(&kp)?;
    let cache = Mutex::new(Cache::default());
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
            poll(cfg, &chain, &kp, &cache, &mut seen, &mut bound);
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
}

impl Api for DealerApi<'_> {
    fn quote(&self, rfq: Rfq) -> Result<Quote, String> {
        quote(self.cfg, self.pricer, self.chain, self.cache, rfq).map_err(|e| {
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
                Ok(q) => (
                    200,
                    json!({ "quote": { "buy_amount": q.buy_amount.to_string(), "expiry": q.expiry } }),
                ),
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

fn quote(
    cfg: &DealerConfig,
    pricer: &Pricer,
    chain: &Chain,
    cache: &Mutex<Cache>,
    rfq: Rfq,
) -> Result<Quote> {
    let sell = parse_address(&rfq.sell_mint)?;
    let buy = parse_address(&rfq.buy_mint)?;
    parse_address(&rfq.trader)?;
    let fair = fair_amount(pricer, chain, sell, buy, rfq.sell_amount)?;
    let buy_amount = apply_spread(fair, cfg.spread_bps).ok_or_else(|| anyhow!("bad spread"))?;
    ensure!(buy_amount > 0, "amount too small");
    let now = chain.now()?;
    let mut cache = cache.lock().map_err(|_| anyhow!("cache poisoned"))?;
    let largest = cache.largest_note.get(&buy).copied().unwrap_or(0);
    ensure!(largest >= buy_amount, "no single note covers {buy_amount}");
    cache.book.retain(|(_, q)| q.expiry > now);
    ensure!(cache.book.len() < MAX_BOOK, "quote book full");
    let q = Quote {
        buy_amount,
        expiry: now.saturating_add(cfg.quote_ttl_secs),
    };
    cache.book.push((rfq, q));
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
fn poll(
    cfg: &DealerConfig,
    chain: &Chain,
    kp: &ShieldedKeypair,
    cache: &Mutex<Cache>,
    seen: &mut HashSet<[u8; 32]>,
    bound: &mut HashMap<[u8; 32], OrderUtxo>,
) {
    if let Err(e) = refresh(cfg, chain, kp, cache) {
        log!("dealer: refresh notes: {e:#}");
    }
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
        match settle(cfg, chain, kp, &order) {
            Ok(true) => {
                bound.remove(&hash);
                seen.insert(hash);
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
    match cache.book.iter().position(|(r, q)| matches(r, q, &o)) {
        Some(i) if now.saturating_add(cfg.settle_margin_secs) < o.order_utxo.terms.expiry => {
            cache.book.remove(i);
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
    let note = chain.exact_note(kp, t.destination_mint.asset, t.destination_amount)?;
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
                r#"{"quote":{"buy_amount":"9223372036854775807","expiry":1000}}"#.into()
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
