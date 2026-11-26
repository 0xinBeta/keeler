import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, test, vi } from "vitest";
import type { LendOperation, Order } from "../keeler/index";

vi.stubEnv("VITE_NETWORK", "devnet");
const { orbTx } = await import("../keeler/network");
const { OpLinks } = await import("./Lend");
const { OrderLinks } = await import("./Orders");

const tUSD = { mint: "u", symbol: "tUSD", decimals: 6 };
const order: Order = { id: "o", sell: tUSD, buy: tUSD, sellAmount: 1n, buyAmount: 1n, expiry: 0, status: "locked" };
const op: LendOperation = { id: "i", kind: "supply", pool: "u", vault: null, positionId: null, amounts: [], step: "keeper", status: "pending", expiry: 9, createdAt: 0 };

test("an Orb link is built on devnet only", () => {
  expect(orbTx("abc", "devnet")).toBe("https://orbmarkets.io/tx/abc?cluster=devnet");
  expect(orbTx("abc")).toBe("https://orbmarkets.io/tx/abc?cluster=devnet");
  for (const network of ["mainnet", "local", ""]) expect(orbTx("abc", network)).toBeUndefined();
});

test("an operation with no landed transaction shows no link and no sentence", () => {
  expect(renderToStaticMarkup(createElement(OrderLinks, { order }))).toBe("");
  expect(renderToStaticMarkup(createElement(OpLinks, { op }))).toBe("");
});

test("a link opens Orb in a new tab, says so, and sits beside what is public", () => {
  const out = renderToStaticMarkup(createElement(OrderLinks, { order: { ...order, signature: "make" } }));
  expect(out).toContain('href="https://orbmarkets.io/tx/make?cluster=devnet"');
  expect(out).toContain('target="_blank"');
  expect(out).toContain('rel="noopener noreferrer"');
  expect(out).toContain("View order on Orb<span class=\"sr-only\"> (opens in a new tab)</span>");
  expect(out).toContain("The amounts and the price are not.");
});

test("what spent an order is labelled from its status", () => {
  expect(renderToStaticMarkup(createElement(OrderLinks, { order: { ...order, status: "settled", spentBy: "s" } }))).toContain("View settlement on Orb");
  expect(renderToStaticMarkup(createElement(OrderLinks, { order: { ...order, status: "refunded", spentBy: "s" } }))).toContain("View refund on Orb");
});

test("what spent an intent is the keeper's fill when it landed and the owner's refund when refunded", () => {
  const landed = renderToStaticMarkup(createElement(OpLinks, { op: { ...op, status: "landed", signature: "intent", spentBy: "fill" } }));
  expect(landed).toContain("View request on Orb");
  expect(landed).toContain("View keeper&#x27;s fill on Orb");
  const refunded = renderToStaticMarkup(createElement(OpLinks, { op: { ...op, status: "refunded", signature: "intent", spentBy: "cancel" } }));
  expect(refunded).toContain("View refund on Orb");
  expect(refunded).not.toContain("fill");
  expect(renderToStaticMarkup(createElement(OpLinks, { op: { ...op, kind: "draw", status: "landed", signature: "built" } }))).toContain("View transaction on Orb");
});
