import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, test } from "vitest";
import type { Vault } from "../keeler/index";
import { HealthBar, OpSteps, healthState, kindLabel } from "./Lend";

const tUSD = { mint: "u", symbol: "tUSD", decimals: 6 };
const tSOL = { mint: "s", symbol: "tSOL", decimals: 9 };
const vault: Vault = {
  id: 1, pool: "u", collateral: tSOL, debt: tUSD,
  collateralFactorBps: 8000, liquidationThresholdBps: 8500, liquidationMaxLimitBps: 9000, penaltyBps: 100,
  minCollateral: 0n, minDebt: 0n, price: 142_370_000_000n, priceTime: 0,
};

test("health states follow the borrow limit and the threshold", () => {
  expect(healthState(7000, vault)).toBe("healthy");
  expect(healthState(8000, vault)).toBe("healthy");
  expect(healthState(8200, vault)).toBe("close");
  expect(healthState(8500, vault)).toBe("close");
  expect(healthState(8501, vault)).toBe("liquidatable");
});

test("the health bar is a labelled meter with its state and the preview", () => {
  const html = renderToStaticMarkup(createElement(HealthBar, { health: 8700, vault, after: 7000 }));
  expect(html).toContain('role="meter"');
  expect(html).toContain('aria-valuetext="87.0%, liquidatable"');
  expect(html).toContain("health-liquidatable");
  expect(html).toContain("70.0%");
  expect(html).toContain("85% liquidation");
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
