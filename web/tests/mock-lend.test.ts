import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { KEEPER_MIN_REPAY, keeperLiquidates, liquidationBound } from "../src/keeler/lend-keeper";
import { PRICE_SCALE, TSOL_TUSD_VAULT, liquidate } from "../src/keeler/lend-math";
import { createMockLend } from "../src/keeler/mock-lend";

const tUSD = { mint: "u", symbol: "tUSD", decimals: 6 };
const tSOL = { mint: "s", symbol: "tSOL", decimals: 9 };

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("window", globalThis);
});
afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

const settled = async <T>(p: Promise<T>) => {
  await vi.advanceTimersByTimeAsync(8000);
  return p;
};

test("the mock keeper liquidates at its bound, then leaves the dust a second liquidation would repay", async () => {
  const m = createMockLend(tUSD, tSOL, { scenario: null, need: () => {}, spend: () => {}, addNote: () => {}, hasNote: () => true });
  await settled(m.deposit(1, null, 4_000_000000n, () => {}));
  const id = (await settled(m.lendAccount())).positions[0]!.id;
  await settled(m.draw(id, 400_000000n, 0n, () => {}));

  m.mock.crash();
  const vault = (await settled(m.pools())).vaults[0]!;
  const first = (await settled(m.lendAccount())).positions[0]!;
  expect(first.lastLiquidation).toMatchObject({ price: liquidationBound(vault) });
  expect(first.collateral).toBeLessThan(4_000_000000n);

  m.mock.movePrice(0);
  const again = (await settled(m.lendAccount())).positions[0]!;
  const bound = liquidationBound(vault);
  expect(liquidate(again.debt, PRICE_SCALE, again.collateral, bound, TSOL_TUSD_VAULT).repay).toBeLessThan(KEEPER_MIN_REPAY);
  expect(keeperLiquidates(again.collateral, again.debt, vault)).toBe(false);
  expect(again.lastLiquidation).toEqual(first.lastLiquidation);
  expect(again.collateral).toBe(first.collateral);
});
