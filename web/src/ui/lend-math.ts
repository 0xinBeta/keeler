// Lending formulas the page needs, on the scales of docs/plan-lend.md: amounts in base units, rates and
// factors in basis points, prices 1e12-scaled debt base units per collateral base unit, a year of 31,536,000 s.
// shortcut: a local copy until web/src/keeler/lend-math.ts (the shared maths with vectors) lands; swap the imports then.
import type { RatePoint, Vault } from "../keeler/index";
import { formatUnits } from "./format";

export const BPS = 10_000n;
export const PRICE_SCALE = 10n ** 12n;
export const YEAR = 31_536_000;

const ceilDiv = (a: bigint, b: bigint) => (a + b - 1n) / b;

/** The collateral's value in debt base units. */
export const collateralValue = (collateral: bigint, price: bigint) => (collateral * price) / PRICE_SCALE;

/** Loan-to-value in basis points: debt × 10000 / (collateral × price / 1e12). 0 without debt, Infinity without collateral. */
export function healthBps(collateral: bigint, debt: bigint, price: bigint): number {
  if (debt === 0n) return 0;
  const value = collateralValue(collateral, price);
  if (value === 0n) return Infinity;
  return Number((debt * BPS) / value);
}

/** A position can be liquidated once its loan-to-value is above the threshold. */
export const liquidatable = (health: number, vault: Vault) => health > vault.liquidationThresholdBps;

/** The price below which the position becomes liquidatable; null without debt or collateral. */
export function liquidationPrice(collateral: bigint, debt: bigint, thresholdBps: number): bigint | null {
  if (debt === 0n || collateral === 0n) return null;
  return (debt * BPS * PRICE_SCALE) / (collateral * BigInt(thresholdBps));
}

/** Debt allowed against this collateral at the collateral factor, rounded down. */
export const borrowLimit = (collateral: bigint, price: bigint, factorBps: number) =>
  (collateral * price * BigInt(factorBps)) / (PRICE_SCALE * BPS);

/** How much more can be borrowed now. */
export const maxBorrow = (collateral: bigint, debt: bigint, price: bigint, factorBps: number) => {
  const limit = borrowLimit(collateral, price, factorBps);
  return limit > debt ? limit - debt : 0n;
};

/** How much collateral can be withdrawn while staying within the collateral factor. */
export function maxWithdraw(collateral: bigint, debt: bigint, price: bigint, factorBps: number): bigint {
  if (debt === 0n) return collateral;
  if (price === 0n) return 0n;
  const needed = ceilDiv(debt * BPS * PRICE_SCALE, price * BigInt(factorBps));
  return collateral > needed ? collateral - needed : 0n;
}

/**
 * What a liquidation takes, by the plan's rule: the largest repayment x that leaves the position at the threshold,
 * or the whole debt above the max limit; collateral worth x × (1 + penalty) is sold at `price`, capped at what there is.
 * null when the position is not liquidatable.
 */
export function liquidationSize(collateral: bigint, debt: bigint, price: bigint, vault: Vault): { repaid: bigint; sold: bigint } | null {
  const h = healthBps(collateral, debt, price);
  if (h <= vault.liquidationThresholdBps) return null;
  const lt = BigInt(vault.liquidationThresholdBps);
  const pen = BigInt(vault.penaltyBps);
  let repaid: bigint;
  if (h > vault.liquidationMaxLimitBps) repaid = debt;
  else {
    // d − x = LT × (V − x(1 + pen))  ⇒  x = (d − LT·V) / (1 − LT(1 + pen)), all in basis points.
    const value = collateralValue(collateral, price);
    repaid = ceilDiv(debt * BPS * BPS - lt * value * BPS, BPS * BPS - lt * (BPS + pen));
    if (repaid > debt) repaid = debt;
  }
  const want = ceilDiv(repaid * (BPS + pen) * PRICE_SCALE, BPS * price);
  return { repaid, sold: want > collateral ? collateral : want };
}

/** Borrow rate at a utilisation, linear between curve points. */
export function borrowRateBps(utilisationBps: number, curve: RatePoint[]): number {
  const u = Math.min(Math.max(utilisationBps, 0), 10_000);
  for (let i = 1; i < curve.length; i++) {
    const a = curve[i - 1]!;
    const b = curve[i]!;
    if (u <= b.utilisationBps)
      return Math.floor(a.rateBps + ((b.rateBps - a.rateBps) * (u - a.utilisationBps)) / (b.utilisationBps - a.utilisationBps));
  }
  return curve[curve.length - 1]?.rateBps ?? 0;
}

/** Suppliers earn the borrow rate, less the fee, spread over the whole pool. */
export const supplyRateBps = (borrowBps: number, utilisationBps: number, feeBps: number) =>
  Math.floor((borrowBps * (10_000 - feeBps) * Math.min(utilisationBps, 10_000)) / 1e8);

/** `amount` grown at `rateBps` a year for `secs` seconds, simple interest, rounded down. */
export const accrue = (amount: bigint, rateBps: number, secs: number) =>
  secs <= 0 ? amount : amount + (amount * BigInt(rateBps) * BigInt(Math.floor(secs))) / (BPS * BigInt(YEAR));

/** "62.4%" from basis points. */
export const pct = (bps: number, digits = 1) => (Number.isFinite(bps) ? `${(bps / 100).toFixed(digits)}%` : "—");

/** A 1e12-scaled price as debt tokens per whole collateral token, to 2 decimals ("142.37"). */
export function priceText(price: bigint, vault: Vault): string {
  const perWhole = (price * 10n ** BigInt(vault.collateral.decimals)) / PRICE_SCALE; // debt base units
  const cents = 10n ** BigInt(Math.max(vault.debt.decimals - 2, 0));
  return formatUnits((perWhole / cents) * cents, vault.debt.decimals);
}

/** Inverse of `priceText` for a whole-token price: debt tokens per collateral token → 1e12-scaled. */
export const priceFromWhole = (debtPerWhole: bigint, vault: Vault) =>
  (debtPerWhole * PRICE_SCALE) / 10n ** BigInt(vault.collateral.decimals);
