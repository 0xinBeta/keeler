// What the lending screens show, on top of the shared maths (keeler/lend-math.ts, which matches the program, and
// keeler/lend-keeper.ts, the keeper's bounds and limits): the interface's units (bps as numbers, Infinity for debt
// against nothing) and price text. No formula of its own beyond the limits the program's `canDraw` and the keeper's
// bounds imply.
import type { Asset, Balance, Pool, Receipt, Vault } from "../keeler/index";
import { KEEPER_MARGIN_BPS } from "../keeler/lend-keeper";
import * as L from "../keeler/lend-math";
import { formatUnits } from "./format";

/** Loan-to-value in basis points, rounded up as the program does; 0 without debt, Infinity without collateral. */
export function healthBps(collateral: bigint, debt: bigint, price: bigint): number {
  const h = L.healthBps(debt, L.collateralValue(collateral, price));
  return h === null ? Infinity : Number(h);
}

/** Whether the page presents lending: the client decides once the config is read; before that the build does, so the first paint does not change. */
export const lendingShown = (configRead: boolean, lend: boolean, compiled: boolean) => (configRead ? lend : lend || compiled);

/**
 * The oracle price at or below which the keeper's liquidation bound reaches the threshold, at the vault's current
 * confidence; null without debt or collateral, never below 0. It leaves out the keeper's minimum repayment: just
 * under this price a liquidation would repay less than that, so the keeper waits. The gap is about
 * min_repay × (1 − lt × (1 + penalty)) / lt, divided by the collateral in whole tokens: at devnet's settings, a tSOL
 * price gap of 0.0017 tUSD divided by the collateral in tSOL.
 */
export function liquidationPrice(collateral: bigint, debt: bigint, vault: Vault): bigint | null {
  const at = L.liquidationPrice(debt, collateral, BigInt(vault.liquidationThresholdBps));
  if (at === null) return null;
  const high = (at * L.BPS) / (L.BPS + KEEPER_MARGIN_BPS);
  return high > vault.confidence ? high - vault.confidence : 0n;
}

/** The price a draw is checked at: the low end of the oracle's band, less the keeper's margin. Never below 0. */
export function drawPrice(vault: Vault): bigint {
  const low = vault.price > vault.confidence ? vault.price - vault.confidence : 0n;
  return (low * (L.BPS - KEEPER_MARGIN_BPS)) / L.BPS;
}

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

/** The largest private note of `asset`: what one spend can use. Null while the balances are unread. */
export const largestNote = (balances: readonly Balance[] | null, asset: Asset) => balances?.find((b) => b.asset.mint === asset.mint)?.largestNote ?? null;

/**
 * Earn's limits: one deposit spends one private note (`note`); one withdrawal spends one deposit receipt, so the
 * largest receipt of this pool caps it (`largestReceipt`). `mine`: this pool's receipts that still hold shares.
 */
export function earnLimits(pool: Pool, receipts: readonly Receipt[] | null, balances: readonly Balance[] | null) {
  const mine = receipts?.filter((r) => r.pool === pool.id && r.shares > 0n) ?? [];
  const largestReceipt = mine.reduce((m, r) => (r.value > m ? r.value : m), 0n);
  return { mine, largestReceipt, note: largestNote(balances, pool.asset) };
}

/** Borrow's limit for each action: one note for adding collateral or repaying, `canDraw` for borrowing or withdrawing. */
export function borrowLimits(vault: Vault, collateral: bigint, debt: bigint, balances: readonly Balance[] | null) {
  const cf = vault.collateralFactorBps;
  return {
    add: largestNote(balances, vault.collateral),
    borrow: maxBorrow(collateral, debt, drawPrice(vault), cf),
    repay: largestNote(balances, vault.debt),
    withdraw: maxWithdraw(collateral, debt, drawPrice(vault), cf),
  };
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
