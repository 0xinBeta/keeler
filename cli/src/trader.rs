//! The trader: ask dealers, lock the best in-band quote, wait for the
//! settlement on the order's nullifier, refund after expiry.
use std::{net::SocketAddr, thread::sleep, time::Duration};

use anyhow::{anyhow, bail, Result};
use keeler_core::{
    price::{fair_buy_amount, Price},
    rfq::{best_quote, quote_message, request_quotes, Quote, Rfq, QUOTE_MESSAGE_LEN},
};
use solana_address::Address;
use solana_keypair::Keypair;
use solana_signer::Signer;
use spr_keypair::{PublicKey, ShieldedKeypair};
use swap_sdk::{
    shared::identity,
    state::{OrderTerms, OrderUtxo},
};

use crate::{
    chain::{hex, shielded, Chain, Cluster, Status},
    config::{parse_address, Common, Pricer, TraderConfig},
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
    pricer: &Pricer,
    chain: &Chain,
    sell: Address,
    buy: Address,
    amount: u64,
) -> Result<u64> {
    let p = pricer.prices(chain.client.rpc(), &[sell, buy])?;
    fair_at(p[0], p[1], chain, sell, buy, amount)
}

/// The fair amount at the given prices of `sell` and `buy`.
pub fn fair_at(
    sell_price: Price,
    buy_price: Price,
    chain: &Chain,
    sell: Address,
    buy: Address,
    amount: u64,
) -> Result<u64> {
    fair_buy_amount(
        amount,
        sell_price,
        buy_price,
        chain.mint_decimals(sell)?,
        chain.mint_decimals(buy)?,
    )
    .ok_or_else(|| anyhow!("no fair amount for {amount} of {sell} in {buy}"))
}

/// The message a dealer signs for a quote of `buy_amount` until `expiry`
/// answering `rfq`, under this build's swap program ID.
pub fn quote_message_for(
    network: Cluster,
    rfq: &Rfq,
    buy_amount: u64,
    expiry: u64,
) -> Result<[u8; QUOTE_MESSAGE_LEN]> {
    let key = |s: &str| Ok::<_, anyhow::Error>(*parse_address(s)?.as_array());
    Ok(quote_message(
        network.byte(),
        swap_program::ID.as_array(),
        &key(&rfq.sell_mint)?,
        &key(&rfq.buy_mint)?,
        &key(&rfq.trader)?,
        rfq.sell_amount,
        buy_amount,
        expiry,
    ))
}

pub fn sign_quote(
    dealer: &Keypair,
    network: Cluster,
    rfq: &Rfq,
    buy_amount: u64,
    expiry: u64,
) -> Result<Quote> {
    let sig = dealer.sign_message(&quote_message_for(network, rfq, buy_amount, expiry)?);
    Ok(Quote {
        buy_amount,
        expiry,
        signature: sig.as_ref().try_into()?,
    })
}

/// Whether `dealer` signed `q` as an answer to `rfq`.
pub fn signed_by(dealer: &Address, network: Cluster, rfq: &Rfq, q: &Quote) -> bool {
    quote_message_for(network, rfq, q.buy_amount, q.expiry)
        .is_ok_and(|m| PublicKey::from_ed25519(dealer.as_array()).verify_message(&m, &q.signature))
}

/// Context for every error once a make may have been sent.
fn may_be_locked(e: anyhow::Error) -> anyhow::Error {
    e.context("the order may be locked; run `keeler refund <config>` after its expiry")
}

pub fn swap(
    cfg: &TraderConfig,
    sell_mint: Address,
    buy_mint: Address,
    sell_amount: u64,
) -> Result<Outcome> {
    let c = &cfg.common;
    c.require(&[sell_mint, buy_mint])?;
    let pricer = c.pricer()?;
    let chain = Chain::connect(&c.network, &c.mints()?)?;
    let kp = shielded(&c.keypair()?)?;
    let fair = fair_amount(&pricer, &chain, sell_mint, buy_mint, sell_amount)?;
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
    let dealer_pks = cfg
        .dealers
        .iter()
        .map(|d| parse_address(&d.pubkey))
        .collect::<Result<Vec<_>>>()?;
    let quotes: Vec<_> = request_quotes(&addrs, &rfq, RFQ_TIMEOUT)
        .into_iter()
        .zip(&dealer_pks)
        .enumerate()
        .map(|(i, (q, pk))| {
            q.filter(|q| {
                let ok = signed_by(pk, chain.cluster, &rfq, q);
                if !ok {
                    log!("trader: dropped dealer {i}'s quote: bad signature");
                }
                ok
            })
        })
        .collect();
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
    let dealer_pk = dealer_pks[best];
    let dealer = chain.resolve(dealer_pk)?;

    let terms = OrderTerms {
        destination_mint: chain.mint(buy_mint)?,
        destination_amount: quote.buy_amount,
        maker: kp.shielded_address()?,
        expiry: quote.expiry,
        taker: Some(identity(&dealer_pk)?),
    };
    let order = chain
        .lock(&kp, terms, sell_amount, source, dealer)
        .map_err(may_be_locked)?;
    log!(
        "locked {sell_amount} for {} from dealer {best}, chain expiry {}",
        quote.buy_amount,
        quote.expiry
    );
    let settled = Outcome::Settled {
        buy_amount: quote.buy_amount,
    };

    // Funds are locked now: a transient error must not end the wait.
    loop {
        match chain.status(&order).and_then(|s| Ok((s, chain.now()?))) {
            Ok((Status::Settled, _)) => return Ok(settled),
            Ok((Status::Cancelled, _)) => return Ok(Outcome::Refunded),
            Ok((Status::Open, now)) if now > quote.expiry => break,
            Ok(_) => {}
            Err(e) => log!("trader: waiting on the order: {e:#}; retrying"),
        }
        sleep(POLL);
    }
    refund_with(&chain, &kp).map_err(may_be_locked)?;
    let hash = hex(&order.hash(0)?);
    match chain.status(&order).map_err(may_be_locked)? {
        Status::Settled => Ok(settled),
        Status::Cancelled => Ok(Outcome::Refunded),
        Status::Open => Err(may_be_locked(anyhow!(
            "order {hash} is still open after refund"
        ))),
    }
}

