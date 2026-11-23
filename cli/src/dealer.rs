//! The dealer: quote from cached note sizes, settle only orders that match a
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

use anyhow::{anyhow, bail, ensure, Result};
use keeler_core::{
    price::apply_spread,
    rfq::{serve, Quote, Rfq},
};
use solana_address::Address;
use spr_keypair::ShieldedKeypair;
use swap_sdk::{index::TakerOrder, state::OrderUtxo};

use crate::{
    chain::{hex, shielded, Chain, Status},
    config::{parse_address, DealerConfig, Pricer},
    trader::fair_amount,
};

const POLL: Duration = Duration::from_secs(1);
const DISCOVER_WAIT: Duration = Duration::from_secs(1);
const MAX_BOOK: usize = 1024;

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
    let chain = Chain::connect(&c.rpc_url, &c.indexer_url, &c.mints()?)?;
    let kp = shielded(&c.keypair()?)?;
    chain.register(&kp)?;
    let cache = Mutex::new(Cache::default());
    refresh(cfg, &chain, &kp, &cache)?;
    let listener = TcpListener::bind(cfg.listen)?;
    thread::scope(|s| {
        let server = s.spawn(|| {
            serve(listener, stop, |rfq| {
                quote(cfg, &pricer, &chain, &cache, rfq).map_err(|e| {
                    eprintln!("dealer: no quote: {e:#}");
                    "no quote".to_string()
                })
            })
        });
        let mut seen = HashSet::new();
        let mut bound = HashMap::new();
        while !stop.load(Ordering::Relaxed) && !server.is_finished() {
            poll(cfg, &chain, &kp, &cache, &mut seen, &mut bound);
            sleep(POLL);
        }
        let stopped = stop.load(Ordering::Relaxed);
        let served = server.join();
        match served {
            Err(_) => bail!("quote server panicked"),
            Ok(Err(e)) => Err(anyhow!("quote server failed: {e}")),
            Ok(Ok(())) if !stopped => bail!("quote server stopped"),
            Ok(Ok(())) => Ok(()),
        }
    })
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
        eprintln!("dealer: refresh notes: {e:#}");
    }
    match chain.taker_orders(kp, DISCOVER_WAIT) {
        Ok(orders) => {
            for o in orders {
                if let Err(e) = consider(cfg, chain, cache, seen, bound, o) {
                    eprintln!("dealer: new order: {e:#}");
                }
            }
        }
        Err(e) => eprintln!("dealer: discover orders: {e:#}"),
    }
    for (hash, order) in bound.clone() {
        match settle(cfg, chain, kp, &order) {
            Ok(true) => {
                bound.remove(&hash);
                seen.insert(hash);
            }
            Ok(false) => {}
            Err(e) => eprintln!("dealer: settle {} failed, will retry: {e:#}", hex(&hash)),
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
        eprintln!("dealer: order {hash} is {status:?}");
        return Ok(true);
    }
    if chain.now()?.saturating_add(cfg.settle_margin_secs) >= order.terms.expiry {
        eprintln!("dealer: order {hash} passed the settle margin unsettled");
        return Ok(true);
    }
    let t = &order.terms;
    let note = chain.exact_note(kp, t.destination_mint.asset, t.destination_amount)?;
    chain.take(kp, order, note)?;
    eprintln!("dealer: settled order {hash}");
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
