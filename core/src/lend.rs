//! Reference maths for Keeler Lend. The circuits, program, keeper and page are
//! written against this file; `web/src/keeler/lend-math.ts` is its twin and
//! `web/tests/fixtures/lend-vectors.json` holds the vectors all of them pass.
//!
//! Integers only. Token amounts are u64, products are taken in u128 with
//! checked arithmetic, and anything that does not fit is `Overflow`.
//!
//! Scales: prices `PRICE_SCALE` = 1e12; rates and factors in basis points
//! (`BPS` = 10,000); `YEAR` = 31,536,000 s.
//!
//! Reserve: supply shares, debt shares, supply price and borrow price (both
//! start at 1e12), the stored borrow rate and the last update time.
//! - `total_supply = floor(supply_shares × supply_price / 1e12)`,
//!   `total_debt = ceil(debt_shares × borrow_price / 1e12)`.
//! - `utilisation_bps` = 0 when total supply is 0, else
//!   `min(10000, floor(total_debt × 10000 / total_supply))`.
//! - `accrue(now, fee)`: refused if `now < last_update`; with `dt = now −
//!   last_update` and the STORED rate, `borrow_price += ceil(borrow_price ×
//!   rate × dt / (1e4 × YEAR))` and `supply_price += floor(supply_price × rate ×
//!   (1e4 − fee) × util × dt / (1e12 × YEAR))`, util taken before the accrual;
//!   `dt = 0` changes nothing. Accrue leaves the rate alone.
//! - After any operation that changes shares the stored rate becomes
//!   `curve(utilisation_bps)` of the new state (`refresh_rate`). Callers
//!   accrue first, then operate.
//! - The curve is piecewise linear between kinks `(util, rate)` from util 0
//!   to 10000: `r0 + floor((r1 − r0) × (u − u0) / (u1 − u0))`. tUSD: 0 → 500
//!   at 8800 → 700 at 9300 → 1500 at 10000, fee 1000.
//!
//! Shares, always rounded for the pool: supply shares minted
//! `floor(amount × 1e12 / supply_price)` (zero is refused) and burned
//! `ceil(...)`; `assets_of = floor(shares × supply_price / 1e12)`; debt
//! `debt_of = ceil(shares × borrow_price / 1e12)`; a borrow adds
//! `ceil(amount × 1e12 / borrow_price)` shares; a repay of at least the debt
//! clears the shares and returns the change, a smaller one removes
//! `floor(amount × 1e12 / borrow_price)` shares.
//! A borrow is refused above idle liquidity or when it would take
//! utilisation above `MAX_BORROW_UTIL_BPS` (9500), compared exactly:
//! `total_debt × 1e4 > 9500 × total_supply` after it.
//!
//! Vault (`price` = debt base units per collateral base unit × 1e12):
//! `value = floor(collateral × price / 1e12)`. A draw needs `debt × 1e4 <= cf ×
//! value`; liquidatable when `debt × 1e4 > lt × value`; above the max limit
//! when `debt × 1e4 > lml × value`.
//!
//! Liquidation repaying `x`: the liquidator also pays `buffer = floor(x × p /
//! 1e4)` into the reserve and receives `payout(x) = floor((x + buffer) × 1e12
//! / price)` of collateral. `x` is fixed: above the max limit, the largest `x
//! <= debt` whose payout fits the collateral; otherwise also the largest such
//! `x` with `(debt − x) × 1e4 >= lt × floor((collateral − payout(x)) × price /
//! 1e12)` (the position is left at or just above the threshold, never pushed
//! past it). An `x` of zero is refused (`NothingToLiquidate`). The position's
//! debt shares then follow the repay rule for `x`.

use std::fmt;

pub const PRICE_SCALE: u64 = 1_000_000_000_000;
pub const BPS: u64 = 10_000;
pub const YEAR: u64 = 31_536_000;
pub const MAX_BORROW_UTIL_BPS: u32 = 9_500;