/// What a refund pass found and did.
pub struct Refund {
    /// Orders of this trader in the configured assets.
    pub found: usize,
    /// Each order it cancelled (hex hash) with the status the nullifier
    /// check showed afterwards.
    pub acted: Vec<(String, Status)>,
}

/// Cancels every unspent order of this trader whose expiry has passed.
pub fn refund(c: &Common) -> Result<Refund> {
    let chain = Chain::connect(&c.network, &c.mints()?)?;
    refund_with(&chain, &shielded(&c.keypair()?)?)
}

fn refund_with(chain: &Chain, kp: &ShieldedKeypair) -> Result<Refund> {
    let now = chain.now()?;
    let orders = chain.maker_orders(kp, DISCOVER_WAIT)?;
    let mut acted = Vec::new();
    let mut failed = Vec::new();
    for found in &orders {
        let order = &found.order_utxo;
        match refund_one(chain, kp, order, now) {
            Ok(Some(done)) => acted.push(done),
            Ok(None) => {}
            Err(e) => {
                let hash = order.hash(0).map(|h| hex(&h)).unwrap_or_default();
                log!("refund: order {hash}: {e:#}");
                failed.push(hash);
            }
        }
    }
    if !failed.is_empty() {
        bail!(
            "refund failed for orders {}; cancelled {:?}",
            failed.join(", "),
            acted
        );
    }
    Ok(Refund {
        found: orders.len(),
        acted,
    })
}

fn refund_one(
    chain: &Chain,
    kp: &ShieldedKeypair,
    order: &OrderUtxo,
    now: u64,
) -> Result<Option<(String, Status)>> {
    if order.terms.expiry >= now || chain.status(order)? != Status::Open {
        return Ok(None);
    }
    let hash = hex(&order.hash(0)?);
    let mut last = None;
    for _ in 0..CANCEL_ATTEMPTS {
        if let Err(e) = chain.cancel(kp, order) {
            last = Some(e);
        }
        // The nullifier, not the cancel result, says what happened: a dealer
        // may have settled first.
        let status = chain.status(order)?;
        if status != Status::Open {
            return Ok(Some((hash, status)));
        }
    }
    bail!(
        "cancel failed: {:#}",
        last.unwrap_or_else(|| anyhow!("order still unspent"))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rfq() -> Rfq {
        Rfq {
            sell_mint: Address::new_from_array([2; 32]).to_string(),
            buy_mint: Address::new_from_array([3; 32]).to_string(),
            sell_amount: 400_000_000,
            trader: Address::new_from_array([4; 32]).to_string(),
        }
    }

    #[test]
    fn signed_quote_round_trips_and_tampering_fails() {
        let dealer = Keypair::new_from_array([1; 32]);
        let q = sign_quote(&dealer, Cluster::Devnet, &rfq(), 999, 1_000).unwrap();
        assert!(signed_by(&dealer.pubkey(), Cluster::Devnet, &rfq(), &q));
        assert!(!signed_by(&dealer.pubkey(), Cluster::Mainnet, &rfq(), &q), "another network");
        let other = Keypair::new_from_array([9; 32]).pubkey();
        assert!(!signed_by(&other, Cluster::Devnet, &rfq(), &q), "another dealer's key");
        let more = Quote {
            buy_amount: 1_000,
            ..q
        };
        assert!(!signed_by(&dealer.pubkey(), Cluster::Devnet, &rfq(), &more), "buy amount");
        let later = Quote { expiry: 1_001, ..q };
        assert!(!signed_by(&dealer.pubkey(), Cluster::Devnet, &rfq(), &later), "expiry");
        let bigger = Rfq {
            sell_amount: 1,
            ..rfq()
        };
        assert!(!signed_by(&dealer.pubkey(), Cluster::Devnet, &bigger, &q), "request");
        let mut bad = q;
        bad.signature[0] ^= 1;
        assert!(!signed_by(&dealer.pubkey(), Cluster::Devnet, &rfq(), &bad), "signature bit");
    }
}
