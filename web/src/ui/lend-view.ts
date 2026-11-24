// What the lending screens show, on top of the shared maths (keeler/lend-math.ts, which matches the program): the
// interface's units (bps as numbers, Infinity for debt against nothing) and price text. No formula of its own
// beyond the limits the program's `canDraw` implies.
import type { Vault } from "../keeler/index";
import * as L from "../keeler/lend-math";
import { formatUnits } from "./format";

/** Loan-to-value in basis points, rounded up as the program does; 0 without debt, Infinity without collateral. */
export function healthBps(collateral: bigint, debt: bigint, price: bigint): number {
  const h = L.healthBps(debt, L.collateralValue(collateral, price));
  return h === null ? Infinity : Number(h);
}

/** Past the threshold: `isLiquidatable`, read from the rounded-up health (equivalent for whole basis points). */
export const liquidatable = (health: number, vault: Vault) => health > vault.liquidationThresholdBps;

/** The highest price at which the position is liquidatable; null without debt or collateral. */
export const liquidationPrice = (collateral: bigint, debt: bigint, thresholdBps: number) => L.liquidationPrice(debt, collateral, BigInt(thresholdBps));

/** How much more can be borrowed now: the largest debt `canDraw` allows, less the debt. */
export function maxBorrow(collateral: bigint, debt: bigint, price: bigint, factorBps: number): bigint {
  const limit = (BigInt(factorBps) * L.collateralValue(collateral, price)) / L.BPS;
  return limit > debt ? limit - debt : 0n;
}

/** How much collateral can come out while `canDraw` still holds. */
export function maxWithdraw(collateral: bigint, debt: bigint, price: bigint, factorBps: number): bigint {
  if (debt === 0n) return collateral;
  if (price === 0n || factorBps === 0) return 0n;
  // canDraw ⇔ floor(c × price / 1e12) ≥ ceil(debt × 1e4 / cf) ⇔ c ≥ ceil(that × 1e12 / price).
  const value = (debt * L.BPS + BigInt(factorBps) - 1n) / BigInt(factorBps);
  const needed = (value * L.PRICE_SCALE + price - 1n) / price;
  return collateral > needed ? collateral - needed : 0n;
}

/** `amount` grown at `rateBps` a year for `secs` seconds, simple interest, rounded down: a display estimate between reads. */
export const grow = (amount: bigint, rateBps: number, secs: number) =>
  secs <= 0 ? amount : amount + (amount * BigInt(rateBps) * BigInt(Math.floor(secs))) / (L.BPS * L.YEAR);

/** "62.4%" from basis points. */
export const pct = (bps: number, digits = 1) => (Number.isFinite(bps) ? `${(bps / 100).toFixed(digits)}%` : "—");

/** A 1e12-scaled price as debt tokens per whole collateral token, to 2 decimals ("142.37"). */
export function priceText(price: bigint, vault: Vault): string {
  const perWhole = (price * 10n ** BigInt(vault.collateral.decimals)) / L.PRICE_SCALE; // debt base units
  const cents = 10n ** BigInt(Math.max(vault.debt.decimals - 2, 0));
  return formatUnits((perWhole / cents) * cents, vault.debt.decimals);
}

/** Inverse of `priceText` for a whole-token price: debt tokens per collateral token → 1e12-scaled. */
export const priceFromWhole = (debtPerWhole: bigint, vault: Vault) => (debtPerWhole * L.PRICE_SCALE) / 10n ** BigInt(vault.collateral.decimals);