const S: u128 = PRICE_SCALE as u128;
const B: u128 = BPS as u128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LendError {
    Overflow,
    InvalidInput,
    TimeBackwards,
    ZeroAmount,
    ZeroShares,
    UtilisationCap,
    InsufficientLiquidity,
    InsufficientShares,
    NotLiquidatable,
    NothingToLiquidate,
}

impl LendError {
    /// The code used in the vectors and by the TypeScript twin.
    pub fn code(self) -> &'static str {
        match self {
            Self::Overflow => "overflow",
            Self::InvalidInput => "invalid_input",
            Self::TimeBackwards => "time_backwards",
            Self::ZeroAmount => "zero_amount",
            Self::ZeroShares => "zero_shares",
            Self::UtilisationCap => "utilisation_cap",
            Self::InsufficientLiquidity => "insufficient_liquidity",
            Self::InsufficientShares => "insufficient_shares",
            Self::NotLiquidatable => "not_liquidatable",
            Self::NothingToLiquidate => "nothing_to_liquidate",
        }
    }
}

impl fmt::Display for LendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for LendError {}

pub type Result<T> = std::result::Result<T, LendError>;
use LendError::*;

fn mul(a: u128, b: u128) -> Result<u128> {
    a.checked_mul(b).ok_or(Overflow)
}

fn to_u64(x: u128) -> Result<u64> {
    u64::try_from(x).map_err(|_| Overflow)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reserve {
    pub supply_shares: u64,
    pub debt_shares: u64,
    pub supply_price: u64,
    pub borrow_price: u64,
    pub rate_bps: u32,
    pub last_update: u64,
}

impl Reserve {
    /// An empty reserve as `init_reserve` creates it.
    pub const fn new(now: u64, rate_bps: u32) -> Self {
        Self {
            supply_shares: 0,
            debt_shares: 0,
            supply_price: PRICE_SCALE,
            borrow_price: PRICE_SCALE,
            rate_bps,
            last_update: now,
        }
    }
}

/// `(util_bps, rate_bps)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kink {
    pub util_bps: u32,
    pub rate_bps: u32,
}

/// Kinks from util 0 to 10000, util strictly rising, rate never falling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Curve<'a> {
    pub kinks: &'a [Kink],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pool {
    pub curve: Curve<'static>,
    pub fee_bps: u32,
}

const fn kink(util_bps: u32, rate_bps: u32) -> Kink {
    Kink { util_bps, rate_bps }
}

