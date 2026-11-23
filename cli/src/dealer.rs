//! The dealer: quote from cached balances, settle only orders that match a
//! remembered quote exactly.
use std::{
    collections::{HashMap, HashSet},
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    thread::{self, sleep},
    time::Duration,
};

use anyhow::{anyhow, ensure, Result};
use keeler_core::{
    price::apply_spread,
    rfq::{serve, Quote, Rfq},
};
use solana_address::Address;
use spr_keypair::ShieldedKeypair;
use swap_sdk::{index::TakerOrder, state::OrderUtxo};

use crate::{
    chain::{hex, shielded, Chain, Status},
    config::{parse_address, DealerConfig},
    trader::fair_amount,
};

const POLL: Duration = Duration::from_secs(1);
const DISCOVER_WAIT: Duration = Duration::from_secs(1);
const MAX_BOOK: usize = 1024;

#[derive(Default)]
struct Cache {
    /// Quotes given and not yet bound to an order.
    book: Vec<(Rfq, Quote)>,
    balances: HashMap<Address, u64>,
}

/// Serves quotes and settles until `stop` is set.
pub fn run(cfg: &DealerConfig, stop: &AtomicBool) -> Result<()> {
    let c = &cfg.common;
    let chain = Chain::connect(&c.rpc_url, &c.indexer_url, &c.mints()?)?;
    let kp = shielded(&c.keypair()?)?;
    chain.register(&kp)?;
    let cache = Mutex::new(Cache::default());
    refresh(cfg, &chain, &kp, &cache)?;
    let listener = TcpListener::bind(cfg.listen)?;
    thread::scope(|s| {
        let server = s.spawn(|| {
            serve(listener, stop, |rfq| {
                quote(cfg, &chain, &cache, rfq).map_err(|e| format!("{e:#}"))
            })
        });
        let mut seen = HashSet::new();
        let mut bound = HashMap::new();
        while !stop.load(Ordering::Relaxed) {
            if let Err(e) = poll(cfg, &chain, &kp, &cache, &mut seen, &mut bound) {
                eprintln!("dealer: {e:#}");
            }
            sleep(POLL);
        }
        server
            .join()
            .map_err(|_| anyhow!("quote server panicked"))??;
        Ok(())
    })
}

fn quote(cfg: &DealerConfig, chain: &Chain, cache: &Mutex<Cache>, rfq: Rfq) -> Result<Quote> {
    let sell = parse_address(&rfq.sell_mint)?;
    let buy = parse_address(&rfq.buy_mint)?;
    parse_address(&rfq.trader)?;
    let fair = fair_amount(&cfg.common, chain, sell, buy, rfq.sell_amount)?;
    let buy_amount = apply_spread(fair, cfg.spread_bps).ok_or_else(|| anyhow!("bad spread"))?;
    ensure!(buy_amount > 0, "amount too small");
    let now = chain.now()?;
    let mut cache = cache.lock().map_err(|_| anyhow!("cache poisoned"))?;
    let have = cache.balances.get(&buy).copied().unwrap_or(0);
    ensure!(have >= buy_amount, "insufficient balance");
    cache.book.retain(|(_, q)| q.expiry > now);
    ensure!(cache.book.len() < MAX_BOOK, "quote book full");
    let q = Quote {
        buy_amount,
        expiry: now + cfg.quote_ttl_secs,
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
    let mut balances = HashMap::new();
    for mint in cfg.common.mints()? {
        balances.insert(mint, chain.balance(kp, mint)?);
    }
    cache
        .lock()
        .map_err(|_| anyhow!("cache poisoned"))?
        .balances = balances;
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

fn poll(
    cfg: &DealerConfig,
    chain: &Chain,
    kp: &ShieldedKeypair,
    cache: &Mutex<Cache>,
    seen: &mut HashSet<[u8; 32]>,
    bound: &mut HashMap<[u8; 32], OrderUtxo>,
) -> Result<()> {
    refresh(cfg, chain, kp, cache)?;
    let margin = cfg.settle_margin_secs;
    for o in chain.taker_orders(kp, DISCOVER_WAIT)? {
        let hash = o.order_utxo.hash(0)?;
        if seen.contains(&hash) || bound.contains_key(&hash) {
            continue;
        }
        if chain.status(&o.order_utxo)? != Status::Open {
            seen.insert(hash);
            continue;
        }
        let now = chain.now()?;
        let mut cache = cache.lock().map_err(|_| anyhow!("cache poisoned"))?;
        match cache.book.iter().position(|(r, q)| matches(r, q, &o)) {
            Some(i) if now + margin < o.order_utxo.terms.expiry => {
                cache.book.remove(i);
                bound.insert(hash, o.order_utxo);
            }
            Some(_) => {
                seen.insert(hash);
                eprintln!("dealer: order {} is past the settle margin", hex(&hash));
            }
            None => {
                seen.insert(hash);
                eprintln!(
                    "dealer: order {} matches no quote; not settling",
                    hex(&hash)
                );
            }
        }
    }
    for (hash, order) in bound.clone() {
        if chain.now()? + margin >= order.terms.expiry {
            bound.remove(&hash);
            seen.insert(hash);
            eprintln!(
                "dealer: order {} passed the settle margin unsettled",
                hex(&hash)
            );
            continue;
        }
        let t = &order.terms;
        let settled = chain
            .exact_note(kp, t.destination_mint.asset, t.destination_amount)
            .and_then(|note| chain.take(kp, &order, note));
        match settled {
            Ok(()) => {
                bound.remove(&hash);
                seen.insert(hash);
                eprintln!("dealer: settled order {}", hex(&hash));
            }
            Err(e) => eprintln!("dealer: settle {} failed, will retry: {e:#}", hex(&hash)),
        }
    }
    Ok(())
}
