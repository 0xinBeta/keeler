use anyhow::{anyhow, bail, Context};
use serde::Deserialize;

/// A USD price: `price * 10^expo`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Price {
    pub price: i64,
    pub expo: i32,
}

/// `sell_amount * sell.price * 10^e / buy.price`, floored, with
/// `e = sell.expo - buy.expo + buy_decimals - sell_decimals`.
pub fn fair_buy_amount(
    sell_amount: u64,
    sell: Price,
    buy: Price,
    sell_decimals: u8,
    buy_decimals: u8,
) -> Option<u64> {
    if sell.price <= 0 || buy.price <= 0 {
        return None;
    }
    let e = sell.expo as i64 - buy.expo as i64 + buy_decimals as i64 - sell_decimals as i64;
    let scale = 10u128.checked_pow(u32::try_from(e.unsigned_abs()).ok()?)?;
    let mut num = (sell_amount as u128).checked_mul(sell.price as u128)?;
    let mut den = buy.price as u128;
    if e >= 0 {
        num = num.checked_mul(scale)?;
    } else {
        den = den.checked_mul(scale)?;
    }
    let out = u64::try_from(num / den).ok()?;
    (out > 0).then_some(out)
}

/// True when `buy_amount` is within `band_bps` of `fair` on both sides.
pub fn within_band(buy_amount: u64, fair: u64, band_bps: u16) -> bool {
    if band_bps > 10_000 {
        return false;
    }
    let (buy, fair, band) = (buy_amount as u128, fair as u128, band_bps as u128);
    buy * 10_000 >= fair * (10_000 - band) && buy * 10_000 <= fair * (10_000 + band)
}

/// The dealer's quote: `fair` less `spread_bps`, floored.
pub fn apply_spread(fair: u64, spread_bps: u16) -> Option<u64> {
    if spread_bps > 10_000 {
        return None;
    }
    // Fits: the result is at most `fair`.
    Some((fair as u128 * (10_000 - spread_bps as u128) / 10_000) as u64)
}

#[derive(Deserialize)]
struct HermesReply {
    parsed: Vec<HermesFeed>,
}

#[derive(Deserialize)]
struct HermesFeed {
    id: String,
    price: HermesPrice,
}

#[derive(Deserialize)]
struct HermesPrice {
    price: String,
    conf: String,
    expo: i32,
    publish_time: i64,
}

/// Parses a Hermes `/v2/updates/price/latest?...&parsed=true` reply into
/// `(feed id as lowercase hex without 0x, price)`. Any stale, non-positive or
/// too-uncertain feed fails the whole call.
pub fn parse_hermes(
    json: &str,
    now: i64,
    max_age_secs: i64,
    max_conf_bps: u16,
) -> anyhow::Result<Vec<(String, Price)>> {
    let reply: HermesReply = serde_json::from_str(json).context("bad Hermes reply")?;
    reply
        .parsed
        .into_iter()
        .map(|feed| {
            let id = feed.id.trim_start_matches("0x").to_ascii_lowercase();
            check_feed(&feed.price, now, max_age_secs, max_conf_bps)
                .map(|price| (id.clone(), price))
                .with_context(|| format!("Pyth feed {id}"))
        })
        .collect()
}

