import { expect, test } from "vitest";
import type { Vault } from "../keeler/index";
import {
  accrue,
  borrowRateBps,
  healthBps,
  liquidatable,
  liquidationPrice,
  liquidationSize,
  maxBorrow,
  maxWithdraw,
  pct,
  priceFromWhole,
  priceText,
  supplyRateBps,
} from "./lend-math";

const tUSD = { mint: "u", symbol: "tUSD", decimals: 6 };
const tSOL = { mint: "s", symbol: "tSOL", decimals: 9 };
const P = 142_370_000_000n; // 142.37 tUSD per tSOL, 1e12-scaled base units
const vault: Vault = {
  id: 1, pool: "u", collateral: tSOL, debt: tUSD,
  collateralFactorBps: 8000, liquidationThresholdBps: 8500, liquidationMaxLimitBps: 9000, penaltyBps: 100,
  minCollateral: 0n, minDebt: 0n, price: P, priceTime: 0,
};
const SOL = 1_000_000_000n;
const USD = 1_000_000n;

test("health is debt × 10000 / collateral value, with the edges named", () => {
  expect(healthBps(4n * SOL, 400n * USD, P)).toBe(7023); // 400 / 569.48
  expect(healthBps(4n * SOL, 0n, P)).toBe(0);
  expect(healthBps(0n, 1n, P)).toBe(Infinity);
  expect(liquidatable(8500, vault)).toBe(false);
  expect(liquidatable(8501, vault)).toBe(true);
});

test("liquidation price is where health reaches the threshold", () => {
  const lp = liquidationPrice(4n * SOL, 400n * USD, 8500)!;
  expect(priceText(lp, vault)).toBe("117.64");
  expect(healthBps(4n * SOL, 400n * USD, lp)).toBe(8500);
  expect(healthBps(4n * SOL, 400n * USD, (lp * 9990n) / 10_000n)).toBeGreaterThan(8500);
  expect(liquidationPrice(4n * SOL, 0n, 8500)).toBeNull();
});

test("borrow and withdraw limits stay at the collateral factor", () => {
  const most = maxBorrow(4n * SOL, 0n, P, 8000);
  expect(most).toBe(455_584_000n);
  expect(healthBps(4n * SOL, most, P)).toBeLessThanOrEqual(8000);
  expect(maxBorrow(4n * SOL, 500n * USD, P, 8000)).toBe(0n);
  const out = maxWithdraw(4n * SOL, 400n * USD, P, 8000);
  expect(healthBps(4n * SOL - out, 400n * USD, P)).toBeLessThanOrEqual(8000);
  expect(healthBps(4n * SOL - out - 1_000_000n, 400n * USD, P)).toBeGreaterThan(8000);
  expect(maxWithdraw(4n * SOL, 0n, P, 8000)).toBe(4n * SOL);
});

test("a partial liquidation leaves the position at the threshold; past the max limit it repays everything", () => {
  const c = 10n * SOL;
  const price = priceFromWhole(120n * USD, vault); // 1200 tUSD of collateral
  const d = 1050n * USD; // 87.5%
  const s = liquidationSize(c, d, price, vault)!;
  expect(s.repaid).toBeGreaterThan(211n * USD);
  expect(s.repaid).toBeLessThan(213n * USD);
  const after = healthBps(c - s.sold, d - s.repaid, price);
  expect(after).toBeGreaterThanOrEqual(8499);
  expect(after).toBeLessThanOrEqual(8500);
  // Sold collateral covers the repayment plus the 1% penalty.
  expect((s.sold * price) / 10n ** 12n).toBeGreaterThanOrEqual((s.repaid * 101n) / 100n);

  const full = liquidationSize(c, 1100n * USD, price, vault)!; // 91.7%
  expect(full.repaid).toBe(1100n * USD);
  expect(full.sold).toBeLessThan(c);
  expect(liquidationSize(c, 1000n * USD, price, vault)).toBeNull();
});

test("rate curve, supply rate and accrual", () => {
  const curve = [
    { utilisationBps: 0, rateBps: 0 },
    { utilisationBps: 8800, rateBps: 500 },
    { utilisationBps: 9300, rateBps: 700 },
    { utilisationBps: 10_000, rateBps: 1500 },
  ];
  expect(borrowRateBps(0, curve)).toBe(0);
  expect(borrowRateBps(4400, curve)).toBe(250);
  expect(borrowRateBps(9300, curve)).toBe(700);
  expect(borrowRateBps(12_000, curve)).toBe(1500);
  expect(supplyRateBps(500, 8000, 1000)).toBe(360);
  expect(accrue(1000n * USD, 1000, 31_536_000)).toBe(1100n * USD);
  expect(accrue(1000n * USD, 1000, 0)).toBe(1000n * USD);
  expect(pct(7023)).toBe("70.2%");
  expect(pct(Infinity)).toBe("—");
});
