import { expect, test } from "vitest";
import type { Vault } from "../../keeler/index";
import { drawPrice, grow, healthBps, maxBorrow } from "../lend-view";
import { amountFraction, borrowModel, earnModel, fallbackFrames, flightStops, ltvFraction, receiptGate, swapModel } from "./model";
import type { BorrowInput, EarnInput, LendOpInput, SwapInput } from "./model";

const tUSD = { mint: "u", symbol: "tUSD", decimals: 6 };
const tSOL = { mint: "s", symbol: "tSOL", decimals: 9 };
const SOL = 1_000_000_000n;
const USD = 1_000_000n;
const vault: Vault = {
  id: 1, pool: "u", collateral: tSOL, debt: tUSD,
  collateralFactorBps: 8000, liquidationThresholdBps: 8500, liquidationMaxLimitBps: 9000, penaltyBps: 100,
  minCollateral: 0n, minDebt: 0n, price: 142_370_000_000n, confidence: 0n, priceTime: 0,
};
const noOp: LendOpInput = { run: null, live: null, submitted: null };
const earn = (x: Partial<EarnInput> = {}): EarnInput => ({ mode: "in", parsed: 250n * USD, ok: true, limit: 1000n * USD, ...noOp, ...x });
const borrow = (x: Partial<BorrowInput> = {}): BorrowInput => ({
  action: "borrow", parsed: 100n * USD, ok: true, limit: 400n * USD, collateral: tSOL, health: 5000, projected: 6200, ...noOp, ...x,
});
const dealers = [{ pubkey: "D1" }, { pubkey: "D2" }, { pubkey: "D3" }];
const swap = (x: Partial<SwapInput> = {}): SwapInput => ({
  parsed: 2n * SOL, ok: true, limit: 5n * SOL, base: "s", sell: "s", dealers, view: { kind: "form" }, orders: [], submitted: null, ...x,
});
const running = (kind: NonNullable<LendOpInput["run"]>["kind"], amounts = [{ asset: tUSD, amount: 100n * USD }]) => ({ kind, amounts, id: "op1", failed: null });

test("fraction: the validated amount over the limit, clamped and rounded to 1/1000", () => {
  expect(earnModel(earn())).toEqual({ tab: "earn", mode: "supply", phase: "edit", fraction: 0.25, valid: true });
  expect(amountFraction(1n, 3n)).toBe(0.333);
  expect(amountFraction(2n, 3n)).toBe(0.667);
  expect(amountFraction(5n, 3n)).toBe(1);
  expect(amountFraction(10n ** 30n, 10n ** 30n + 1n)).toBe(1);
  expect(earnModel(earn({ parsed: 2000n * USD, ok: false }))).toMatchObject({ fraction: 0, valid: false });
  expect(earnModel(earn({ parsed: null, ok: true }))).toMatchObject({ fraction: 0, valid: false });
});

test("a null or zero limit gives fraction 0, never a division by zero", () => {
  expect(earnModel(earn({ limit: null }))).toMatchObject({ fraction: 0, valid: true });
  expect(earnModel(earn({ limit: 0n }))).toMatchObject({ fraction: 0, valid: true });
  expect(amountFraction(1n, 0n)).toBe(0);
  expect(amountFraction(null, 5n)).toBe(0);
});

test("ltv: whole basis points, clamped to 1.25; Infinity (debt against nothing) is 1.25", () => {
  expect(ltvFraction(7024)).toBe(0.7024);
  expect(ltvFraction(7024.4)).toBe(0.7024);
  expect(ltvFraction(Infinity)).toBe(1.25);
  expect(ltvFraction(20_000)).toBe(1.25);
  expect(ltvFraction(-3)).toBe(0);
  expect(ltvFraction(NaN)).toBe(0);
  expect(borrowModel(borrow({ health: Infinity, ok: false }))).toMatchObject({ ltv: 1.25, valid: false });
});

