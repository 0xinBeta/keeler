use super::*;
use serde_json::{json, Value};

const T0: u64 = 1_763_900_000;
const C: Curve = TUSD_POOL.curve;
const V: Vault = TSOL_TUSD_VAULT;
/// $150 per SOL: tUSD base units (6 decimals) per lamport (9 decimals) × 1e12.
const SOL_150: u64 = 150_000_000_000;

fn reserve(supply_shares: u64, debt_shares: u64, sp: u64, bp: u64, rate: u32) -> Reserve {
    Reserve {
        supply_shares,
        debt_shares,
        supply_price: sp,
        borrow_price: bp,
        rate_bps: rate,
        last_update: T0,
    }
}

/// 1M tUSD supplied, 880k borrowed, at the starting prices and 5%.
const AT_KINK: Reserve = Reserve {
    supply_shares: 1_000_000_000_000,
    debt_shares: 880_000_000_000,
    supply_price: PRICE_SCALE,
    borrow_price: PRICE_SCALE,
    rate_bps: 500,
    last_update: T0,
};

// ---------------------------------------------------------------- hand checks

#[test]
fn one_year_at_the_kink() {
    let r = accrue(&AT_KINK, T0 + YEAR, 1000).unwrap();
    // 5%, and 5% × 90% × 88% = 3.96% for suppliers.
    assert_eq!(r.borrow_price, 1_050_000_000_000);
    assert_eq!(r.supply_price, 1_039_600_000_000);
    assert_eq!(r.rate_bps, 500, "accrue keeps the stored rate");
    assert_eq!(r.last_update, T0 + YEAR);
    assert_eq!(accrue(&AT_KINK, T0, 1000).unwrap(), AT_KINK);
    assert_eq!(accrue(&AT_KINK, T0 - 1, 1000), Err(TimeBackwards));
    assert_eq!(accrue(&AT_KINK, T0 + 1, 10_001), Err(InvalidInput));
}

#[test]
fn curve_points() {
    let rate = |u| curve_rate(&C, u).unwrap();
    assert_eq!(
        [0, 1, 4400, 8799, 8800, 8801, 9050, 9300, 9301, 9650, 10_000].map(rate),
        [0, 0, 250, 499, 500, 500, 600, 700, 701, 1100, 1500]
    );
    assert_eq!(curve_rate(&C, 10_001), Err(InvalidInput));
    let bad = [kink(0, 10), kink(10_000, 5)];
    assert_eq!(curve_rate(&Curve { kinks: &bad }, 0), Err(InvalidInput));
}

#[test]
fn shares_round_for_the_pool() {
    let sp = 1_500_000_000_000; // 1.5
    assert_eq!(shares_for_supply(10, sp), Ok(6));
    assert_eq!(shares_to_burn(10, sp), Ok(7));
    assert_eq!(assets_of(7, sp), Ok(10));
    assert_eq!(shares_for_supply(1, sp), Err(ZeroShares));
    let bp = 1_500_000_000_000;
    assert_eq!(debt_of(7, bp), Ok(11));
    assert_eq!(borrow_shares(10, bp), Ok(7));
    assert_eq!(
        repay_shares(7, 11, bp),
        Ok(Repaid {
            shares: 0,
            change: 0
        })
    );
    assert_eq!(
        repay_shares(7, 15, bp),
        Ok(Repaid {
            shares: 0,
            change: 4
        })
    );
    assert_eq!(
        repay_shares(7, 10, bp),
        Ok(Repaid {
            shares: 1,
            change: 0
        })
    );
}

#[test]
fn a_partial_repay_must_remove_a_share() {
    let bp = PRICE_SCALE + 1;
    let r = reserve(10_000_000, 1_000_000, PRICE_SCALE, bp, 0);
    assert_eq!(repay(&r, 1_000_000, 1, &C), Err(ZeroShares));
    assert_eq!(repay(&r, 1_000_000, 0, &C), Err(ZeroShares));
    let (_, done) = repay(&r, 1_000_000, 2, &C).unwrap();
    assert_eq!(done.shares, 999_999);
    let (_, done) = repay(&r, 0, 1, &C).unwrap();
    assert_eq!(
        done,
        Repaid {
            shares: 0,
            change: 1
        }
    );
}

#[test]
fn borrow_cap_and_liquidity() {
    let r = reserve(1_000_000, 0, PRICE_SCALE, PRICE_SCALE, 0);
    let (after, shares) = borrow(&r, 1_000_000, 950_000, &C).unwrap();
    assert_eq!(shares, 950_000);
    assert_eq!(after.rate_bps, curve_rate(&C, 9500).unwrap());
    assert_eq!(borrow(&r, 1_000_000, 950_001, &C), Err(UtilisationCap));
    assert_eq!(borrow(&r, 10, 11, &C), Err(InsufficientLiquidity));
}

#[test]
fn sol_liquidation_back_to_the_threshold() {
    // 10 SOL at $150 = 1500 tUSD against 1300 tUSD: 86.7%.
    let l = liquidate(1_300_000_000, PRICE_SCALE, 10_000_000_000, SOL_150, &V).unwrap();
    // (1300 × 1e4 − 8500 × 1500) × 1e4 / (1e8 − 8500 × 10100) = 176.678... tUSD
    assert_eq!(l.repay, 176_678_442);
    let left = collateral_value(l.collateral, SOL_150).unwrap();
    let debt = debt_of(l.debt_shares, PRICE_SCALE).unwrap();
    assert_eq!(debt, 1_300_000_000 - l.repay);
    assert!(debt as u128 * B >= 8500 * left as u128);
    assert!(!is_liquidatable(debt - 1, left, &V));
}

