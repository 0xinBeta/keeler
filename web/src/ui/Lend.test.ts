import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, test } from "vitest";
import type { Vault } from "../keeler/index";
import { EmptiedNote, HealthBar, LiquidatedNote, OpSteps, healthState, kindLabel } from "./Lend";

const tUSD = { mint: "u", symbol: "tUSD", decimals: 6 };
const tSOL = { mint: "s", symbol: "tSOL", decimals: 9 };
const vault: Vault = {
  id: 1, pool: "u", collateral: tSOL, debt: tUSD,
  collateralFactorBps: 8000, liquidationThresholdBps: 8500, liquidationMaxLimitBps: 9000, penaltyBps: 100,
  minCollateral: 0n, minDebt: 0n, price: 142_370_000_000n, confidence: 0n, priceTime: 0,
};

test("health states follow the borrow limit and the keeper's liquidation test", () => {
  const SOL = 1_000_000_000n;
  expect(healthState(SOL, 99_000000n, vault)).toBe("healthy");
  expect(healthState(SOL, 113_896000n, vault)).toBe("healthy");
  expect(healthState(SOL, 117_000000n, vault)).toBe("close");
  expect(healthState(SOL, 121_500000n, vault)).toBe("close");
  expect(healthState(SOL, 124_000000n, vault)).toBe("liquidatable");
  expect(healthState(0n, 1n, vault)).toBe("liquidatable");
});

test("the health bar is a labelled meter that draws the state it is given", () => {
  const html = renderToStaticMarkup(createElement(HealthBar, { health: 8700, state: "liquidatable", vault, after: { health: 7000, state: "healthy" } }));
  expect(html).toContain('role="meter"');
  expect(html).toContain('aria-valuetext="87.0%, liquidatable"');
  expect(html).toContain("health-liquidatable");
  expect(html).toContain("70.0%");
  expect(html).toContain("85% liquidation");
  const band = renderToStaticMarkup(createElement(HealthBar, { health: 8534, state: "close", vault }));
  expect(band).toContain('aria-valuetext="85.3%, close to liquidation"');
  expect(band).toContain("health-close");
});

test("the liquidation note says what the liquidation left, not what a later repayment did", () => {
  const lastLiquidation = { at: 100, debtRepaid: 18_113930n, collateralSold: 137_771696n, price: 132_792_660_000n, collateralLeft: 362_228304n, debtLeft: 40_886072n };
  const later = renderToStaticMarkup(createElement(LiquidatedNote, { position: { vault: 1, id: "42", collateral: 362_228304n, debt: 0n, lastLiquidation }, vault, now: 130 }));
  expect(later).toContain("30 s ago");
  expect(later).not.toContain("whole loan");
  expect(later).toContain("at 132.79 tUSD. It brought the position back to 85.0% at that price, and the rest stayed yours.");
  const partial = renderToStaticMarkup(createElement(LiquidatedNote, { position: { vault: 1, id: "42", collateral: 3_000000000n, debt: 0n, lastLiquidation: { ...lastLiquidation, price: 120_000_000_000n, collateralLeft: 3_000000000n, debtLeft: 324_720000n } }, vault, now: 130 }));
  expect(partial).toContain("back to 90.2% at that price");
  const full = renderToStaticMarkup(createElement(LiquidatedNote, { position: { vault: 1, id: "42", collateral: 1n, debt: 0n, lastLiquidation: { ...lastLiquidation, debtLeft: 0n } }, vault, now: 130 }));
  expect(full).toContain("It repaid the whole loan");
});

test("a full liquidation that left nothing says so", () => {
  const lastLiquidation = { at: 100, debtRepaid: 100_000000n, collateralSold: 4_000000000n, price: 120_000_000_000n, collateralLeft: 0n, debtLeft: 0n };
  const html = renderToStaticMarkup(createElement(EmptiedNote, { position: { vault: 1, id: "42", collateral: 0n, debt: 0n, lastLiquidation }, vault, now: 100 }));
  expect(html).toContain("Your position was liquidated in full");
  expect(html).toContain("no collateral and no debt");
  expect(html).toContain('role="status"');
});

test("a draw is named by what it moves", () => {
  const draw = (assets: (typeof tUSD)[]) => kindLabel({ kind: "draw", amounts: assets.map((asset) => ({ asset, amount: 1n })) }, tSOL);
  expect(draw([tUSD])).toBe("Borrow");
  expect(draw([tSOL])).toBe("Withdraw collateral");
  expect(draw([tUSD, tSOL])).toBe("Borrow and withdraw");
});

test("steps: an intent waits for the keeper, and an unfilled one says so", () => {
  const waiting = renderToStaticMarkup(createElement(OpSteps, { kind: "supply", step: "keeper", status: "pending" }));
  expect(waiting).toMatch(/step-done[^]*Proving in your browser[^]*step-done[^]*Sign in your wallet[^]*step-now[^]*Waiting for the keeper/);
  const stalled = renderToStaticMarkup(createElement(OpSteps, { kind: "supply", step: "keeper", status: "refundable" }));
  expect(stalled).toContain("The keeper did not take it in");
  const draw = renderToStaticMarkup(createElement(OpSteps, { kind: "draw", step: "landed", status: "landed" }));
  expect(draw).not.toContain("Proving");
  expect(draw.match(/step-done/g)).toHaveLength(3);
});
