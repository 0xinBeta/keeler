use anyhow::{bail, ensure};

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

/// Pyth's receiver program, `rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ`,
/// which owns the price feed accounts.
pub const PYTH_RECEIVER: [u8; 32] = [
    12, 183, 250, 187, 82, 247, 166, 72, 187, 91, 49, 125, 154, 1, 139, 144, 87, 203, 2, 71, 116,
    250, 254, 1, 230, 196, 223, 152, 204, 56, 88, 129,
];
/// Anchor's discriminator of `PriceUpdateV2`: sha256("account:PriceUpdateV2")[..8].
const PRICE_UPDATE_V2: [u8; 8] = [34, 241, 35, 99, 157, 126, 244, 205];
/// Verification level `Full`; `Partial` (0) is rejected.
const FULL: u8 = 1;

/// Reads a Pyth `PriceUpdateV2` account (owner and data) for `feed_id`. Any
/// wrong owner or type, partial verification, other feed, age beyond
/// `max_age_secs` either way from `now` (chain time), non-positive price or
/// confidence wider than `max_conf_bps` is an error.
pub fn parse_pyth_account(
    owner: &[u8; 32],
    data: &[u8],
    feed_id: &[u8; 32],
    now: i64,
    max_age_secs: i64,
    max_conf_bps: u16,
) -> anyhow::Result<Price> {
    ensure!(*owner == PYTH_RECEIVER, "not owned by the Pyth receiver");
    // Little-endian: discriminator, write authority, verification level, then
    // with Full: feed id, price, conf, exponent, publish time.
    ensure!(data.len() >= 101, "too short for a price update");
    ensure!(data[..8] == PRICE_UPDATE_V2, "not a price update account");
    ensure!(
        data[40] == FULL,
        "verification level {} is not Full",
        data[40]
    );
    ensure!(data[41..73] == feed_id[..], "another feed's account");
    let int = |at: usize| <[u8; 8]>::try_from(&data[at..at + 8]).expect("8 bytes");
    let price = i64::from_le_bytes(int(73));
    let conf = u64::from_le_bytes(int(81));
    let expo = i32::from_le_bytes(data[89..93].try_into().expect("4 bytes"));
    let published = i64::from_le_bytes(int(93));
    let age = now.saturating_sub(published);
    if age > max_age_secs {
        bail!("stale: published {age} s ago, max {max_age_secs} s");
    }
    if -age > max_age_secs {
        bail!("published {} s in the future", -age);
    }
    if price <= 0 {
        bail!("non-positive price {price}");
    }
    if conf as u128 * 10_000 > price as u128 * max_conf_bps as u128 {
        bail!("confidence {conf} wider than {max_conf_bps} bps of {price}");
    }
    Ok(Price { price, expo })
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

    const FEED: [u8; 32] = [0xef; 32];

    /// A Full `PriceUpdateV2` account of 134 bytes, as on mainnet.
    fn account(price: i64, conf: u64, published: i64) -> Vec<u8> {
        let mut d = PRICE_UPDATE_V2.to_vec();
        d.extend([7; 32]); // write authority
        d.push(FULL);
        d.extend(FEED);
        d.extend(price.to_le_bytes());
        d.extend(conf.to_le_bytes());
        d.extend((-8i32).to_le_bytes());
        d.extend(published.to_le_bytes());
        d.extend([0; 32]); // previous publish time, EMA price and conf, posted slot
        d.push(0); // spare byte the account is sized with
        d
    }

    const NOW: i64 = 1_763_900_000;

    fn parse(data: &[u8], max_conf_bps: u16) -> anyhow::Result<Price> {
        parse_pyth_account(&PYTH_RECEIVER, data, &FEED, NOW, 60, max_conf_bps)
    }

    #[test]
    fn pyth_account_good() {
        let data = account(15_000_000_000, 1_000_000, NOW - 30);
        assert_eq!(data.len(), 134);
        assert_eq!(parse(&data, 100).unwrap(), SOL);
    }

    #[test]
    fn pyth_account_rejects_wrong_owner_level_type_and_feed() {
        let data = account(15_000_000_000, 0, NOW);
        assert!(parse_pyth_account(&[0; 32], &data, &FEED, NOW, 60, 100).is_err());
        let mut partial = data.clone();
        partial[40] = 0;
        assert!(parse(&partial, 100)
            .unwrap_err()
            .to_string()
            .contains("not Full"));
        let mut other = data.clone();
        other[0] ^= 1;
        assert!(parse(&other, 100).is_err(), "discriminator");
        assert!(parse_pyth_account(&PYTH_RECEIVER, &data, &[1; 32], NOW, 60, 100).is_err());
        assert!(parse(&data[..100], 100).is_err(), "short");
    }

    #[test]
    fn pyth_account_age_both_ways() {
        assert!(parse(&account(1, 0, NOW - 60), 100).is_ok());
        assert!(parse(&account(1, 0, NOW - 61), 100)
            .unwrap_err()
            .to_string()
            .contains("stale"));
        assert!(parse(&account(1, 0, NOW + 60), 100).is_ok());
        assert!(parse(&account(1, 0, NOW + 61), 100)
            .unwrap_err()
            .to_string()
            .contains("future"));
    }

    #[test]
    fn pyth_account_confidence_and_price() {
        // conf 8 bps of price.
        let data = account(15_000_000_000, 12_000_000, NOW);
        assert!(parse(&data, 8).is_ok());
        assert!(parse(&data, 7)
            .unwrap_err()
            .to_string()
            .contains("confidence"));
        assert!(parse(&account(0, 0, NOW), 100).is_err());
        assert!(parse(&account(-1, 0, NOW), 100).is_err());
    }
}
