import { expect, test } from "vitest";
import type { Balance, Pool, Vault } from "../keeler/index";
import { readFileSync } from "node:fs";
import { PRICE_SCALE, canDraw, collateralValue, isLiquidatable, liquidate } from "../keeler/lend-math";
import {
  KEEPER_MARGIN_BPS, KEEPER_MIN_REPAY, borrowLimits, drawPrice, earnLimits, grow, healthBps, keeperLiquidates, lendingShown, liquidatable,
  liquidationBound, liquidationPrice, maxBorrow, maxWithdraw, pct, priceFromWhole, priceText,
} from "./lend-view";

const tUSD = { mint: "u", symbol: "tUSD", decimals: 6 };
const tSOL = { mint: "s", symbol: "tSOL", decimals: 9 };
const P = 142_370_000_000n; // 142.37 tUSD per tSOL, 1e12-scaled base units
const vault: Vault = {
  id: 1, pool: "u", collateral: tSOL, debt: tUSD,
  collateralFactorBps: 8000, liquidationThresholdBps: 8500, liquidationMaxLimitBps: 9000, penaltyBps: 100,
  minCollateral: 0n, minDebt: 0n, price: P, confidence: 0n, priceTime: 0,
};
const V = { cfBps: 8000n, ltBps: 8500n, lmlBps: 9000n, penaltyBps: 100n };
const SOL = 1_000_000_000n;
const USD = 1_000_000n;

test("health is debt × 10000 / collateral value rounded up, with the edges named", () => {
  expect(healthBps(4n * SOL, 400n * USD, P)).toBe(7024); // 400 / 569.48 = 70.239%
  expect(healthBps(4n * SOL, 0n, P)).toBe(0);
  expect(healthBps(0n, 1n, P)).toBe(Infinity);
});

const at = (price: bigint, confidence: bigint): Vault => ({ ...vault, price, confidence });
const DEVNET = at(132_000_000_000n, 132_000_000n);

test("the keeper values a liquidation at price + confidence plus its margin, rounded up", () => {
  expect(liquidationBound(DEVNET)).toBe(132_792_660_000n);
  expect(liquidationBound(at(P, 0n))).toBe(143_081_850_000n);
  expect(liquidationBound(at(1n, 0n))).toBe(2n);
});

test("liquidatable is the keeper's own test at its bound, on the recorded devnet position", () => {
  expect(liquidatable(500_000_000n, 59_000_002n, DEVNET)).toBe(true);
  const [c, d] = [362_228_304n, 40_886_072n];
  expect(healthBps(c, d, DEVNET.price)).toBe(8552);
  expect(healthBps(c, d, liquidationBound(DEVNET))).toBe(8501);
  expect(liquidate(d, PRICE_SCALE, c, liquidationBound(DEVNET), V).repay).toBe(6n);
  expect(keeperLiquidates(c, d, DEVNET)).toBe(false);
  expect(liquidatable(c, d, DEVNET)).toBe(false);
  const lp = liquidationPrice(c, d, DEVNET)!;
  expect(lp).toBe(132_000_005_450n);
  expect(priceText(lp, DEVNET)).toBe("132");
});

test("past the max limit a tiny repayment is still worth it; under it, it is not", () => {
  expect(healthBps(40_000n, 5000n, liquidationBound(DEVNET))).toBeGreaterThan(9000);
  expect(liquidate(5000n, PRICE_SCALE, 40_000n, liquidationBound(DEVNET), V).repay).toBeLessThan(KEEPER_MIN_REPAY);
  expect(liquidatable(40_000n, 5000n, DEVNET)).toBe(true);
  expect(healthBps(43_000n, 5000n, liquidationBound(DEVNET))).toBeGreaterThan(8500);
  expect(healthBps(43_000n, 5000n, liquidationBound(DEVNET))).toBeLessThanOrEqual(9000);
  expect(liquidatable(43_000n, 5000n, DEVNET)).toBe(false);
});

test("no debt is never liquidatable; debt the page cannot value always is", () => {
  expect(liquidatable(4n * SOL, 0n, DEVNET)).toBe(false);
  expect(liquidatable(0n, 0n, at(0n, 0n))).toBe(false);
  expect(liquidatable(0n, 1n, DEVNET)).toBe(true);
  expect(liquidatable(4n * SOL, 1n, at(0n, 0n))).toBe(true);
  expect(keeperLiquidates(4n * SOL, 1n, at(0n, 0n))).toBe(false);
  expect(liquidationPrice(4n * SOL, 0n, DEVNET)).toBeNull();
  expect(liquidationPrice(0n, 1n, DEVNET)).toBeNull();
});