fn check_feed(
    p: &HermesPrice,
    now: i64,
    max_age_secs: i64,
    max_conf_bps: u16,
) -> anyhow::Result<Price> {
    let price: i64 = p
        .price
        .parse()
        .map_err(|_| anyhow!("bad price {:?}", p.price))?;
    let conf: u64 = p
        .conf
        .parse()
        .map_err(|_| anyhow!("bad conf {:?}", p.conf))?;
    let age = now.saturating_sub(p.publish_time);
    if age > max_age_secs {
        bail!("stale: published {age} s ago, max {max_age_secs} s");
    }
    if price <= 0 {
        bail!("non-positive price {price}");
    }
    if conf as u128 * 10_000 > price as u128 * max_conf_bps as u128 {
        bail!("confidence {conf} wider than {max_conf_bps} bps of {price}");
    }
    Ok(Price {
        price,
        expo: p.expo,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOL: Price = Price {
        price: 15_000_000_000,
        expo: -8,
    }; // $150
    const USDC: Price = Price {
        price: 100_000_000,
        expo: -8,
    }; // $1

    #[test]
    fn fair_negative_e_sol_to_usdc() {
        // e = -8 + 8 + 6 - 9 = -3: 2 SOL -> 300 USDC
        assert_eq!(
            fair_buy_amount(2_000_000_000, SOL, USDC, 9, 6),
            Some(300_000_000)
        );
    }

    #[test]
    fn fair_positive_e_usdc_to_sol() {
        // e = -8 + 8 + 9 - 6 = 3: 300 USDC -> 2 SOL
        assert_eq!(
            fair_buy_amount(300_000_000, USDC, SOL, 6, 9),
            Some(2_000_000_000)
        );
    }

    #[test]
    fn fair_mixed_expo() {
        // USDC quoted with expo -6; e = -8 + 6 + 6 - 9 = -5
        let usdc6 = Price {
            price: 1_000_000,
            expo: -6,
        };
        assert_eq!(
            fair_buy_amount(2_000_000_000, SOL, usdc6, 9, 6),
            Some(300_000_000)
        );
        // e = -6 + 8 + 9 - 6 = 5
        assert_eq!(
            fair_buy_amount(300_000_000, usdc6, SOL, 6, 9),
            Some(2_000_000_000)
        );
    }

    #[test]
    fn fair_floors_and_rejects_zero() {
        let one = Price { price: 1, expo: 0 };
        let three = Price { price: 3, expo: 0 };
        assert_eq!(fair_buy_amount(10, one, three, 0, 0), Some(3));
        assert_eq!(fair_buy_amount(2, one, three, 0, 0), None);
        assert_eq!(fair_buy_amount(0, one, one, 0, 0), None);
    }

    #[test]
    fn fair_overflow() {
        let big = Price {
            price: i64::MAX,
            expo: 0,
        };
        let one = Price { price: 1, expo: 0 };
        assert_eq!(fair_buy_amount(u64::MAX, big, one, 0, 30), None);
        // fits u128 but not u64
        assert_eq!(
            fair_buy_amount(u64::MAX, Price { price: 2, expo: 0 }, one, 0, 0),
            None
        );
        // 10^e itself overflows, both signs
        assert_eq!(
            fair_buy_amount(
                1,
                Price {
                    price: 1,
                    expo: i32::MAX
                },
                one,
                0,
                0
            ),
            None
        );
        assert_eq!(
            fair_buy_amount(
                1,
                Price {
                    price: 1,
                    expo: i32::MIN
                },
                one,
                0,
                0
            ),
            None
        );
    }

    #[test]
    fn fair_zero_and_negative_price() {
        for bad in [0, -1] {
            let p = Price {
                price: bad,
                expo: 0,
            };
            assert_eq!(fair_buy_amount(100, p, USDC, 6, 6), None);
            assert_eq!(fair_buy_amount(100, USDC, p, 6, 6), None);
        }
    }

    #[test]
    fn band_edges() {
        assert!(within_band(9_900, 10_000, 100));
        assert!(!within_band(9_899, 10_000, 100));
        assert!(within_band(10_100, 10_000, 100));
        assert!(!within_band(10_101, 10_000, 100));
        assert!(within_band(10_000, 10_000, 0));
        assert!(!within_band(10_001, 10_000, 0));
    }

    #[test]
    fn band_does_not_floor_threshold() {
        assert!(!within_band(2, 3, 100));
        assert!(within_band(3, 3, 100));
    }

    #[test]
    fn bps_over_10000() {
        assert!(within_band(0, 10, 10_000));
        assert!(!within_band(10, 10, 10_001));
        assert_eq!(apply_spread(10, 10_001), None);
    }

    #[test]
    fn spread() {
        assert_eq!(apply_spread(10_000, 30), Some(9_970));
        assert_eq!(apply_spread(999, 10), Some(998)); // 998.001 floored
        assert_eq!(apply_spread(u64::MAX, 0), Some(u64::MAX));
        assert_eq!(apply_spread(10, 10_000), Some(0));
    }

    // Shape of a real Hermes `parsed=true` reply (binary data shortened).
    const HERMES: &str = r#"{
      "binary": {"encoding": "hex", "data": ["504e41550100000003b801000000040d00"]},
      "parsed": [
        {
          "id": "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d",
          "price": {"price": "14237512345", "conf": "7012345", "expo": -8, "publish_time": 1763900000},
          "ema_price": {"price": "14250011223", "conf": "7543210", "expo": -8, "publish_time": 1763900000},
          "metadata": {"slot": 230000000, "proof_available_time": 1763900001, "prev_publish_time": 1763899999}
        },
        {
          "id": "eaa020c61cc479712813461ce153894a96a6c00b21ed0cfc2798d1f9a9e9c94a",
          "price": {"price": "99985000", "conf": "71213", "expo": -8, "publish_time": 1763899998},
          "ema_price": {"price": "99987500", "conf": "75000", "expo": -8, "publish_time": 1763899998},
          "metadata": {"slot": 230000000, "proof_available_time": 1763900001, "prev_publish_time": 1763899997}
        }
      ]
    }"#;
    const SOL_ID: &str = "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";
    const USDC_ID: &str = "eaa020c61cc479712813461ce153894a96a6c00b21ed0cfc2798d1f9a9e9c94a";

    #[test]
    fn hermes_good() {
        let got = parse_hermes(HERMES, 1_763_900_010, 60, 100).unwrap();
        assert_eq!(
            got,
            vec![
                (
                    SOL_ID.to_string(),
                    Price {
                        price: 14_237_512_345,
                        expo: -8
                    }
                ),
                (
                    USDC_ID.to_string(),
                    Price {
                        price: 99_985_000,
                        expo: -8
                    }
                ),
            ]
        );
    }

    #[test]
    fn hermes_stale() {
        // USDC published at ...998: 60 s old is fine, 61 s is not
        assert!(parse_hermes(HERMES, 1_763_900_058, 60, 100).is_ok());
        let err = parse_hermes(HERMES, 1_763_900_059, 60, 100).unwrap_err();
        assert!(format!("{err:#}").contains(USDC_ID), "{err:#}");
        assert!(format!("{err:#}").contains("stale"), "{err:#}");
    }

    #[test]
    fn hermes_wide_confidence() {
        // SOL conf is ~4.93 bps of price, USDC ~7.12 bps
        let err = parse_hermes(HERMES, 1_763_900_000, 60, 5).unwrap_err();
        assert!(format!("{err:#}").contains(USDC_ID), "{err:#}");
        assert!(parse_hermes(HERMES, 1_763_900_000, 60, 8).is_ok());
    }

    #[test]
    fn hermes_non_positive_price() {
        let json = HERMES.replace("\"99985000\"", "\"0\"");
        let err = parse_hermes(&json, 1_763_900_000, 60, 100).unwrap_err();
        assert!(format!("{err:#}").contains("non-positive"), "{err:#}");
        let json = HERMES.replace("\"99985000\"", "\"-5\"");
        assert!(parse_hermes(&json, 1_763_900_000, 60, 100).is_err());
    }

    #[test]
    fn hermes_missing_and_extra_feed() {
        // The caller matches ids: a feed missing from the reply is simply absent.
        let feed = r#"{"id":"0xABCD","price":{"price":"5","conf":"0","expo":0,"publish_time":10}}"#;
        let got = parse_hermes(&format!(r#"{{"parsed":[{feed}]}}"#), 10, 60, 100).unwrap();
        assert_eq!(got, vec![("abcd".to_string(), Price { price: 5, expo: 0 })]);
        // An extra (here stale) feed is returned too, and checked like the rest.
        let three = HERMES.replacen("\"parsed\": [", &format!("\"parsed\": [{feed},"), 1);
        let err = parse_hermes(&three, 1_763_900_000, 60, 100).unwrap_err();
        assert!(format!("{err:#}").contains("abcd"), "{err:#}");
        // No parsed section at all.
        assert!(parse_hermes(r#"{"binary":{"encoding":"hex","data":[]}}"#, 10, 60, 100).is_err());
        assert!(parse_hermes("garbage", 10, 60, 100).is_err());
    }
}
