// Port of `core/src/lend.rs` in bigint; the rules are in that file's header.
// Both pass web/tests/fixtures/lend-vectors.json.

export const PRICE_SCALE = 1_000_000_000_000n;
export const BPS = 10_000n;
export const YEAR = 31_536_000n;
export const MAX_BORROW_UTIL_BPS = 9_500n;

const S = PRICE_SCALE;
const B = BPS;
const U32_MAX = (1n << 32n) - 1n;
const U64_MAX = (1n << 64n) - 1n;
const U128_MAX = (1n << 128n) - 1n;

export type LendErrorCode =
  | "overflow"
  | "invalid_input"
  | "time_backwards"
  | "zero_amount"
  | "zero_shares"
  | "utilisation_cap"
  | "insufficient_liquidity"
  | "insufficient_shares"
  | "not_liquidatable"
  | "nothing_to_liquidate";

export class LendError extends Error {
  constructor(readonly code: LendErrorCode) {
    super(code);
    this.name = "LendError";
  }
}

export type Reserve = {
  supplyShares: bigint;
  debtShares: bigint;
  supplyPrice: bigint;
  borrowPrice: bigint;
  rateBps: bigint;
  lastUpdate: bigint;
};
export type Kink = { utilBps: bigint; rateBps: bigint };
export type Curve = { kinks: readonly Kink[] };
export type Pool = { curve: Curve; feeBps: bigint };
export type Vault = { cfBps: bigint; ltBps: bigint; lmlBps: bigint; penaltyBps: bigint };
export type Repaid = { shares: bigint; change: bigint };
export type Liquidation = { repay: bigint; buffer: bigint; payout: bigint; debtShares: bigint; collateral: bigint };
export type Apr = { borrowBps: bigint; supplyBps: bigint };

export const TUSD_POOL: Pool = {
  curve: {
    kinks: [
      { utilBps: 0n, rateBps: 0n },
      { utilBps: 8800n, rateBps: 500n },
      { utilBps: 9300n, rateBps: 700n },
      { utilBps: 10_000n, rateBps: 1500n },
    ],
  },
  feeBps: 1000n,
};

export const TSOL_TUSD_VAULT: Vault = { cfBps: 8000n, ltBps: 8500n, lmlBps: 9000n, penaltyBps: 100n };

const fail = (code: LendErrorCode): never => {
  throw new LendError(code);
};
/** Rust's integer types: anything outside them is not a valid input. */
const u64 = (...xs: bigint[]) => xs.forEach((x) => (x < 0n || x > U64_MAX) && fail("invalid_input"));
const u32 = (...xs: bigint[]) => xs.forEach((x) => (x < 0n || x > U32_MAX) && fail("invalid_input"));
const checkReserve = (r: Reserve) => {
  u64(r.supplyShares, r.debtShares, r.supplyPrice, r.borrowPrice, r.lastUpdate);
  u32(r.rateBps);
};
const checkVault = (v: Vault) => u32(v.cfBps, v.ltBps, v.lmlBps, v.penaltyBps);
/** `checked_mul` in u128. */
const mul = (a: bigint, b: bigint) => (a * b > U128_MAX ? fail("overflow") : a * b);
const toU64 = (x: bigint) => (x > U64_MAX ? fail("overflow") : x);
const divCeil = (a: bigint, b: bigint) => (a + b - 1n) / b;
const min = (a: bigint, b: bigint) => (a < b ? a : b);

/** An empty reserve as `init_reserve` creates it. */
export function newReserve(now: bigint, rateBps: bigint): Reserve {
  return { supplyShares: 0n, debtShares: 0n, supplyPrice: S, borrowPrice: S, rateBps, lastUpdate: now };
}

export function totalSupply(r: Reserve): bigint {
  checkReserve(r);
  return toU64((r.supplyShares * r.supplyPrice) / S);
}

export function totalDebt(r: Reserve): bigint {
  checkReserve(r);
  return toU64(divCeil(r.debtShares * r.borrowPrice, S));
}

export function utilisationBps(r: Reserve): bigint {
  const supply = totalSupply(r);
  if (supply === 0n) return 0n;
  return min((totalDebt(r) * B) / supply, B);
}

export function accrue(r: Reserve, now: bigint, feeBps: bigint): Reserve {
  checkReserve(r);
  u64(now);
  u32(feeBps);
  if (now < r.lastUpdate) fail("time_backwards");
  if (feeBps > B) fail("invalid_input");
  const dt = now - r.lastUpdate;
  if (dt === 0n) return { ...r };
  const util = utilisationBps(r);
  const borrowInc = divCeil(mul(mul(r.borrowPrice, r.rateBps), dt), B * YEAR);
  const supplyNum = mul(mul(mul(mul(r.supplyPrice, r.rateBps), B - feeBps), util), dt);
  const supplyInc = supplyNum / (B * B * B * YEAR);
  return {
    ...r,
    borrowPrice: toU64(r.borrowPrice + borrowInc),
    supplyPrice: toU64(r.supplyPrice + supplyInc),
    lastUpdate: now,
  };
}