#[test]
fn sol_liquidation_above_the_max_limit() {
    // 1400 against 1500: 93.3%, the whole debt goes, the rest stays.
    let l = liquidate(1_400_000_000, PRICE_SCALE, 10_000_000_000, SOL_150, &V).unwrap();
    assert_eq!((l.repay, l.debt_shares), (1_400_000_000, 0));
    assert_eq!(l.payout, 9_426_666_666);
    assert_eq!(l.collateral, 573_333_334);
    // 1600 against 1500: bad debt, all collateral that fits is sold.
    let l = liquidate(1_600_000_000, PRICE_SCALE, 10_000_000_000, SOL_150, &V).unwrap();
    assert_eq!(l.repay, 1_485_148_515);
    assert_eq!((l.payout, l.collateral), (10_000_000_000, 0));
    assert_eq!(
        debt_of(l.debt_shares, PRICE_SCALE),
        Ok(1_600_000_000 - 1_485_148_515)
    );
}

#[test]
fn display_helpers() {
    assert_eq!(health_bps(1300, 1500), Some(8667));
    assert_eq!(health_bps(0, 0), Some(0));
    assert_eq!(health_bps(1, 0), None);
    // 1300 tUSD on 10 SOL at lt 85%: liquidatable at or below $152.94.
    let lp = liquidation_price(1_300_000_000, 10_000_000_000, 8500).unwrap() as u64;
    assert_eq!(lp, 152_941_176_499);
    let at = |p| {
        is_liquidatable(
            1_300_000_000,
            collateral_value(10_000_000_000, p).unwrap(),
            &V,
        )
    };
    assert!(at(lp) && !at(lp + 1));
    assert_eq!(
        apr_bps(600, 9050, 1000),
        Ok(Apr {
            borrow_bps: 600,
            supply_bps: 488
        })
    );
}

// ------------------------------------------------------------------ vectors

const VECTORS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../web/tests/fixtures/lend-vectors.json"
);

fn s(x: impl ToString) -> Value {
    Value::String(x.to_string())
}

fn n<T: std::str::FromStr>(v: &Value, k: &str) -> T {
    v[k].as_str()
        .unwrap_or_else(|| panic!("{k} is not a string"))
        .parse()
        .unwrap_or_else(|_| panic!("{k} does not parse"))
}

fn r_json(r: &Reserve) -> Value {
    json!({
        "supply_shares": s(r.supply_shares),
        "debt_shares": s(r.debt_shares),
        "supply_price": s(r.supply_price),
        "borrow_price": s(r.borrow_price),
        "rate_bps": s(r.rate_bps),
        "last_update": s(r.last_update),
    })
}

fn r_of(v: &Value) -> Reserve {
    Reserve {
        supply_shares: n(v, "supply_shares"),
        debt_shares: n(v, "debt_shares"),
        supply_price: n(v, "supply_price"),
        borrow_price: n(v, "borrow_price"),
        rate_bps: n(v, "rate_bps"),
        last_update: n(v, "last_update"),
    }
}

fn curve_json(c: &Curve) -> Value {
    c.kinks
        .iter()
        .map(|k| json!([s(k.util_bps), s(k.rate_bps)]))
        .collect()
}

fn kinks_of(v: &Value) -> Vec<Kink> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|k| {
            kink(
                k[0].as_str().unwrap().parse().unwrap(),
                k[1].as_str().unwrap().parse().unwrap(),
            )
        })
        .collect()
}

fn vault_json(v: &Vault) -> Value {
    json!({
        "cf_bps": s(v.cf_bps),
        "lt_bps": s(v.lt_bps),
        "lml_bps": s(v.lml_bps),
        "penalty_bps": s(v.penalty_bps),
    })
}

fn vault_of(v: &Value) -> Vault {
    Vault {
        cf_bps: n(v, "cf_bps"),
        lt_bps: n(v, "lt_bps"),
        lml_bps: n(v, "lml_bps"),
        penalty_bps: n(v, "penalty_bps"),
    }
}

