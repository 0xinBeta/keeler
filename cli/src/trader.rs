//! The trader: ask dealers, lock the best in-band quote, wait for the
//! settlement on the order's nullifier, refund after expiry.
use std::{net::SocketAddr, thread::sleep, time::Duration};

use anyhow::{anyhow, bail, Result};
use keeler_core::{
    price::fair_buy_amount,
    rfq::{best_quote, request_quotes, Rfq},
};
use solana_address::Address;
use solana_signer::Signer;
use spr_keypair::ShieldedKeypair;
use swap_sdk::{shared::identity, state::OrderTerms};

use crate::{
    chain::{hex, shielded, Chain, Status},
    config::{parse_address, Common, TraderConfig},
};

const RFQ_TIMEOUT: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_secs(1);
/// How long `refund` waits for the indexer to show any order at all.
const DISCOVER_WAIT: Duration = Duration::from_secs(5);
const CANCEL_ATTEMPTS: usize = 3;

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Settled { buy_amount: u64 },
    Refunded,
}

/// The fair amount of `buy_mint` for `sell_amount` of `sell_mint` at the
/// configured reference prices.
pub fn fair_amount(
    c: &Common,
    chain: &Chain,
    sell: Address,
    buy: Address,
    amount: u64,
) -> Result<u64> {
    let prices = c.prices()?;
    let price = |m: Address| {
        prices
            .get(&m)
            .copied()
            .ok_or_else(|| anyhow!("no price for {m}"))
    };
    fair_buy_amount(
        amount,
        price(sell)?,
        price(buy)?,
        chain.mint_decimals(sell)?,
        chain.mint_decimals(buy)?,
    )
    .ok_or_else(|| anyhow!("no fair amount for {amount} of {sell} in {buy}"))
}

pub fn swap(
    cfg: &TraderConfig,
    sell_mint: Address,
    buy_mint: Address,
    sell_amount: u64,
) -> Result<Outcome> {
    let c = &cfg.common;
    let chain = Chain::connect(&c.rpc_url, &c.indexer_url, &c.mints()?)?;
    let kp = shielded(&c.keypair()?)?;
    let fair = fair_amount(c, &chain, sell_mint, buy_mint, sell_amount)?;
    // `make` spends one source note, so one note must cover the whole amount.
    let source = chain
        .notes(&kp, sell_mint)?
        .into_iter()
        .filter(|u| u.utxo.amount >= sell_amount)
        .min_by_key(|u| u.utxo.amount)
        .ok_or_else(|| anyhow!("no single note of {sell_mint} covers {sell_amount}"))?;
    chain.register(&kp)?;

    let rfq = Rfq {
        sell_mint: sell_mint.to_string(),
        buy_mint: buy_mint.to_string(),
        sell_amount,
        trader: kp.pubkey().to_string(),
    };
    let addrs: Vec<SocketAddr> = cfg.dealers.iter().map(|d| d.address).collect();
    let quotes = request_quotes(&addrs, &rfq, RFQ_TIMEOUT);
    let now = chain.now()?;
    let best = best_quote(
        &quotes,
        fair,
        cfg.band_bps,
        now,
        cfg.min_ttl_secs,
        cfg.max_ttl_secs,
    )
    .ok_or_else(|| anyhow!("no acceptable quote (fair {fair}, {} asked)", addrs.len()))?;
    let quote = quotes[best].ok_or_else(|| anyhow!("best quote missing"))?;
    let dealer_pk = parse_address(&cfg.dealers[best].pubkey)?;
    let dealer = chain.resolve(dealer_pk)?;

    let terms = OrderTerms {
        destination_mint: chain.mint(buy_mint)?,
        destination_amount: quote.buy_amount,
        maker: kp.shielded_address()?,
        expiry: quote.expiry,
        taker: Some(identity(&dealer_pk)?),
    };
    let order = chain.lock(&kp, terms, sell_amount, source, dealer)?;
    eprintln!(
        "locked {sell_amount} for {} from dealer {best}, chain expiry {}",
        quote.buy_amount, quote.expiry
    );

    loop {
        match chain.status(&order)? {
            Status::Settled => {
                return Ok(Outcome::Settled {
                    buy_amount: quote.buy_amount,
                })
            }
            Status::Cancelled => return Ok(Outcome::Refunded),
            Status::Open if chain.now()? > quote.expiry => break,
            Status::Open => sleep(POLL),
        }
    }
    refund_with(&chain, &kp)?;
    match chain.status(&order)? {
        Status::Settled => Ok(Outcome::Settled {
            buy_amount: quote.buy_amount,
        }),
        Status::Cancelled => Ok(Outcome::Refunded),
        Status::Open => bail!("order {} is still open after refund", hex(&order.hash(0)?)),
    }
}

/// Cancels every unspent order of this trader whose expiry has passed.
/// Returns each order it acted on (hex hash) with the status the nullifier
/// check showed afterwards; an empty list means nothing was due.
pub fn refund(c: &Common) -> Result<Vec<(String, Status)>> {
    let chain = Chain::connect(&c.rpc_url, &c.indexer_url, &c.mints()?)?;
    refund_with(&chain, &shielded(&c.keypair()?)?)
}

fn refund_with(chain: &Chain, kp: &ShieldedKeypair) -> Result<Vec<(String, Status)>> {
    let now = chain.now()?;
    let mut acted = Vec::new();
    for found in chain.maker_orders(kp, DISCOVER_WAIT)? {
        let order = found.order_utxo;
        if order.terms.expiry >= now || chain.status(&order)? != Status::Open {
            continue;
        }
        let hash = hex(&order.hash(0)?);
        let mut last = None;
        let mut status = Status::Open;
        for _ in 0..CANCEL_ATTEMPTS {
            if let Err(e) = chain.cancel(kp, &order) {
                last = Some(e);
            }
            // The nullifier, not the cancel result, says what happened: a
            // dealer may have settled first.
            status = chain.status(&order)?;
            if status != Status::Open {
                break;
            }
        }
        if status == Status::Open {
            bail!(
                "cancel of order {hash} failed: {:#}",
                last.unwrap_or_else(|| anyhow!("order still unspent"))
            );
        }
        acted.push((hash, status));
    }
    Ok(acted)
}