export function curveRate(curve: Curve, utilBps: bigint): bigint {
  const k = curve.kinks;
  k.forEach((x) => u32(x.utilBps, x.rateBps));
  u32(utilBps);
  const first = k[0];
  const last = k[k.length - 1];
  const bad =
    !first ||
    !last ||
    k.length < 2 ||
    first.utilBps !== 0n ||
    last.utilBps !== B ||
    k.some((x, i) => i > 0 && (x.utilBps <= k[i - 1]!.utilBps || x.rateBps < k[i - 1]!.rateBps));
  if (bad || utilBps > B) fail("invalid_input");
  const i = k.findIndex((x, i) => i > 0 && utilBps <= x.utilBps);
  const [a, b] = [k[i - 1]!, k[i]!];
  return a.rateBps + ((b.rateBps - a.rateBps) * (utilBps - a.utilBps)) / (b.utilBps - a.utilBps);
}

/** Stores `curve(utilisationBps)` of `r`, after an operation changed shares. */
export function refreshRate(r: Reserve, curve: Curve): Reserve {
  return { ...r, rateBps: curveRate(curve, utilisationBps(r)) };
}

export function sharesForSupply(amount: bigint, supplyPrice: bigint): bigint {
  u64(amount, supplyPrice);
  if (supplyPrice === 0n) fail("invalid_input");
  const s = toU64((amount * S) / supplyPrice);
  return s === 0n ? fail("zero_shares") : s;
}

export function sharesToBurn(amount: bigint, supplyPrice: bigint): bigint {
  u64(amount, supplyPrice);
  if (supplyPrice === 0n) fail("invalid_input");
  return toU64(divCeil(amount * S, supplyPrice));
}

export function assetsOf(shares: bigint, supplyPrice: bigint): bigint {
  u64(shares, supplyPrice);
  return toU64((shares * supplyPrice) / S);
}

export function debtOf(shares: bigint, borrowPrice: bigint): bigint {
  u64(shares, borrowPrice);
  return toU64(divCeil(shares * borrowPrice, S));
}

export function borrowShares(amount: bigint, borrowPrice: bigint): bigint {
  u64(amount, borrowPrice);
  if (borrowPrice === 0n) fail("invalid_input");
  return toU64(divCeil(amount * S, borrowPrice));
}

export function repayShares(shares: bigint, amount: bigint, borrowPrice: bigint): Repaid {
  u64(amount);
  const debt = debtOf(shares, borrowPrice);
  if (amount >= debt) return { shares: 0n, change: amount - debt };
  return { shares: shares - (amount * S) / borrowPrice, change: 0n };
}

/** Mints supply shares for `amount`. */
export function supply(r: Reserve, amount: bigint, curve: Curve): { reserve: Reserve; shares: bigint } {
  checkReserve(r);
  const shares = sharesForSupply(amount, r.supplyPrice);
  const next = { ...r, supplyShares: r.supplyShares + shares };
  if (next.supplyShares > U64_MAX) fail("overflow");
  return { reserve: refreshRate(next, curve), shares };
}

/** Withdraws `amount` against a receipt of `held` shares. */
export function redeem(
  r: Reserve,
  idle: bigint,
  held: bigint,
  amount: bigint,
  curve: Curve,
): { reserve: Reserve; burned: bigint } {
  checkReserve(r);
  u64(idle, held, amount);
  if (amount === 0n) fail("zero_amount");
  if (amount > idle) fail("insufficient_liquidity");
  const burned = sharesToBurn(amount, r.supplyPrice);
  if (burned > held || burned > r.supplyShares) fail("insufficient_shares");
  return { reserve: refreshRate({ ...r, supplyShares: r.supplyShares - burned }, curve), burned };
}

/** Lends `amount` out of `idle`; refused past idle or 95% utilisation, compared exactly. */
export function borrow(r: Reserve, idle: bigint, amount: bigint, curve: Curve): { reserve: Reserve; shares: bigint } {
  checkReserve(r);
  u64(idle, amount);
  if (amount === 0n) fail("zero_amount");
  if (amount > idle) fail("insufficient_liquidity");
  const shares = borrowShares(amount, r.borrowPrice);
  const next = { ...r, debtShares: r.debtShares + shares };
  if (next.debtShares > U64_MAX) fail("overflow");
  if (totalDebt(next) * B > MAX_BORROW_UTIL_BPS * totalSupply(next)) fail("utilisation_cap");
  return { reserve: refreshRate(next, curve), shares };
}