/// Runs one vector: `{"out": ...}` or `{"error": code}`.
fn run(f: &str, i: &Value) -> Value {
    let kinks = i.get("curve").map(kinks_of).unwrap_or_default();
    let curve = Curve { kinks: &kinks };
    let res = || r_of(&i["reserve"]);
    let vault = || vault_of(&i["vault"]);
    let opt = |x: Option<u128>| x.map(s).unwrap_or(Value::Null);
    let out: Result<Value> = match f {
        "total_supply" => total_supply(&res()).map(s),
        "total_debt" => total_debt(&res()).map(s),
        "utilisation_bps" => utilisation_bps(&res()).map(s),
        "accrue" => accrue(&res(), n(i, "now"), n(i, "fee_bps")).map(|r| r_json(&r)),
        "curve_rate" => curve_rate(&curve, n(i, "util_bps")).map(s),
        "refresh_rate" => refresh_rate(&res(), &curve).map(|r| r_json(&r)),
        "shares_for_supply" => shares_for_supply(n(i, "amount"), n(i, "supply_price")).map(s),
        "shares_to_burn" => shares_to_burn(n(i, "amount"), n(i, "supply_price")).map(s),
        "assets_of" => assets_of(n(i, "shares"), n(i, "supply_price")).map(s),
        "debt_of" => debt_of(n(i, "shares"), n(i, "borrow_price")).map(s),
        "borrow_shares" => borrow_shares(n(i, "amount"), n(i, "borrow_price")).map(s),
        "repay_shares" => repay_shares(n(i, "shares"), n(i, "amount"), n(i, "borrow_price"))
            .map(|x| json!({"shares": s(x.shares), "change": s(x.change)})),
        "supply" => supply(&res(), n(i, "amount"), &curve)
            .map(|(r, m)| json!({"reserve": r_json(&r), "shares": s(m)})),
        "redeem" => redeem(&res(), n(i, "idle"), n(i, "held"), n(i, "amount"), &curve)
            .map(|(r, b)| json!({"reserve": r_json(&r), "burned": s(b)})),
        "borrow" => borrow(&res(), n(i, "idle"), n(i, "amount"), &curve)
            .map(|(r, a)| json!({"reserve": r_json(&r), "shares": s(a)})),
        "repay" => repay(&res(), n(i, "position_shares"), n(i, "amount"), &curve).map(
            |(r, x)| json!({"reserve": r_json(&r), "shares": s(x.shares), "change": s(x.change)}),
        ),
        "collateral_value" => collateral_value(n(i, "collateral"), n(i, "price")).map(s),
        "can_draw" => Ok(can_draw(n(i, "debt"), n(i, "value"), &vault()).into()),
        "is_liquidatable" => Ok(is_liquidatable(n(i, "debt"), n(i, "value"), &vault()).into()),
        "above_max_limit" => Ok(above_max_limit(n(i, "debt"), n(i, "value"), &vault()).into()),
        "liquidate" => liquidate(
            n(i, "debt_shares"),
            n(i, "borrow_price"),
            n(i, "collateral"),
            n(i, "price"),
            &vault(),
        )
        .map(|l| {
            json!({
                "repay": s(l.repay),
                "payout": s(l.payout),
                "debt_shares": s(l.debt_shares),
                "collateral": s(l.collateral),
            })
        }),
        "health_bps" => Ok(opt(health_bps(n(i, "debt"), n(i, "value")))),
        "liquidation_price" => Ok(opt(liquidation_price(
            n(i, "debt"),
            n(i, "collateral"),
            n(i, "lt_bps"),
        ))),
        "apr_bps" => apr_bps(n(i, "rate_bps"), n(i, "util_bps"), n(i, "fee_bps"))
            .map(|a| json!({"borrow_bps": s(a.borrow_bps), "supply_bps": s(a.supply_bps)})),
        _ => panic!("unknown function {f}"),
    };
    match out {
        Ok(v) => json!({ "out": v }),
        Err(e) => json!({ "error": e.code() }),
    }
}