test("borrow: the projected ltv for a valid amount, the current one otherwise; actions map to modes", () => {
  expect(borrowModel(borrow())).toEqual({ tab: "borrow", mode: "draw", phase: "edit", fraction: 0.25, valid: true, ltv: 0.62 });
  expect(borrowModel(borrow({ ok: false }))).toEqual({ tab: "borrow", mode: "draw", phase: "edit", fraction: 0, valid: false, ltv: 0.5 });
  expect(borrowModel(borrow({ action: "add" })).mode).toBe("add");
  expect(borrowModel(borrow({ action: "repay" })).mode).toBe("repay");
  expect(borrowModel(borrow({ action: "withdraw" })).mode).toBe("remove");
});

test("a one-second interest tick gives an identical model", () => {
  const c = 4n * SOL;
  const debt = 400n * USD;
  const rate = 950;
  const at = (secs: number): BorrowInput => {
    const d = grow(debt, rate, secs);
    const amount = 30n * USD;
    return borrow({ parsed: amount, limit: maxBorrow(c, d, drawPrice(vault), 8000), health: healthBps(c, d, vault.price), projected: healthBps(c, d + amount, vault.price) });
  };
  expect(borrowModel(at(1))).toEqual(borrowModel(at(0)));
  expect(borrowModel(at(11))).toEqual(borrowModel(at(10)));
  expect(earnModel(earn({ limit: grow(1000n * USD, 412, 1) }))).toEqual(earnModel(earn({ limit: 1000n * USD })));
});

test("after submit: in flight is review with the fraction and ltv snapshotted at submit", () => {
  const submitted = { fraction: 0.4, ltv: 0.61 };
  const op = { run: running("draw"), live: null, submitted };
  expect(borrowModel(borrow({ ...op, limit: 0n, parsed: null, ok: false }))).toEqual({
    tab: "borrow", mode: "draw", phase: "review", fraction: 0.4, valid: true, ltv: 0.61,
  });
  expect(borrowModel(borrow({ ...op, live: { id: "op1", status: "pending" } })).phase).toBe("review");
  expect(earnModel(earn({ run: running("supply"), live: null, submitted, limit: 1n }))).toMatchObject({ mode: "supply", phase: "review", fraction: 0.4 });
});

test("landed is settled with the operation's id and the confirmed ltv", () => {
  const op = { run: running("repay"), live: { id: "op1", status: "landed" as const }, submitted: { fraction: 0.2, ltv: 0.3 } };
  expect(borrowModel(borrow({ ...op, health: 4100 }))).toEqual({
    tab: "borrow", mode: "repay", phase: "settled", fraction: 0.2, valid: true, ltv: 0.41, receiptId: "op1",
  });
  expect(earnModel(earn({ run: running("redeem"), live: { id: "op1", status: "landed" }, submitted: { fraction: 1 } }))).toMatchObject({
    mode: "redeem", phase: "settled", receiptId: "op1",
  });
});

test("refundable, refunded, failed, rejected and nothing to read are edit, invalid, fraction 0", () => {
  const idle = { phase: "edit", valid: false, fraction: 0 };
  for (const status of ["refundable", "refunded", "failed"] as const) {
    expect(earnModel(earn({ run: running("supply"), live: { id: "op1", status }, submitted: { fraction: 0.5 } }))).toMatchObject(idle);
    expect(borrowModel(borrow({ run: running("deposit"), live: { id: "op1", status }, submitted: { fraction: 0.5 } }))).toMatchObject(idle);
  }
  expect(earnModel(earn({ run: { ...running("supply"), failed: "You rejected it in your wallet." }, submitted: { fraction: 0.5 } }))).toMatchObject(idle);
  expect(earnModel(null)).toEqual({ tab: "earn", mode: "supply", ...idle });
  expect(borrowModel(null)).toEqual({ tab: "borrow", mode: "add", ...idle, ltv: 0 });
  expect(swapModel(null)).toEqual({ tab: "swap", mode: "supply", ...idle });
});

test("lending kinds map to modes; a draw is told from a withdrawal by its amounts; close moves everything", () => {
  const live = null;
  const submitted = { fraction: 0.3 };
  expect(borrowModel(borrow({ run: running("deposit"), live, submitted })).mode).toBe("add");
  expect(borrowModel(borrow({ run: running("draw", [{ asset: tSOL, amount: SOL }]), live, submitted })).mode).toBe("remove");
  expect(borrowModel(borrow({ run: running("draw", [{ asset: tUSD, amount: USD }, { asset: tSOL, amount: SOL }]), live, submitted })).mode).toBe("draw");
  expect(borrowModel(borrow({ run: running("close", [{ asset: tSOL, amount: SOL }]), live, submitted: null }))).toMatchObject({ mode: "close", fraction: 1 });
});