test("the liquidation price is where the bound reaches the threshold; the minimum repayment lags it by under a cent", () => {
  const CENT = 10_000_000n;
  for (const conf of [0n, 132_000_000n, 1_000_000_000n]) {
    const lp = liquidationPrice(4n * SOL, 400n * USD, at(P, conf))!;
    for (let price = lp - 3n; price <= lp + 3n; price++)
      expect(isLiquidatable(400n * USD, collateralValue(4n * SOL, liquidationBound(at(price, conf))), V)).toBe(price <= lp);
    expect(liquidatable(4n * SOL, 400n * USD, at(lp + 1n, conf))).toBe(false);
    expect(liquidatable(4n * SOL, 400n * USD, at(lp - CENT, conf))).toBe(true);
  }
  expect(priceText(liquidationPrice(4n * SOL, 400n * USD, at(P, 0n))!, vault)).toBe("117.06");
  expect(liquidationPrice(4n * SOL, 400n * USD, at(P, 200n * 1_000_000_000n))).toBe(0n);
});

test("the keeper's margin and minimum repayment are the tracked deployment's", () => {
  for (const f of ["dealer.json", "standby.json"]) {
    const limits = JSON.parse(readFileSync(new URL(`../../../deploy/devnet/${f}`, import.meta.url), "utf8")).lend.limits;
    expect(BigInt(limits.bound_margin_bps)).toBe(KEEPER_MARGIN_BPS);
    expect(BigInt(limits.min_repay)).toBe(KEEPER_MIN_REPAY);
  }
});

test("borrow and withdraw limits are exactly where canDraw stops", () => {
  const most = maxBorrow(4n * SOL, 0n, P, 8000);
  expect(most).toBe(455_584_000n);
  expect(canDraw(most, collateralValue(4n * SOL, P), V)).toBe(true);
  expect(canDraw(most + 1n, collateralValue(4n * SOL, P), V)).toBe(false);
  expect(maxBorrow(4n * SOL, 500n * USD, P, 8000)).toBe(0n);
  const out = maxWithdraw(4n * SOL, 400n * USD, P, 8000);
  expect(canDraw(400n * USD, collateralValue(4n * SOL - out, P), V)).toBe(true);
  expect(canDraw(400n * USD, collateralValue(4n * SOL - out - 1n, P), V)).toBe(false);
  expect(maxWithdraw(4n * SOL, 0n, P, 8000)).toBe(4n * SOL);
  expect(maxWithdraw(4n * SOL, 1n, 0n, 8000)).toBe(0n);
});

test("display growth, percentages and prices", () => {
  expect(grow(1000n * USD, 1000, 31_536_000)).toBe(1100n * USD);
  expect(grow(1000n * USD, 1000, 0)).toBe(1000n * USD);
  expect(pct(7023)).toBe("70.2%");
  expect(pct(Infinity)).toBe("—");
  expect(priceText(priceFromWhole(120n * USD, vault), vault)).toBe("120");
});

test("earn limits: one note to deposit, the largest of this pool's receipts to withdraw", () => {
  const pool = { id: "u", asset: tUSD } as Pool;
  const balances = [{ asset: tUSD, public: 0n, private: 900n * USD, largestNote: 600n * USD }] as Balance[];
  const receipts = [
    { pool: "u", shares: 1n, value: 300n * USD },
    { pool: "u", shares: 1n, value: 500n * USD },
    { pool: "u", shares: 0n, value: 0n },
    { pool: "other", shares: 1n, value: 900n * USD },
  ];
  const l = earnLimits(pool, receipts, balances);
  expect(l.note).toBe(600n * USD);
  expect(l.largestReceipt).toBe(500n * USD);
  expect(l.mine).toHaveLength(2);
  expect(earnLimits(pool, null, null)).toEqual({ mine: [], largestReceipt: 0n, note: null });
});

test("borrow limits per action match the note and canDraw limits", () => {
  const balances = [
    { asset: tSOL, public: 0n, private: 5n * SOL, largestNote: 3n * SOL },
    { asset: tUSD, public: 0n, private: 0n, largestNote: 0n },
  ] as Balance[];
  const l = borrowLimits(vault, 4n * SOL, 100n * USD, balances);
  expect(l.add).toBe(3n * SOL);
  expect(l.repay).toBe(0n);
  expect(l.borrow).toBe(maxBorrow(4n * SOL, 100n * USD, drawPrice(vault), 8000));
  expect(l.withdraw).toBe(maxWithdraw(4n * SOL, 100n * USD, drawPrice(vault), 8000));
  expect(borrowLimits(vault, 0n, 0n, null)).toEqual({ add: null, borrow: 0n, repay: null, withdraw: 0n });
});

test("lending is presented from the build until the config is read, then from the client", () => {
  expect(lendingShown(false, false, true)).toBe(true);
  expect(lendingShown(false, false, false)).toBe(false);
  expect(lendingShown(true, false, true)).toBe(false);
  expect(lendingShown(true, true, false)).toBe(true);
});