/// `(name, function, inputs)` for every case; outputs come from `run`.
fn cases() -> Vec<(&'static str, &'static str, Value)> {
    let max = u64::MAX;
    let r = |r: Reserve| r_json(&r);
    let tusd = curve_json(&C);
    let tsol = vault_json(&V);
    // A vault whose max limit is past 1e4 / (1e4 + p), so the normal branch meets the collateral cap.
    let steep = vault_json(&Vault {
        cf_bps: 9500,
        lt_bps: 9800,
        lml_bps: 9950,
        penalty_bps: 100,
    });
    let broken = vault_json(&Vault {
        cf_bps: 9000,
        lt_bps: 9901,
        lml_bps: 9950,
        penalty_bps: 100,
    });
    let half = 1_500_000_000_000u64;
    let empty = Reserve::new(T0, 0);
    let grown = reserve(
        1_000_000_000_000,
        880_000_000_000,
        1_039_600_000_000,
        1_050_000_000_000,
        500,
    );
    let over = reserve(100, 200, PRICE_SCALE, PRICE_SCALE, 1500);
    let tiny = reserve(1_000_000, 500_000, PRICE_SCALE, PRICE_SCALE, 1);
    let liq = |shares: u64, bp: u64, coll: u64, price: u64, vault: &Value| json!({"debt_shares": s(shares), "borrow_price": s(bp), "collateral": s(coll), "price": s(price), "vault": vault});
    vec![
        (
            "empty reserve",
            "total_supply",
            json!({"reserve": r(empty)}),
        ),
        (
            "floors",
            "total_supply",
            json!({"reserve": r(reserve(3, 0, half, PRICE_SCALE, 0))}),
        ),
        (
            "overflows u64",
            "total_supply",
            json!({"reserve": r(reserve(max, 0, 2 * PRICE_SCALE, PRICE_SCALE, 0))}),
        ),
        (
            "rounds up",
            "total_debt",
            json!({"reserve": r(reserve(0, 3, PRICE_SCALE, PRICE_SCALE + 1, 0))}),
        ),
        ("grown", "total_debt", json!({"reserve": r(grown)})),
        (
            "overflows u64",
            "total_debt",
            json!({"reserve": r(reserve(0, max, PRICE_SCALE, PRICE_SCALE + 1, 0))}),
        ),
        (
            "zero supply is 0",
            "utilisation_bps",
            json!({"reserve": r(reserve(0, 5, PRICE_SCALE, PRICE_SCALE, 0))}),
        ),
        (
            "at the kink",
            "utilisation_bps",
            json!({"reserve": r(AT_KINK)}),
        ),
        (
            "grown, floored",
            "utilisation_bps",
            json!({"reserve": r(grown)}),
        ),
        (
            "clamped at 10000",
            "utilisation_bps",
            json!({"reserve": r(over)}),
        ),
        (
            "one year at the kink",
            "accrue",
            json!({"reserve": r(AT_KINK), "now": s(T0 + YEAR), "fee_bps": "1000"}),
        ),
        (
            "one day at the kink",
            "accrue",
            json!({"reserve": r(AT_KINK), "now": s(T0 + 86_400), "fee_bps": "1000"}),
        ),
        (
            "dt 0 changes nothing",
            "accrue",
            json!({"reserve": r(AT_KINK), "now": s(T0), "fee_bps": "1000"}),
        ),
        (
            "time backwards",
            "accrue",
            json!({"reserve": r(AT_KINK), "now": s(T0 - 1), "fee_bps": "1000"}),
        ),
        (
            "fee above 10000",
            "accrue",
            json!({"reserve": r(AT_KINK), "now": s(T0 + 1), "fee_bps": "10001"}),
        ),
        (
            "fee 10000: suppliers get nothing",
            "accrue",
            json!({"reserve": r(AT_KINK), "now": s(T0 + YEAR), "fee_bps": "10000"}),
        ),
        (
            "rate 0",
            "accrue",
            json!({"reserve": r(reserve(10, 5, PRICE_SCALE, PRICE_SCALE, 0)), "now": s(T0 + YEAR), "fee_bps": "1000"}),
        ),
        (
            "one second at 1 bps: borrow rounds up, supply down",
            "accrue",
            json!({"reserve": r(tiny), "now": s(T0 + 1), "fee_bps": "1000"}),
        ),
        (
            "empty pool: supply price still",
            "accrue",
            json!({"reserve": r(Reserve { rate_bps: 700, ..empty }), "now": s(T0 + YEAR), "fee_bps": "1000"}),
        ),
        (
            "utilisation clamped for suppliers",
            "accrue",
            json!({"reserve": r(over), "now": s(T0 + YEAR), "fee_bps": "1000"}),
        ),
        (
            "u128 overflow",
            "accrue",
            json!({"reserve": r(reserve(1, 1, max, max, u32::MAX)), "now": s(max), "fee_bps": "0"}),
        ),
        (
            "price overflows u64",
            "accrue",
            json!({"reserve": r(reserve(1, 1, PRICE_SCALE, max / 2, 10_000)), "now": s(T0 + 2 * YEAR), "fee_bps": "0"}),
        ),
        ("u 0", "curve_rate", json!({"curve": tusd, "util_bps": "0"})),
        (
            "u 4400",
            "curve_rate",
            json!({"curve": tusd, "util_bps": "4400"}),
        ),
        (
            "u 8799 floors",
            "curve_rate",
            json!({"curve": tusd, "util_bps": "8799"}),
        ),
        (
            "u 8800",
            "curve_rate",
            json!({"curve": tusd, "util_bps": "8800"}),
        ),
        (
            "u 9050",
            "curve_rate",
            json!({"curve": tusd, "util_bps": "9050"}),
        ),
        (
            "u 9300",
            "curve_rate",
            json!({"curve": tusd, "util_bps": "9300"}),
        ),
        (
            "u 9301",
            "curve_rate",
            json!({"curve": tusd, "util_bps": "9301"}),
        ),
        (
            "u 9650",
            "curve_rate",
            json!({"curve": tusd, "util_bps": "9650"}),
        ),
        (
            "u 10000",
            "curve_rate",
            json!({"curve": tusd, "util_bps": "10000"}),
        ),
        (
            "u 10001",
            "curve_rate",
            json!({"curve": tusd, "util_bps": "10001"}),
        ),
        (
            "falling curve",
            "curve_rate",
            json!({"curve": [["0", "10"], ["10000", "5"]], "util_bps": "0"}),
        ),
        (
            "curve not ending at 10000",
            "curve_rate",
            json!({"curve": [["0", "0"], ["9000", "5"]], "util_bps": "0"}),
        ),
        (
            "after accrual",
            "refresh_rate",
            json!({"reserve": r(grown), "curve": tusd}),
        ),
        (
            "empty",
            "refresh_rate",
            json!({"reserve": r(empty), "curve": tusd}),
        ),
        (
            "at 1",
            "shares_for_supply",
            json!({"amount": "1000000", "supply_price": s(PRICE_SCALE)}),
        ),
        (
            "floors",
            "shares_for_supply",
            json!({"amount": "10", "supply_price": s(half)}),
        ),
        (
            "zero shares",
            "shares_for_supply",
            json!({"amount": "1", "supply_price": s(half)}),
        ),
        (
            "zero amount",
            "shares_for_supply",
            json!({"amount": "0", "supply_price": s(PRICE_SCALE)}),
        ),
        (
            "zero price",
            "shares_for_supply",
            json!({"amount": "1", "supply_price": "0"}),
        ),
        (
            "overflow",
            "shares_for_supply",
            json!({"amount": s(max), "supply_price": "1"}),
        ),
        (
            "rounds up",
            "shares_to_burn",
            json!({"amount": "10", "supply_price": s(half)}),
        ),
        (
            "exact",
            "shares_to_burn",
            json!({"amount": "15", "supply_price": s(half)}),
        ),
        (
            "zero price",
            "shares_to_burn",
            json!({"amount": "1", "supply_price": "0"}),
        ),
        (
            "floors",
            "assets_of",
            json!({"shares": "7", "supply_price": s(half)}),
        ),
        (
            "overflow",
            "assets_of",
            json!({"shares": s(max), "supply_price": s(max)}),
        ),
        (
            "rounds up",
            "debt_of",
            json!({"shares": "7", "borrow_price": s(half)}),
        ),
        (
            "zero",
            "debt_of",
            json!({"shares": "0", "borrow_price": s(half)}),
        ),
        (
            "rounds up",
            "borrow_shares",
            json!({"amount": "10", "borrow_price": s(half)}),
        ),
        (
            "zero price",
            "borrow_shares",
            json!({"amount": "10", "borrow_price": "0"}),
        ),
        (
            "exact debt",
            "repay_shares",
            json!({"shares": "7", "amount": "11", "borrow_price": s(half)}),
        ),
        (
            "overpay returns change",
            "repay_shares",
            json!({"shares": "7", "amount": "15", "borrow_price": s(half)}),
        ),
        (
            "partial floors",
            "repay_shares",
            json!({"shares": "7", "amount": "10", "borrow_price": s(half)}),
        ),
        (
            "dust removes no share",
            "repay_shares",
            json!({"shares": "7", "amount": "1", "borrow_price": s(half)}),
        ),
        (
            "no debt",
            "repay_shares",
            json!({"shares": "0", "amount": "5", "borrow_price": s(half)}),
        ),
        (
            "first supply",
            "supply",
            json!({"reserve": r(empty), "amount": "1000000", "curve": tusd}),
        ),
        (
            "into a busy pool",
            "supply",
            json!({"reserve": r(grown), "amount": "123456789", "curve": tusd}),
        ),
        (
            "zero shares",
            "supply",
            json!({"reserve": r(grown), "amount": "1", "curve": tusd}),
        ),
        (
            "partial",
            "redeem",
            json!({"reserve": r(grown), "idle": "120000000000", "held": "1000000", "amount": "1000000", "curve": tusd}),
        ),
        (
            "all of a receipt",
            "redeem",
            json!({"reserve": r(grown), "idle": "120000000000", "held": "1000000", "amount": "1039600", "curve": tusd}),
        ),
        (
            "more than held",
            "redeem",
            json!({"reserve": r(grown), "idle": "120000000000", "held": "1000000", "amount": "1039601", "curve": tusd}),
        ),
        (
            "more than idle",
            "redeem",
            json!({"reserve": r(grown), "idle": "5", "held": "1000000", "amount": "6", "curve": tusd}),
        ),
        (
            "zero",
            "redeem",
            json!({"reserve": r(grown), "idle": "5", "held": "1000000", "amount": "0", "curve": tusd}),
        ),
        (
            "normal",
            "borrow",
            json!({"reserve": r(grown), "idle": "120000000000", "amount": "1000000", "curve": tusd}),
        ),
        (
            "to exactly 95%",
            "borrow",
            json!({"reserve": r(reserve(1_000_000, 0, PRICE_SCALE, PRICE_SCALE, 0)), "idle": "1000000", "amount": "950000", "curve": tusd}),
        ),
        (
            "past 95%",
            "borrow",
            json!({"reserve": r(reserve(1_000_000, 0, PRICE_SCALE, PRICE_SCALE, 0)), "idle": "1000000", "amount": "950001", "curve": tusd}),
        ),
        (
            "more than idle",
            "borrow",
            json!({"reserve": r(reserve(1_000_000, 0, PRICE_SCALE, PRICE_SCALE, 0)), "idle": "10", "amount": "11", "curve": tusd}),
        ),
        (
            "zero",
            "borrow",
            json!({"reserve": r(grown), "idle": "10", "amount": "0", "curve": tusd}),
        ),
        (
            "partial",
            "repay",
            json!({"reserve": r(grown), "position_shares": "1000000", "amount": "500000", "curve": tusd}),
        ),
        (
            "full with change",
            "repay",
            json!({"reserve": r(grown), "position_shares": "1000000", "amount": "2000000", "curve": tusd}),
        ),
        (
            "more shares than the reserve",
            "repay",
            json!({"reserve": r(reserve(10, 5, PRICE_SCALE, PRICE_SCALE, 0)), "position_shares": "6", "amount": "1", "curve": tusd}),
        ),
        (
            "10 SOL at $150",
            "collateral_value",
            json!({"collateral": "10000000000", "price": s(SOL_150)}),
        ),
        (
            "floors",
            "collateral_value",
            json!({"collateral": "7", "price": s(SOL_150)}),
        ),
        (
            "overflow",
            "collateral_value",
            json!({"collateral": s(max), "price": s(max)}),
        ),
        (
            "at cf",
            "can_draw",
            json!({"debt": "1200000000", "value": "1500000000", "vault": tsol}),
        ),
        (
            "one over cf",
            "can_draw",
            json!({"debt": "1200000001", "value": "1500000000", "vault": tsol}),
        ),
        (
            "at lt is not",
            "is_liquidatable",
            json!({"debt": "1275000000", "value": "1500000000", "vault": tsol}),
        ),
        (
            "one over lt",
            "is_liquidatable",
            json!({"debt": "1275000001", "value": "1500000000", "vault": tsol}),
        ),
        (
            "debt with no value",
            "is_liquidatable",
            json!({"debt": "1", "value": "0", "vault": tsol}),
        ),
        (
            "at lml is not",
            "above_max_limit",
            json!({"debt": "1350000000", "value": "1500000000", "vault": tsol}),
        ),
        (
            "one over lml",
            "above_max_limit",
            json!({"debt": "1350000001", "value": "1500000000", "vault": tsol}),
        ),
        (
            "healthy",
            "liquidate",
            liq(1_275_000_000, PRICE_SCALE, 10_000_000_000, SOL_150, &tsol),
        ),
        (
            "back to the threshold",
            "liquidate",
            liq(1_300_000_000, PRICE_SCALE, 10_000_000_000, SOL_150, &tsol),
        ),
        (
            "back to the threshold, grown borrow price",
            "liquidate",
            liq(
                1_250_000_000,
                1_050_000_000_000,
                10_000_000_000,
                SOL_150,
                &tsol,
            ),
        ),
        (
            "just past the threshold",
            "liquidate",
            liq(1_275_000_100, PRICE_SCALE, 10_000_000_000, SOL_150, &tsol),
        ),
        (
            "a few units past the threshold",
            "liquidate",
            liq(1_275_000_001, PRICE_SCALE, 10_000_000_000, SOL_150, &tsol),
        ),
        (
            "so little past the threshold that x is 0",
            "liquidate",
            liq(1_275_000_006, PRICE_SCALE, 10_000_000_047, SOL_150, &tsol),
        ),
        (
            "at the max limit, partial",
            "liquidate",
            liq(1_350_000_000, PRICE_SCALE, 10_000_000_000, SOL_150, &tsol),
        ),
        (
            "above the max limit, all debt",
            "liquidate",
            liq(1_400_000_000, PRICE_SCALE, 10_000_000_000, SOL_150, &tsol),
        ),
        (
            "bad debt, collateral used up",
            "liquidate",
            liq(1_600_000_000, PRICE_SCALE, 10_000_000_000, SOL_150, &tsol),
        ),
        (
            "no collateral",
            "liquidate",
            liq(1_000, PRICE_SCALE, 0, SOL_150, &tsol),
        ),
        (
            "normal branch capped by the collateral",
            "liquidate",
            liq(992_000, PRICE_SCALE, 1_000_000, PRICE_SCALE, &steep),
        ),
        (
            "coarse collateral: price 1000 per unit",
            "liquidate",
            liq(8_700, PRICE_SCALE, 10, 1_000 * PRICE_SCALE, &tsol),
        ),
        ("zero price", "liquidate", liq(1, PRICE_SCALE, 1, 0, &tsol)),
        (
            "lt × (1e4 + p) >= 1e8",
            "liquidate",
            liq(1, PRICE_SCALE, 1, PRICE_SCALE, &broken),
        ),
        (
            "1300 on 1500",
            "health_bps",
            json!({"debt": "1300", "value": "1500"}),
        ),
        (
            "rounds up",
            "health_bps",
            json!({"debt": "1", "value": "3"}),
        ),
        ("no debt", "health_bps", json!({"debt": "0", "value": "0"})),
        (
            "debt, no value",
            "health_bps",
            json!({"debt": "1", "value": "0"}),
        ),
        (
            "1300 tUSD on 10 SOL",
            "liquidation_price",
            json!({"debt": "1300000000", "collateral": "10000000000", "lt_bps": "8500"}),
        ),
        (
            "no debt",
            "liquidation_price",
            json!({"debt": "0", "collateral": "10", "lt_bps": "8500"}),
        ),
        (
            "no collateral",
            "liquidation_price",
            json!({"debt": "10", "collateral": "0", "lt_bps": "8500"}),
        ),
        (
            "at 9050",
            "apr_bps",
            json!({"rate_bps": "600", "util_bps": "9050", "fee_bps": "1000"}),
        ),
        (
            "idle pool",
            "apr_bps",
            json!({"rate_bps": "0", "util_bps": "0", "fee_bps": "1000"}),
        ),
        (
            "util above 10000",
            "apr_bps",
            json!({"rate_bps": "600", "util_bps": "10001", "fee_bps": "1000"}),
        ),
    ]
}