/// The USDC curve: 0 → 5% at 88% → 7% at 93% → 15% at 100%, fee 10%.
pub const TUSD_POOL: Pool = Pool {
    curve: Curve {
        kinks: &[
            kink(0, 0),
            kink(8800, 500),
            kink(9300, 700),
            kink(10_000, 1500),
        ],
    },
    fee_bps: 1000,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Vault {
    pub cf_bps: u32,
    pub lt_bps: u32,
    pub lml_bps: u32,
    pub penalty_bps: u32,
}

pub const TSOL_TUSD_VAULT: Vault = Vault {
    cf_bps: 8000,
    lt_bps: 8500,
    lml_bps: 9000,
    penalty_bps: 100,
};

pub fn total_supply(r: &Reserve) -> Result<u64> {
    to_u64(r.supply_shares as u128 * r.supply_price as u128 / S)
}

pub fn total_debt(r: &Reserve) -> Result<u64> {
    to_u64((r.debt_shares as u128 * r.borrow_price as u128).div_ceil(S))
}

pub fn utilisation_bps(r: &Reserve) -> Result<u32> {
    let supply = total_supply(r)? as u128;
    if supply == 0 {
        return Ok(0);
    }
    let debt = total_debt(r)? as u128;
    Ok((debt * B / supply).min(B) as u32)
}

pub fn accrue(r: &Reserve, now: u64, fee_bps: u32) -> Result<Reserve> {
    if now < r.last_update {
        return Err(TimeBackwards);
    }
    if fee_bps as u128 > B {
        return Err(InvalidInput);
    }
    let dt = (now - r.last_update) as u128;
    if dt == 0 {
        return Ok(*r);
    }
    let util = utilisation_bps(r)? as u128;
    let (bp, sp, rate) = (
        r.borrow_price as u128,
        r.supply_price as u128,
        r.rate_bps as u128,
    );
    let borrow_inc = mul(mul(bp, rate)?, dt)?.div_ceil(B * YEAR as u128);
    let supply_num = mul(mul(mul(mul(sp, rate)?, B - fee_bps as u128)?, util)?, dt)?;
    let supply_inc = supply_num / (B * B * B * YEAR as u128);
    Ok(Reserve {
        borrow_price: to_u64(bp + borrow_inc)?,
        supply_price: to_u64(sp + supply_inc)?,
        last_update: now,
        ..*r
    })
}

pub fn curve_rate(curve: &Curve, util_bps: u32) -> Result<u32> {
    let k = curve.kinks;
    let bad = k.len() < 2
        || k[0].util_bps != 0
        || k[k.len() - 1].util_bps as u128 != B
        || k.windows(2)
            .any(|w| w[1].util_bps <= w[0].util_bps || w[1].rate_bps < w[0].rate_bps);
    if bad || util_bps as u128 > B {
        return Err(InvalidInput);
    }
    let w = k
        .windows(2)
        .find(|w| util_bps <= w[1].util_bps)
        .expect("the last kink is 10000");
    let (u0, r0, u1, r1) = (
        w[0].util_bps as u64,
        w[0].rate_bps as u64,
        w[1].util_bps as u64,
        w[1].rate_bps as u64,
    );
    // At most r1, so it fits.
    Ok((r0 + (r1 - r0) * (util_bps as u64 - u0) / (u1 - u0)) as u32)
}

/// Stores `curve(utilisation_bps)` of `r`, after an operation changed shares.
pub fn refresh_rate(r: &Reserve, curve: &Curve) -> Result<Reserve> {
    Ok(Reserve {
        rate_bps: curve_rate(curve, utilisation_bps(r)?)?,
        ..*r
    })
}

pub fn shares_for_supply(amount: u64, supply_price: u64) -> Result<u64> {
    if supply_price == 0 {
        return Err(InvalidInput);
    }
    match to_u64(amount as u128 * S / supply_price as u128)? {
        0 => Err(ZeroShares),
        s => Ok(s),
    }
}

pub fn shares_to_burn(amount: u64, supply_price: u64) -> Result<u64> {
    if supply_price == 0 {
        return Err(InvalidInput);
    }
    to_u64((amount as u128 * S).div_ceil(supply_price as u128))
}

pub fn assets_of(shares: u64, supply_price: u64) -> Result<u64> {
    to_u64(shares as u128 * supply_price as u128 / S)
}

pub fn debt_of(shares: u64, borrow_price: u64) -> Result<u64> {
    to_u64((shares as u128 * borrow_price as u128).div_ceil(S))
}

pub fn borrow_shares(amount: u64, borrow_price: u64) -> Result<u64> {
    if borrow_price == 0 {
        return Err(InvalidInput);
    }
    to_u64((amount as u128 * S).div_ceil(borrow_price as u128))
}

/// A position's debt shares and the change after a repay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Repaid {
    pub shares: u64,
    pub change: u64,
}

pub fn repay_shares(shares: u64, amount: u64, borrow_price: u64) -> Result<Repaid> {
    let debt = debt_of(shares, borrow_price)?;
    if amount >= debt {
        return Ok(Repaid {
            shares: 0,
            change: amount - debt,
        });
    }
    // amount < ceil(shares × bp / S) gives amount × S / bp < shares, so this fits.
    let removed = (amount as u128 * S / borrow_price as u128) as u64;
    Ok(Repaid {
        shares: shares - removed,
        change: 0,
    })
}