test("swap: direction from the pair, lane from the dealer's index, quotes and progress phases", () => {
  expect(swapModel(swap())).toEqual({ tab: "swap", mode: "supply", phase: "edit", fraction: 0.4, valid: true, sell: "base" });
  expect(swapModel(swap({ sell: "u" })).sell).toBe("quote");
  expect(swapModel(swap({ view: { kind: "quotes", selected: "D2" } }))).toMatchObject({ phase: "quotes", lane: 1, fraction: 0.4 });
  expect(swapModel(swap({ view: { kind: "quotes", selected: "D1" } })).lane).toBe(0);
  expect(swapModel(swap({ view: { kind: "quotes", selected: "D3" } }))).not.toHaveProperty("lane");
  expect(swapModel(swap({ view: { kind: "quotes", selected: null } }))).not.toHaveProperty("lane");
  const progress = (order: SwapInput["orders"][number] | null, failed: string | null = null): SwapInput["view"] => ({ kind: "progress", quote: { dealer: "D1" }, order, failed });
  const submitted = { fraction: 0.4 };
  expect(swapModel(swap({ view: progress(null), submitted, limit: 0n }))).toEqual({ tab: "swap", mode: "supply", phase: "review", fraction: 0.4, valid: true, sell: "base", lane: 0 });
  expect(swapModel(swap({ view: progress({ id: "o1", status: "locked" }), submitted })).phase).toBe("review");
});

test("swap: settled reads the refreshed order; an error, refund or failure is edit and invalid", () => {
  const view: SwapInput["view"] = { kind: "progress", quote: { dealer: "D2" }, order: { id: "o1", status: "locked" }, failed: null };
  const submitted = { fraction: 0.4 };
  expect(swapModel(swap({ view, submitted, orders: [{ id: "o1", status: "settled" }] }))).toEqual({
    tab: "swap", mode: "supply", phase: "settled", fraction: 0.4, valid: true, sell: "base", lane: 1, receiptId: "o1",
  });
  const idle = { phase: "edit", valid: false, fraction: 0 };
  expect(swapModel(swap({ view, submitted, orders: [{ id: "o1", status: "settled", error: "The payout did not match the quote." }] }))).toMatchObject(idle);
  for (const status of ["refundable", "refunded", "failed"] as const) expect(swapModel(swap({ view, submitted, orders: [{ id: "o1", status }] }))).toMatchObject(idle);
  expect(swapModel(swap({ view: { ...view, failed: "The swap stopped." }, submitted }))).toMatchObject(idle);
});

test("a receipt id animates once, across remounts of the scene that holds the gate", () => {
  const gate = receiptGate();
  const mount = () => (id: string) => gate.claim(id);
  const first = mount();
  expect(first("op1")).toBe(true);
  expect(first("op1")).toBe(false);
  const second = mount();
  expect(second("op1")).toBe(false);
  expect(second("o2")).toBe(true);
  expect(receiptGate().claim("op1")).toBe(true);
});

test("flight stops and the fallback frames crossfaded between them", () => {
  expect(flightStops).toEqual([0, 0.22, 0.45, 0.66, 0.84, 1]);
  expect(fallbackFrames(0, false)).toEqual({ from: "wide-0", to: "wide-1", mix: 0 });
  expect(fallbackFrames(0.11, true)).toEqual({ from: "tall-0", to: "tall-1", mix: 0.5 });
  expect(fallbackFrames(0.45, false)).toEqual({ from: "wide-2", to: "wide-3", mix: 0 });
  expect(fallbackFrames(1, false)).toEqual({ from: "wide-5", to: "wide-5", mix: 0 });
  expect(fallbackFrames(1.7, true)).toEqual({ from: "tall-5", to: "tall-5", mix: 0 });
  expect(fallbackFrames(-1, false).from).toBe("wide-0");
  expect(fallbackFrames(NaN, false).from).toBe("wide-0");
  const f = fallbackFrames(0.75, false);
  expect(f.from).toBe("wide-3");
  expect(f.mix).toBeCloseTo(0.5);
});
