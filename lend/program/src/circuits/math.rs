//! Interest and share maths every circuit calls. The rules are
//! `core/src/lend.rs` (the plain Rust reference) and its vectors in
//! `web/tests/fixtures/lend-vectors.json`, which the tests below run through
//! this code natively.
//!
//! Values are `u64` in and out. Xark cannot divide a `u128` by a witness, so
//! wide products are taken in the BN254 field, where they cannot wrap at the
//! bit widths noted at each call, and each division is a hinted quotient and
//! remainder pinned by `q * d + r == n`, `r < d` and a range check on `q`.
//! Every refusal (overflow, time going backwards, a zero price, zero shares)
//! is a failed `require`: the circuit has no proof for it and the client gets
//! `RequirementFailed`.
use spr_program::{require, require_eq, xark, Field};

use super::ReserveState;

pub const PRICE_SCALE: u64 = 1_000_000_000_000;
pub const BPS: u64 = 10_000;
pub const YEAR: u64 = 31_536_000;
/// A borrow may not take utilisation above this, compared exactly.
pub const MAX_BORROW_UTIL_BPS: u64 = 9_500;

/// The largest stored rate and elapsed time the accrual takes, so its
/// products stay inside the field's exact range.
pub const MAX_RATE_BPS: u64 = 1 << 16;
pub const MAX_ELAPSED: u64 = 1 << 40;

/// A pool's fixed parameters: kinks `(util_bps, rate_bps)` from util 0 to
/// 10000, util strictly rising and rate never falling, and the fee on
/// interest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolParams {
    pub kinks: &'static [(u64, u64)],
    pub fee_bps: u64,
}

/// tUSD: the USDC curve (0 -> 5% at 88% -> 7% at 93% -> 15% at 100%), 10% fee.
pub const TUSD_POOL: PoolParams = PoolParams {
    kinks: &[(0, 0), (8_800, 500), (9_300, 700), (10_000, 1_500)],
    fee_bps: 1_000,
};

/// A vault's fixed parameters, in basis points: collateral factor,
/// liquidation threshold, max limit and penalty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VaultParams {
    pub cf_bps: u64,
    pub lt_bps: u64,
    pub lml_bps: u64,
    pub penalty_bps: u64,
}

impl VaultParams {
    /// `cf < lt < lml <= 1e4` and `lt x (1e4 + penalty) < 1e8`: otherwise
    /// liquidating would raise the loan-to-value (`core/src/lend.rs`).
    pub const fn is_valid(&self) -> bool {
        self.cf_bps < self.lt_bps
            && self.lt_bps < self.lml_bps
            && self.lml_bps <= BPS
            && self.lt_bps * (BPS + self.penalty_bps) < BPS * BPS
    }
}

/// tSOL collateral against tUSD debt: 80 / 85 / 90 / 1%.
pub const TSOL_TUSD_VAULT: VaultParams = VaultParams {
    cf_bps: 8_000,
    lt_bps: 8_500,
    lml_bps: 9_000,
    penalty_bps: 100,
};
const _: () = assert!(TSOL_TUSD_VAULT.is_valid());

fn f(x: u64) -> Field {
    Field::from(x)
}

/// A `u64` packed with a bit above it, so the input reader turns a field
/// element into a `u64` with one proven 65-bit decomposition.
#[xark::input]
struct Word {
    value: u64,
    high: bool,
}

/// `x` as a `u64`, refused when it does not fit.
pub fn to_u64(x: Field) -> u64 {
    // Natively the reader asserts the bound; check it first so a refusal is a
    // recorded requirement, not a panic.
    #[cfg(not(xark))]
    let x = {
        let fits = x.to_limbs().get(1..) == Some(&[0u64; 3][..]);
        require(fits);
        if fits {
            x
        } else {
            Field::zero()
        }
    };
    let word: Word = xark::input::from_fields(&[x]);
    require(!word.high);
    word.value
}