fn generate() -> String {
    let cases: Vec<Value> = cases()
        .into_iter()
        .map(|(name, f, input)| {
            let mut c = json!({"name": name, "fn": f, "in": input});
            for (k, v) in run(f, &c["in"]).as_object().unwrap() {
                c[k] = v.clone();
            }
            c
        })
        .collect();
    let doc = json!({
        "about": "Keeler Lend maths vectors, generated by `KEELER_WRITE_VECTORS=1 cargo test -p keeler-core lend` from core/src/lend/tests.rs; the rules are in core/src/lend.rs. Each case calls `fn` with `in` and expects `out` or `error` (an error code). Numbers are decimal strings; booleans and null are JSON. A reserve is {supply_shares, debt_shares, supply_price, borrow_price, rate_bps, last_update}; a curve is [[util_bps, rate_bps], ...]; a vault is {cf_bps, lt_bps, lml_bps, penalty_bps}. Prices are scaled 1e12.",
        "constants": {
            "PRICE_SCALE": s(PRICE_SCALE),
            "BPS": s(BPS),
            "YEAR": s(YEAR),
            "MAX_BORROW_UTIL_BPS": s(MAX_BORROW_UTIL_BPS),
            "tusd_pool": {"curve": curve_json(&C), "fee_bps": s(TUSD_POOL.fee_bps)},
            "tsol_tusd_vault": vault_json(&V),
        },
        "cases": cases,
    });
    serde_json::to_string_pretty(&doc).unwrap() + "\n"
}