/** Repays `amount` on a position of `positionShares` debt shares. */
export function repay(
  r: Reserve,
  positionShares: bigint,
  amount: bigint,
  curve: Curve,
): { reserve: Reserve } & Repaid {
  checkReserve(r);
  u64(positionShares);
  if (positionShares > r.debtShares) fail("invalid_input");
  const done = repayShares(positionShares, amount, r.borrowPrice);
  const next = { ...r, debtShares: r.debtShares - (positionShares - done.shares) };
  return { reserve: refreshRate(next, curve), ...done };
}

export function collateralValue(collateral: bigint, price: bigint): bigint {
  u64(collateral, price);
  return toU64((collateral * price) / S);
}

export function canDraw(debt: bigint, value: bigint, v: Vault): boolean {
  u64(debt, value);
  checkVault(v);
  return debt * B <= v.cfBps * value;
}

export function isLiquidatable(debt: bigint, value: bigint, v: Vault): boolean {
  u64(debt, value);
  checkVault(v);
  return debt * B > v.ltBps * value;
}

export function aboveMaxLimit(debt: bigint, value: bigint, v: Vault): boolean {
  u64(debt, value);
  checkVault(v);
  return debt * B > v.lmlBps * value;
}

/** `floor((x + floor(x × p / 1e4)) × 1e12 / price)`, uncapped. */
const payoutOf = (x: bigint, p: bigint, price: bigint) => ((x + (x * p) / B) * S) / price;

export function liquidate(
  debtShares: bigint,
  borrowPrice: bigint,
  collateral: bigint,
  price: bigint,
  v: Vault,
): Liquidation {
  checkVault(v);
  u64(price);
  const [lt, p] = [v.ltBps, v.penaltyBps];
  if (price === 0n || lt * (B + p) >= B * B) fail("invalid_input");
  const m = B * B - lt * (B + p);
  const debt = debtOf(debtShares, borrowPrice);
  const value = collateralValue(collateral, price);
  if (!isLiquidatable(debt, value, v)) fail("not_liquidatable");
  const [d, c] = [debt, collateral];
  const g = ((c + 1n) * price - 1n) / S;
  const xFit = ((g + 1n) * B - 1n) / (B + p);
  const xMax = min(d, xFit);
  let x: bigint;
  if (aboveMaxLimit(debt, value, v)) {
    x = xMax;
  } else {
    // The stair walk of the Rust version; see the comments there.
    x = min(xMax, divCeil((d * B + lt - lt * value) * B, m));
    for (;;) {
      const paid = payoutOf(x, p, price);
      const left = ((c - paid) * price) / S;
      const lo = divCeil(divCeil(paid * price, S) * B, B + p);
      const need = divCeil(lt * left, B);
      if (d >= need && d - need >= lo) {
        x = min(x, d - need);
        break;
      }
      x = lo - 1n;
    }
  }
  if (x === 0n) fail("nothing_to_liquidate");
  const paid = payoutOf(x, p, price);
  return {
    repay: x,
    buffer: (x * p) / B,
    payout: paid,
    debtShares: repayShares(debtShares, x, borrowPrice).shares,
    collateral: c - paid,
  };
}

/** Loan-to-value in bps, rounded up; null for debt against no value. */
export function healthBps(debt: bigint, value: bigint): bigint | null {
  u64(debt, value);
  if (debt === 0n) return 0n;
  if (value === 0n) return null;
  return divCeil(debt * B, value);
}

/** The highest price (scaled 1e12) at which the position is liquidatable; null without debt or collateral or with `ltBps` 0. */
export function liquidationPrice(debt: bigint, collateral: bigint, ltBps: bigint): bigint | null {
  u64(debt, collateral);
  u32(ltBps);
  if (debt === 0n || collateral === 0n || ltBps === 0n) return null;
  return (divCeil(debt * B, ltBps) * S - 1n) / collateral;
}

/** Borrow APR is the rate; supply APR is `floor(rate × (1e4 − fee) × util / 1e8)`. */
export function aprBps(rateBps: bigint, utilBps: bigint, feeBps: bigint): Apr {
  u32(rateBps, utilBps, feeBps);
  if (utilBps > B || feeBps > B) fail("invalid_input");
  return { borrowBps: rateBps, supplyBps: (rateBps * (B - feeBps) * utilBps) / (B * B) };
}