/// Xark's hints must not take a product of two witnesses: the compiled
/// circuit hands such a hint 0 (seen on a local network: `hint_div_rem(a * b,
/// c)` gave `q = r = 0`, so the proof failed though the native run passed).
/// Every quotient here comes from `hint_mulmod_divmod`, which takes the two
/// factors separately, and a product used as a factor is materialised by
/// [`mul`] first.
fn quotient(a: Field, b: Field, d: Field) -> (Field, Field) {
    let ([q], [r]) = Field::hint_mulmod_divmod::<1>([a], [b], [d], 128);
    (q, r)
}

/// `a x b` as its own wire, for `a x b < 2^128`; refused when wider.
fn mul(a: Field, b: Field) -> Field {
    let (q, r) = quotient(a, b, Field::from(1u8));
    require_eq(q, a * b);
    require_eq(r, Field::zero());
    q
}

/// `(floor(a x b / d), a x b mod d)` for `a x b < 2^NB`, `0 < d < 2^DB`.
/// Refused for `d == 0`. `q < 2^128` and `r < d < 2^DB`, so `q * d + r`
/// cannot wrap when `NB + DB <= 252`; a quotient past 128 bits is refused.
fn div_rem<const NB: usize, const DB: usize>(a: Field, b: Field, d: Field) -> (Field, Field) {
    const { assert!(NB + DB <= 252) };
    let zero = d == Field::zero();
    require(!zero);
    let d = if zero { Field::from(1u8) } else { d };
    let (q, r) = quotient(a, b, d);
    require_eq(q * d + r, a * b);
    require(r.lt::<DB>(d));
    let _ = q.to_bits::<128>();
    (q, r)
}

fn div_floor<const NB: usize, const DB: usize>(a: Field, b: Field, d: Field) -> Field {
    div_rem::<NB, DB>(a, b, d).0
}

fn div_ceil<const NB: usize, const DB: usize>(a: Field, b: Field, d: Field) -> Field {
    let (q, r) = div_rem::<NB, DB>(a, b, d);
    q + Field::from(r != Field::zero())
}

/// `a + b`, refused when it overflows.
pub fn add(a: u64, b: u64) -> u64 {
    to_u64(f(a) + f(b))
}

/// `a - b`, refused when it underflows.
pub fn sub(a: u64, b: u64) -> u64 {
    require(a >= b);
    if a >= b {
        a - b
    } else {
        0
    }
}

/// `floor(shares x supply_price / 1e12)`.
pub fn assets_of(shares: u64, supply_price: u64) -> u64 {
    to_u64(div_floor::<128, 40>(
        f(shares),
        f(supply_price),
        f(PRICE_SCALE),
    ))
}

/// `ceil(shares x borrow_price / 1e12)`.
pub fn debt_of(shares: u64, borrow_price: u64) -> u64 {
    to_u64(div_ceil::<128, 40>(
        f(shares),
        f(borrow_price),
        f(PRICE_SCALE),
    ))
}

pub fn total_supply(state: &ReserveState) -> u64 {
    assets_of(state.supply_shares, state.supply_price)
}

pub fn total_debt(state: &ReserveState) -> u64 {
    debt_of(state.debt_shares, state.borrow_price)
}

/// 0 when nothing is supplied, else `min(10000, floor(debt x 10000 / supply))`.
pub fn utilisation_bps(total_supply: u64, total_debt: u64) -> u64 {
    let empty = total_supply == 0;
    let divisor = if empty { 1 } else { total_supply };
    let util = div_floor::<78, 64>(f(total_debt), f(BPS), f(divisor));
    let capped = if util.gt::<78>(f(BPS)) { f(BPS) } else { util };
    // Read before branching: to_u64 declares a circuit input.
    let capped = to_u64(capped);
    if empty {
        0
    } else {
        capped
    }
}

pub fn reserve_utilisation_bps(state: &ReserveState) -> u64 {
    utilisation_bps(total_supply(state), total_debt(state))
}