#[test]
fn vectors_file_is_current() {
    let fresh = generate();
    if std::env::var_os("KEELER_WRITE_VECTORS").is_some() {
        std::fs::write(VECTORS, &fresh).unwrap();
    }
    let file = std::fs::read_to_string(VECTORS).expect("run with KEELER_WRITE_VECTORS=1");
    assert!(
        file == fresh,
        "lend-vectors.json is stale: rerun with KEELER_WRITE_VECTORS=1"
    );
}

#[test]
fn vectors_from_the_file_pass() {
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(VECTORS).unwrap()).unwrap();
    let cases = doc["cases"].as_array().unwrap();
    assert!(cases.len() > 90);
    for c in cases {
        let got = run(c["fn"].as_str().unwrap(), &c["in"]);
        let want = match c.get("out") {
            Some(o) => json!({ "out": o }),
            None => json!({ "error": c["error"] }),
        };
        assert_eq!(got, want, "{} / {}", c["fn"], c["name"]);
    }
    // Every function and every error code is exercised.
    let fns: std::collections::BTreeSet<_> =
        cases.iter().map(|c| c["fn"].as_str().unwrap()).collect();
    assert_eq!(fns.len(), 24);
    let errs: std::collections::BTreeSet<_> =
        cases.iter().filter_map(|c| c["error"].as_str()).collect();
    assert_eq!(errs.len(), 10, "{errs:?}");
}

