import { expect, test } from "vitest";
import type { Vault } from "../keeler/index";
import { canDraw, collateralValue } from "../keeler/lend-math";
import { grow, healthBps, liquidatable, liquidationPrice, maxBorrow, maxWithdraw, pct, priceFromWhole, priceText } from "./lend-view";

const tUSD = { mint: "u", symbol: "tUSD", decimals: 6 };
const tSOL = { mint: "s", symbol: "tSOL", decimals: 9 };
const P = 142_370_000_000n; // 142.37 tUSD per tSOL, 1e12-scaled base units
const vault: Vault = {
  id: 1, pool: "u", collateral: tSOL, debt: tUSD,
  collateralFactorBps: 8000, liquidationThresholdBps: 8500, liquidationMaxLimitBps: 9000, penaltyBps: 100,
  minCollateral: 0n, minDebt: 0n, price: P, priceTime: 0,
};
const V = { cfBps: 8000n, ltBps: 8500n, lmlBps: 9000n, penaltyBps: 100n };
const SOL = 1_000_000_000n;
const USD = 1_000_000n;

test("health is debt × 10000 / collateral value rounded up, with the edges named", () => {
  expect(healthBps(4n * SOL, 400n * USD, P)).toBe(7024); // 400 / 569.48 = 70.239%
  expect(healthBps(4n * SOL, 0n, P)).toBe(0);
  expect(healthBps(0n, 1n, P)).toBe(Infinity);
  expect(liquidatable(8500, vault)).toBe(false);
  expect(liquidatable(8501, vault)).toBe(true);
});

test("the liquidation price is the highest price at which the position is liquidatable", () => {
  const lp = liquidationPrice(4n * SOL, 400n * USD, 8500)!;
  expect(priceText(lp, vault)).toBe("117.64");
  expect(liquidatable(healthBps(4n * SOL, 400n * USD, lp), vault)).toBe(true);
  expect(liquidatable(healthBps(4n * SOL, 400n * USD, lp + 1n), vault)).toBe(false);
  expect(liquidationPrice(4n * SOL, 0n, 8500)).toBeNull();
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