/// The borrow rate at `util_bps` on the pool's curve:
/// `r0 + floor((r1 - r0) x (u - u0) / (u1 - u0))` on the segment holding `u`.
pub fn curve_rate(kinks: &[(u64, u64)], util_bps: u64) -> u64 {
    // The kinks are constants: these checks fold away in the circuit.
    require(kinks.len() >= 2);
    require(kinks.first().map(|k| k.0) == Some(0));
    require(kinks.last().map(|k| k.0) == Some(BPS));
    for w in kinks.windows(2) {
        if let [(u0, r0), (u1, r1)] = w {
            require(u1 > u0 && r1 >= r0);
        }
    }
    require(util_bps <= BPS);
    let mut rate = 0u64;
    let mut found = false;
    for w in kinks.windows(2) {
        if let [(u0, r0), (u1, r1)] = *w {
            // Clamped into the segment, so the segments not taken stay in range.
            let u = if util_bps < u0 {
                u0
            } else if util_bps > u1 {
                u1
            } else {
                util_bps
            };
            // Invalid kinks are refused above; this only keeps native runs from panicking.
            let value = if u1 > u0 && r1 >= r0 {
                r0 + (r1 - r0) * (u - u0) / (u1 - u0)
            } else {
                0
            };
            let here = !found && util_bps <= u1;
            rate = if here { value } else { rate };
            found = found || here;
        }
    }
    rate
}

/// Applies the stored rate from `last_update` to `now` and sets
/// `last_update = now`: `borrow_price += ceil(bp x rate x dt / (1e4 x YEAR))`,
/// `supply_price += floor(sp x rate x (1e4 - fee) x util x dt / (1e12 x
/// YEAR))` with util taken before. Leaves the rate alone (see
/// [`refresh_rate`]). Zero elapsed time changes nothing.
pub fn accrue(state: &mut ReserveState, fee_bps: u64, now: u64) {
    require(now >= state.last_update);
    require(fee_bps <= BPS);
    let dt = sub(now, state.last_update);
    let in_range = state.rate_bps < MAX_RATE_BPS && dt < MAX_ELAPSED;
    require(in_range);
    // Out of range is already refused; zeros keep the products below inside
    // their bit widths (natively, past the refusal, they would panic).
    let dt = if in_range { dt } else { 0 };
    let util = reserve_utilisation_bps(state);
    let rate = f(if in_range { state.rate_bps } else { 0 });
    let fee_share = if fee_bps <= BPS { BPS - fee_bps } else { 0 };
    // 16 + 40 bits.
    let rate_time = mul(rate, f(dt));
    // 64 + 56 bits over 39.
    let borrow_inc = div_ceil::<120, 39>(f(state.borrow_price), rate_time, f(BPS) * f(YEAR));
    // 64 + (56 + 14 + 14) bits over 65.
    let supply_factor = mul(rate_time, mul(f(fee_share), f(util)));
    let supply_inc = div_floor::<148, 65>(
        f(state.supply_price),
        supply_factor,
        f(BPS) * f(BPS) * f(BPS) * f(YEAR),
    );
    state.borrow_price = to_u64(f(state.borrow_price) + borrow_inc);
    state.supply_price = to_u64(f(state.supply_price) + supply_inc);
    state.last_update = now;
}

/// Stores the curve rate of the state's utilisation, after an operation
/// changed the shares.
pub fn refresh_rate(state: &mut ReserveState, pool: &PoolParams) {
    state.rate_bps = curve_rate(pool.kinks, reserve_utilisation_bps(state));
}

/// The end of every reserve operation: the new rate and the keeper's fresh
/// salt.
pub fn refresh(state: &mut ReserveState, pool: &PoolParams, salt: u128) {
    refresh_rate(state, pool);
    state.salt = salt;
}

/// Refused when the state's debt is above `MAX_BORROW_UTIL_BPS` of its
/// supply: `total_debt x 1e4 > 9500 x total_supply`.
pub fn assert_borrow_cap(state: &ReserveState) {
    let debt = f(total_debt(state)) * f(BPS);
    let cap = f(total_supply(state)) * f(MAX_BORROW_UTIL_BPS);
    require(!debt.gt::<80>(cap));
}