// --------------------------------------------------------------- properties

/// xorshift64*, deterministic.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    /// Spread over magnitudes, 1 ..= 10^digits.
    fn mag(&mut self, digits: u32) -> u64 {
        let d = self.below(digits as u64) as u32 + 1;
        self.below(10u64.pow(d)) + 1
    }
}

#[test]
fn rounding_always_favours_the_pool() {
    let mut g = Rng(7);
    for _ in 0..20_000 {
        let amount = g.mag(15);
        let price = PRICE_SCALE + g.mag(12);
        let (a, p) = (amount as u128, price as u128);
        if let Ok(m) = shares_for_supply(amount, price) {
            assert!(m as u128 * p <= a * S);
            // Supplying then withdrawing all never returns more.
            assert!(assets_of(m, price).unwrap() <= amount);
        }
        let burned = shares_to_burn(amount, price).unwrap();
        assert!(burned as u128 * p >= a * S);
        let added = borrow_shares(amount, price).unwrap();
        assert!(added as u128 * p >= a * S);
        assert!(
            debt_of(added, price).unwrap() >= amount,
            "borrowing creates at least the amount"
        );
        let shares = g.mag(15);
        let debt = debt_of(shares, price).unwrap();
        assert!(debt as u128 * S >= shares as u128 * p);
        let pay = g.below(debt + 2);
        let after = repay_shares(shares, pay, price).unwrap();
        let left = debt_of(after.shares, price).unwrap();
        assert!(
            left + pay - after.change >= debt,
            "a repay never clears more than it pays"
        );
        // Liquidation payout never exceeds the value paid in.
        let coll_price = g.mag(13);
        let x = g.mag(12) as u128;
        let pen = g.below(1000) as u128;
        assert!(payout(x, pen, coll_price as u128) * coll_price as u128 <= (x + x * pen / B) * S);
    }
}

fn random_reserve(g: &mut Rng) -> Reserve {
    let supply_shares = g.mag(14);
    Reserve {
        supply_shares,
        debt_shares: g.below(supply_shares + supply_shares / 5 + 1),
        supply_price: PRICE_SCALE + g.mag(11),
        borrow_price: PRICE_SCALE + g.mag(12),
        rate_bps: g.below(3000) as u32,
        last_update: T0,
    }
}

#[test]
fn accrual_is_monotone_and_solvent() {
    let mut g = Rng(11);
    for _ in 0..20_000 {
        let r = random_reserve(&mut g);
        let fee = g.below(10_001) as u32;
        let t1 = g.mag(8);
        let t2 = t1 + g.mag(8);
        let a = accrue(&r, T0 + t1, fee).unwrap();
        let b = accrue(&r, T0 + t2, fee).unwrap();
        assert!(r.supply_price <= a.supply_price && a.supply_price <= b.supply_price);
        assert!(r.borrow_price <= a.borrow_price && a.borrow_price <= b.borrow_price);
        let faster = accrue(
            &Reserve {
                rate_bps: r.rate_bps + 1,
                ..r
            },
            T0 + t1,
            fee,
        )
        .unwrap();
        assert!(faster.supply_price >= a.supply_price && faster.borrow_price >= a.borrow_price);
        // Suppliers are credited at most what borrowers are charged, less the fee,
        // within the totals' own rounding (one unit each side).
        let ds = total_supply(&a).unwrap() - total_supply(&r).unwrap();
        let dd = total_debt(&a).unwrap() - total_debt(&r).unwrap();
        assert!(
            (ds as u128).saturating_sub(2) * B <= dd as u128 * (B - fee as u128),
            "{r:?} {t1} {fee}"
        );
    }
}

/// Splitting an interval can credit more than one accrual over it, but only by
/// compounding and the rise in utilisation between the halves, never by rounding.
#[test]
fn split_accrual_gains_only_by_compounding() {
    let mut g = Rng(13);
    let mut compounding_seen = false;
    for _ in 0..20_000 {
        let r = random_reserve(&mut g);
        let fee = g.below(10_001) as u32;
        let t1 = g.mag(7);
        let t2 = g.mag(7);
        let one = accrue(&r, T0 + t1 + t2, fee).unwrap();
        let mid = accrue(&r, T0 + t1, fee).unwrap();
        let two = accrue(&mid, T0 + t1 + t2, fee).unwrap();
        assert!(
            two.borrow_price >= one.borrow_price,
            "borrowers never pay less for a split"
        );
        let (p0, p1, p2) = (
            r.supply_price as u128,
            mid.supply_price as u128,
            two.supply_price as u128,
        );
        let excess = p2 as i128 - one.supply_price as i128;
        let (u0, u1) = (
            utilisation_bps(&r).unwrap() as u128,
            utilisation_bps(&mid).unwrap() as u128,
        );
        let k = r.rate_bps as u128 * (B - fee as u128);
        let den = B * B * B * YEAR as u128;
        let compounding = ((p1 - p0) * (p2 - p1 + 1)).div_ceil(p1);
        let drift = (p0 * k * u1.saturating_sub(u0) * t2 as u128).div_ceil(den);
        assert!(
            excess <= (1 + compounding + drift) as i128,
            "{r:?} {t1} {t2} {fee}"
        );
        compounding_seen |= excess > 1;
    }
    assert!(
        compounding_seen,
        "the literal 'never more than one accrual' does not hold"
    );
}

// ------------------------------------------------------------- liquidation