/// Mints supply shares for `amount`; returns the new reserve and the shares.
pub fn supply(r: &Reserve, amount: u64, curve: &Curve) -> Result<(Reserve, u64)> {
    let minted = shares_for_supply(amount, r.supply_price)?;
    let next = Reserve {
        supply_shares: r.supply_shares.checked_add(minted).ok_or(Overflow)?,
        ..*r
    };
    Ok((refresh_rate(&next, curve)?, minted))
}

/// Withdraws `amount` against a receipt of `held` shares; returns the new
/// reserve and the shares burned.
pub fn redeem(
    r: &Reserve,
    idle: u64,
    held: u64,
    amount: u64,
    curve: &Curve,
) -> Result<(Reserve, u64)> {
    if amount == 0 {
        return Err(ZeroAmount);
    }
    if amount > idle {
        return Err(InsufficientLiquidity);
    }
    let burned = shares_to_burn(amount, r.supply_price)?;
    if burned > held || burned > r.supply_shares {
        return Err(InsufficientShares);
    }
    let next = Reserve {
        supply_shares: r.supply_shares - burned,
        ..*r
    };
    Ok((refresh_rate(&next, curve)?, burned))
}

/// Lends `amount` out of `idle`; returns the new reserve and the debt shares added.
pub fn borrow(r: &Reserve, idle: u64, amount: u64, curve: &Curve) -> Result<(Reserve, u64)> {
    if amount == 0 {
        return Err(ZeroAmount);
    }
    if amount > idle {
        return Err(InsufficientLiquidity);
    }
    let added = borrow_shares(amount, r.borrow_price)?;
    let next = Reserve {
        debt_shares: r.debt_shares.checked_add(added).ok_or(Overflow)?,
        ..*r
    };
    // Exact, not the floored utilisation, so 95.001% is refused too.
    if total_debt(&next)? as u128 * B > MAX_BORROW_UTIL_BPS as u128 * total_supply(&next)? as u128 {
        return Err(UtilisationCap);
    }
    Ok((refresh_rate(&next, curve)?, added))
}

/// Repays `amount` on a position of `position_shares` debt shares.
pub fn repay(
    r: &Reserve,
    position_shares: u64,
    amount: u64,
    curve: &Curve,
) -> Result<(Reserve, Repaid)> {
    if position_shares > r.debt_shares {
        return Err(InvalidInput);
    }
    let done = repay_shares(position_shares, amount, r.borrow_price)?;
    let next = Reserve {
        debt_shares: r.debt_shares - (position_shares - done.shares),
        ..*r
    };
    Ok((refresh_rate(&next, curve)?, done))
}

pub fn collateral_value(collateral: u64, price: u64) -> Result<u64> {
    to_u64(collateral as u128 * price as u128 / S)
}

pub fn can_draw(debt: u64, value: u64, v: &Vault) -> bool {
    debt as u128 * B <= v.cf_bps as u128 * value as u128
}

pub fn is_liquidatable(debt: u64, value: u64, v: &Vault) -> bool {
    debt as u128 * B > v.lt_bps as u128 * value as u128
}