/// Supply shares minted for `amount`: `floor(amount x 1e12 / price)`, never
/// zero.
pub fn shares_for_supply(amount: u64, supply_price: u64) -> u64 {
    let shares = to_u64(div_floor::<104, 64>(
        f(amount),
        f(PRICE_SCALE),
        f(supply_price),
    ));
    require(shares != 0);
    shares
}

/// Supply shares burned to pay out `amount`: `ceil(amount x 1e12 / price)`.
pub fn shares_to_burn(amount: u64, supply_price: u64) -> u64 {
    to_u64(div_ceil::<104, 64>(
        f(amount),
        f(PRICE_SCALE),
        f(supply_price),
    ))
}

/// Debt shares added for borrowing `amount`: `ceil(amount x 1e12 / price)`.
pub fn borrow_shares(amount: u64, borrow_price: u64) -> u64 {
    to_u64(div_ceil::<104, 64>(
        f(amount),
        f(PRICE_SCALE),
        f(borrow_price),
    ))
}

/// A repay of `amount` on `shares` debt shares: `(shares left, change)`.
/// Covering the whole debt clears the shares and returns the rest; a smaller
/// repay removes `floor(amount x 1e12 / borrow_price)` shares.
pub fn repay_shares(shares: u64, amount: u64, borrow_price: u64) -> (u64, u64) {
    let debt = debt_of(shares, borrow_price);
    let full = amount >= debt;
    let change = if full { amount - debt } else { 0 };
    // Only a partial repay divides; it has a nonzero debt, so a nonzero price.
    let partial = if full { 0 } else { amount };
    let price = if full { 1 } else { borrow_price };
    let removed = to_u64(div_floor::<104, 64>(f(partial), f(PRICE_SCALE), f(price)));
    let left = if full { 0 } else { sub(shares, removed) };
    (left, change)
}

#[cfg(all(test, not(xark)))]
mod tests {
    use super::*;
    use serde_json::Value;

    const VECTORS: &str = include_str!("../../../../web/tests/fixtures/lend-vectors.json");

    fn num(v: &Value) -> u64 {
        v.as_str().expect("decimal string").parse().expect("u64")
    }

    fn reserve(v: &Value) -> ReserveState {
        ReserveState {
            supply_shares: num(&v["supply_shares"]),
            debt_shares: num(&v["debt_shares"]),
            supply_price: num(&v["supply_price"]),
            borrow_price: num(&v["borrow_price"]),
            rate_bps: num(&v["rate_bps"]),
            last_update: num(&v["last_update"]),
            salt: 0,
        }
    }