/// f(x) >= 0 in the spec, by plain arithmetic.
fn at_or_above_threshold(d: u64, c: u64, price: u64, v: &Vault, x: u64) -> bool {
    let paid = payout(x as u128, v.penalty_bps as u128, price as u128) as u64;
    let left = collateral_value(c - paid, price).unwrap();
    (d - x) as u128 * B >= v.lt_bps as u128 * left as u128
}

fn fits(c: u64, price: u64, v: &Vault, x: u64) -> bool {
    payout(x as u128, v.penalty_bps as u128, price as u128) <= c as u128
}

#[test]
fn liquidation_size_matches_brute_force() {
    let vaults = [
        V,
        Vault {
            cf_bps: 8500,
            lt_bps: 9000,
            lml_bps: 9500,
            penalty_bps: 500,
        },
        Vault {
            cf_bps: 4000,
            lt_bps: 5000,
            lml_bps: 7000,
            penalty_bps: 1000,
        },
        Vault {
            cf_bps: 9500,
            lt_bps: 9800,
            lml_bps: 9950,
            penalty_bps: 100,
        },
        Vault {
            cf_bps: 7000,
            lt_bps: 7500,
            lml_bps: 8000,
            penalty_bps: 0,
        },
    ];
    let prices = [
        PRICE_SCALE / 7,
        SOL_150,
        PRICE_SCALE / 2 + 3,
        PRICE_SCALE,
        1_700_000_000_000,
        3 * PRICE_SCALE,
        7_300_000_000_000,
        100 * PRICE_SCALE + 13,
    ];
    let mut checked = [0u32; 3]; // normal, above max, nothing to do
    for v in &vaults {
        for &price in &prices {
            for c in 0..=40u64 {
                let value = collateral_value(c, price).unwrap();
                for d in 0..=(value * 11 / 10 + 3).min(250) {
                    let got = liquidate(d, PRICE_SCALE, c, price, v);
                    if !is_liquidatable(d, value, v) {
                        assert_eq!(got, Err(NotLiquidatable));
                        continue;
                    }
                    let x_max = (0..=d).rev().find(|&x| fits(c, price, v, x)).unwrap();
                    let want = if above_max_limit(d, value, v) {
                        checked[1] += 1;
                        x_max
                    } else {
                        checked[0] += 1;
                        (0..=x_max)
                            .rev()
                            .find(|&x| at_or_above_threshold(d, c, price, v, x))
                            .unwrap()
                    };
                    if want == 0 {
                        checked[2] += 1;
                        assert_eq!(got, Err(NothingToLiquidate), "{v:?} {price} c {c} d {d}");
                        continue;
                    }
                    let l = got.unwrap_or_else(|e| panic!("{e} {v:?} {price} c {c} d {d}"));
                    assert_eq!(l.repay, want, "{v:?} {price} c {c} d {d}");
                    assert_eq!(l.payout + l.collateral, c);
                    assert_eq!(l.debt_shares, d - want);
                }
            }
        }
    }
    assert!(checked.iter().all(|&n| n > 100), "{checked:?}");
}

#[test]
fn liquidation_invariant_at_scale() {
    let mut g = Rng(17);
    for _ in 0..5_000 {
        let c = g.mag(13);
        let price = g.mag(13);
        let Ok(value) = collateral_value(c, price) else {
            continue;
        };
        if value == 0 {
            continue;
        }
        // LTV between 85% and 95%.
        let d = (value as u128 * (8500 + g.below(1000) as u128) / B) as u64;
        let Ok(l) = liquidate(d, PRICE_SCALE, c, price, &V) else {
            continue;
        };
        let x = l.repay;
        if above_max_limit(d, value, &V) {
            assert!(x == d || !fits(c, price, &V, x + 1));
        } else {
            assert!(at_or_above_threshold(d, c, price, &V, x));
            assert!(
                x == d
                    || !fits(c, price, &V, x + 1)
                    || !at_or_above_threshold(d, c, price, &V, x + 1)
            );
        }
    }
}

#[test]
fn liquidation_price_is_the_edge() {
    let mut g = Rng(19);
    for _ in 0..20_000 {
        let (d, c) = (g.mag(12), g.mag(13));
        let lt = 1 + g.below(10_000) as u32;
        let v = Vault { lt_bps: lt, ..V };
        let p = liquidation_price(d, c, lt).unwrap();
        let value = |p: u128| {
            u64::try_from(p)
                .ok()
                .and_then(|p| collateral_value(c, p).ok())
        };
        let (Some(at), Some(above)) = (value(p), value(p + 1)) else {
            continue;
        };
        assert!(
            is_liquidatable(d, at, &v) && !is_liquidatable(d, above, &v),
            "{d} {c} {lt}"
        );
    }
}

#[test]
fn a_liquidation_always_removes_a_debt_share() {
    // Past 1.0 a share is worth more than one base unit, so a tiny repay can
    // remove none; the circuit refuses that, so `liquidate` must too.
    let bp = 3 * PRICE_SCALE + 1;
    let (mut ok, mut dust) = (0, 0);
    for shares in 1..=60u64 {
        for c in 1..=40u64 {
            for price in [PRICE_SCALE, SOL_150 / 100, 7 * PRICE_SCALE] {
                match liquidate(shares, bp, c, price, &V) {
                    Ok(l) => {
                        ok += 1;
                        assert!(l.debt_shares < shares, "{shares} {c} {price}");
                        assert_eq!(
                            l.debt_shares,
                            repay_shares(shares, l.repay, bp).unwrap().shares
                        );
                    }
                    Err(NothingToLiquidate) => dust += 1,
                    Err(_) => {}
                }
            }
        }
    }
    assert!(ok > 100 && dust > 0, "{ok} {dust}");
}