pub fn above_max_limit(debt: u64, value: u64, v: &Vault) -> bool {
    debt as u128 * B > v.lml_bps as u128 * value as u128
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Liquidation {
    /// Debt repaid, `x`.
    pub repay: u64,
    /// Paid on top of `repay`, kept by the reserve.
    pub buffer: u64,
    /// Collateral to the liquidator.
    pub payout: u64,
    /// The position after.
    pub debt_shares: u64,
    pub collateral: u64,
}

/// `floor((x + floor(x × p / 1e4)) × 1e12 / price)`, uncapped.
fn payout(x: u128, p: u128, price: u128) -> u128 {
    (x + x * p / B) * S / price
}

pub fn liquidate(
    debt_shares: u64,
    borrow_price: u64,
    collateral: u64,
    price: u64,
    v: &Vault,
) -> Result<Liquidation> {
    let (lt, p) = (v.lt_bps as u128, v.penalty_bps as u128);
    // M = 1e8 − lt × (1e4 + p) must be positive, or liquidating raises the LTV.
    if price == 0 || lt * (B + p) >= B * B {
        return Err(InvalidInput);
    }
    let m = B * B - lt * (B + p);
    let debt = debt_of(debt_shares, borrow_price)?;
    let value = collateral_value(collateral, price)?;
    if !is_liquidatable(debt, value, v) {
        return Err(NotLiquidatable);
    }
    let (d, c, pr) = (debt as u128, collateral as u128, price as u128);
    // payout(x) <= c  ⇔  x + floor(x p / B) <= g  ⇔  x <= x_fit.
    let g = ((c + 1) * pr - 1) / S;
    let x_fit = ((g + 1) * B - 1) / (B + p);
    let x_max = d.min(x_fit);
    let x = if above_max_limit(debt, value, v) {
        x_max
    } else {
        // f(x) = (d − x) B − lt × floor((c − payout(x)) price / S) is below
        // zero for every x >= (d B − lt value + lt) B / m, so the answer is at
        // or under `start`. Walk down stair by stair (x values sharing one
        // payout); within a stair f falls as x rises, so each is one division.
        let start = ((d * B + lt - lt * value as u128) * B).div_ceil(m);
        let mut x = x_max.min(start);
        loop {
            let paid = payout(x, p, pr);
            let left = (c - paid) * pr / S;
            let lo = ((paid * pr).div_ceil(S) * B).div_ceil(B + p);
            let need = (lt * left).div_ceil(B);
            if d >= need && d - need >= lo {
                break x.min(d - need);
            }
            // lo == 0 cannot be reached: f(0) > 0 when liquidatable.
            x = lo - 1;
        }
    };
    if x == 0 {
        return Err(NothingToLiquidate);
    }
    let paid = payout(x, p, pr);
    let after = repay_shares(debt_shares, x as u64, borrow_price)?;
    Ok(Liquidation {
        repay: x as u64,
        buffer: (x * p / B) as u64,
        payout: paid as u64,
        debt_shares: after.shares,
        collateral: (c - paid) as u64,
    })
}

/// Loan-to-value in bps, rounded up; `None` for debt against no value.
pub fn health_bps(debt: u64, value: u64) -> Option<u128> {
    match (debt, value) {
        (0, _) => Some(0),
        (_, 0) => None,
        _ => Some((debt as u128 * B).div_ceil(value as u128)),
    }
}

/// The highest price (scaled 1e12) at which the position is liquidatable;
/// `None` without debt or collateral or with `lt_bps` 0.
pub fn liquidation_price(debt: u64, collateral: u64, lt_bps: u32) -> Option<u128> {
    if debt == 0 || collateral == 0 || lt_bps == 0 {
        return None;
    }
    // Liquidatable ⇔ value <= ceil(debt B / lt) − 1 ⇔ collateral × price < ceil(..) × S.
    let v = (debt as u128 * B).div_ceil(lt_bps as u128);
    Some((v * S - 1) / collateral as u128)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Apr {
    pub borrow_bps: u32,
    pub supply_bps: u32,
}

/// Borrow APR is the rate; supply APR is `floor(rate × (1e4 − fee) × util / 1e8)`.
pub fn apr_bps(rate_bps: u32, util_bps: u32, fee_bps: u32) -> Result<Apr> {
    if util_bps as u128 > B || fee_bps as u128 > B {
        return Err(InvalidInput);
    }
    let supply = rate_bps as u128 * (B - fee_bps as u128) * util_bps as u128 / (B * B);
    Ok(Apr {
        borrow_bps: rate_bps,
        supply_bps: supply as u32,
    })
}

#[cfg(test)]
mod tests;