    fn kinks(v: &Value) -> &'static [(u64, u64)] {
        let list: Vec<(u64, u64)> = v
            .as_array()
            .expect("curve")
            .iter()
            .map(|k| (num(&k[0]), num(&k[1])))
            .collect();
        Box::leak(list.into_boxed_slice())
    }

    /// Runs `run` the way the circuit's native execution does.
    fn run<T>(run: impl FnOnce() -> T) -> Option<T> {
        spr_program::checked(|| Ok(run())).ok()
    }

    /// Every vector of the functions this module implements: an `out` must
    /// match, an `error` must be refused.
    #[test]
    fn passes_the_shared_vectors() {
        let all: Value = serde_json::from_str(VECTORS).expect("vectors");
        let mut checked = 0;
        for case in all["cases"].as_array().expect("cases") {
            let name = format!("{} / {}", case["fn"], case["name"]);
            let i = &case["in"];
            let got: Option<Value> = match case["fn"].as_str().unwrap() {
                "total_supply" => {
                    run(|| total_supply(&reserve(&i["reserve"]))).map(|x| x.to_string().into())
                }
                "total_debt" => {
                    run(|| total_debt(&reserve(&i["reserve"]))).map(|x| x.to_string().into())
                }
                "utilisation_bps" => run(|| reserve_utilisation_bps(&reserve(&i["reserve"])))
                    .map(|x| x.to_string().into()),
                "curve_rate" => run(|| curve_rate(kinks(&i["curve"]), num(&i["util_bps"])))
                    .map(|x| x.to_string().into()),
                "shares_for_supply" => {
                    run(|| shares_for_supply(num(&i["amount"]), num(&i["supply_price"])))
                        .map(|x| x.to_string().into())
                }
                "shares_to_burn" => {
                    run(|| shares_to_burn(num(&i["amount"]), num(&i["supply_price"])))
                        .map(|x| x.to_string().into())
                }
                "assets_of" => run(|| assets_of(num(&i["shares"]), num(&i["supply_price"])))
                    .map(|x| x.to_string().into()),
                "debt_of" => run(|| debt_of(num(&i["shares"]), num(&i["borrow_price"])))
                    .map(|x| x.to_string().into()),
                "borrow_shares" => {
                    run(|| borrow_shares(num(&i["amount"]), num(&i["borrow_price"])))
                        .map(|x| x.to_string().into())
                }
                "repay_shares" => run(|| {
                    repay_shares(
                        num(&i["shares"]),
                        num(&i["amount"]),
                        num(&i["borrow_price"]),
                    )
                })
                .map(
                    |(s, c)| serde_json::json!({"shares": s.to_string(), "change": c.to_string()}),
                ),
                "accrue" => run(|| {
                    let mut s = reserve(&i["reserve"]);
                    accrue(&mut s, num(&i["fee_bps"]), num(&i["now"]));
                    s
                })
                .map(|s| state_json(&s)),
                "refresh_rate" => run(|| {
                    let mut s = reserve(&i["reserve"]);
                    s.rate_bps = curve_rate(kinks(&i["curve"]), reserve_utilisation_bps(&s));
                    s
                })
                .map(|s| state_json(&s)),
                _ => continue,
            };
            checked += 1;
            match (&case["out"], got) {
                (Value::Null, None) => {}
                (Value::Null, Some(got)) => panic!("{name}: expected {}, got {got}", case["error"]),
                (want, Some(got)) => assert_eq!(&got, want, "{name}"),
                (want, None) => panic!("{name}: refused, expected {want}"),
            }
        }
        // The fixture's cases for the 13 functions above; the rest are Vault/Earn flows.
        assert_eq!(checked, 56, "vectors ran");
    }

    fn state_json(s: &ReserveState) -> Value {
        serde_json::json!({
            "supply_shares": s.supply_shares.to_string(),
            "debt_shares": s.debt_shares.to_string(),
            "supply_price": s.supply_price.to_string(),
            "borrow_price": s.borrow_price.to_string(),
            "rate_bps": s.rate_bps.to_string(),
            "last_update": s.last_update.to_string(),
        })
    }

    #[test]
    fn to_u64_round_trips_and_refuses_wide_values() {
        for x in [0, 1, 5, u64::MAX - 1, u64::MAX] {
            assert_eq!(run(|| to_u64(f(x))), Some(x));
        }
        assert_eq!(run(|| to_u64(f(u64::MAX) + Field::from(1u8))), None);
        assert_eq!(run(|| to_u64(f(u64::MAX) * f(u64::MAX))), None);
    }

    #[test]
    fn borrow_cap_is_exact() {
        let mut s = ReserveState {
            supply_shares: 10_000,
            debt_shares: 9_500,
            supply_price: PRICE_SCALE,
            borrow_price: PRICE_SCALE,
            ..Default::default()
        };
        assert!(run(|| assert_borrow_cap(&s)).is_some());
        s.debt_shares = 9_501;
        assert!(run(|| assert_borrow_cap(&s)).is_none());
    }

    #[test]
    fn repay_partial_and_full() {
        let price = PRICE_SCALE + PRICE_SCALE / 2; // 1.5: 10 shares owe 15
        assert_eq!(run(|| repay_shares(10, 4, price)), Some((8, 0)));
        assert_eq!(run(|| repay_shares(10, 14, price)), Some((1, 0)));
        assert_eq!(run(|| repay_shares(10, 15, price)), Some((0, 0)));
        assert_eq!(run(|| repay_shares(10, 20, price)), Some((0, 5)));
        assert_eq!(run(|| repay_shares(0, 20, 0)), Some((0, 20)));
    }
}
