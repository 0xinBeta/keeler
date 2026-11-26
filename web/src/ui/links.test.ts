import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, test, vi } from "vitest";
import type { LendOperation, Order } from "../keeler/index";

vi.stubEnv("VITE_NETWORK", "devnet");
const { orbTx } = await import("../keeler/network");
const { OnChainNotes, ON_CHAIN, OrbLinks } = await import("./shared");
const { opLinks, opSays } = await import("./Lend");
const { orderLinks } = await import("./Orders");

const sig = (c: string) => c.repeat(88);
const tUSD = { mint: "u", symbol: "tUSD", decimals: 6 };
const order: Order = { id: "o", sell: tUSD, buy: tUSD, sellAmount: 1n, buyAmount: 1n, expiry: 0, status: "locked" };
const op: LendOperation = { id: "i", kind: "supply", pool: "u", vault: null, positionId: null, amounts: [], step: "keeper", status: "pending", expiry: 9, createdAt: 0 };
const links = (l: Parameters<typeof OrbLinks>[0]["links"], says?: string) => renderToStaticMarkup(createElement(OrbLinks, { links: l, says }));

test("an Orb link is built on devnet only, for a base58 signature only", () => {
  expect(orbTx(sig("5"), "devnet")).toBe(`https://orbmarkets.io/tx/${sig("5")}?cluster=devnet`);
  expect(orbTx(sig("5"))).toBe(`https://orbmarkets.io/tx/${sig("5")}?cluster=devnet`);
  for (const network of ["mainnet", "local", ""]) expect(orbTx(sig("5"), network)).toBeUndefined();
  for (const bad of ["../../address/x/history", `${sig("5").slice(0, 70)}/../x`, `${sig("5").slice(0, 70)}?a=b`, sig("0"), sig("l"), "5".repeat(63), "5".repeat(89)])
    expect(orbTx(bad, "devnet")).toBeUndefined();
});

test("an operation with no landed transaction shows no link and no sentence", () => {
  expect(links(orderLinks(order), ON_CHAIN.swap)).toBe("");
  expect(links(opLinks(op), opSays(op))).toBe("");
  expect(renderToStaticMarkup(createElement(OnChainNotes, { rows: [{ links: orderLinks(order), says: ON_CHAIN.swap }] }))).toBe("");
});

test("links open Orb in a new tab, say so, are separated, and sit above what is public", () => {
  const out = links(orderLinks({ ...order, status: "settled", makeSignature: sig("M"), spentBy: sig("S") }), ON_CHAIN.swap);
  expect(out).toContain(`href="https://orbmarkets.io/tx/${sig("M")}?cluster=devnet"`);
  expect(out).toContain('target="_blank"');
  expect(out).toContain('rel="noopener noreferrer"');
  expect(out).toContain('View order on Orb<span class="sr-only"> (opens in a new tab)</span></a><span aria-hidden="true"> · </span><a');
  expect(out).toContain(`<p class="order-note">${ON_CHAIN.swap}</p>`);
});

test("what spent an order is labelled from its status, and a take that paid nothing matchable says so", () => {
  const labels = (o: Partial<Order>) => orderLinks({ ...order, ...o }).map((l) => l.label);
  expect(labels({ status: "settled" })[1]).toBe("View settlement on Orb");
  expect(labels({ status: "refunded" })[1]).toBe("View refund on Orb");
  expect(labels({ status: "settled", error: "taken" })[1]).toBe("View the transaction that took it on Orb");
});

test("an intent's request and refund name no pool or vault; the keeper's fill and a keeper-built operation do", () => {
  const pending = { ...op, signature: sig("R") };
  expect(opSays(pending)).toBe(ON_CHAIN.request);
  const refunded = { ...op, status: "refunded" as const, signature: sig("R"), spentBy: sig("C") };
  expect(opLinks(refunded).map((l) => l.label)).toEqual(["View request on Orb", "View refund on Orb"]);
  expect(opSays(refunded)).toBe(ON_CHAIN.request);
  const filled = { ...op, status: "landed" as const, signature: sig("R"), spentBy: sig("F") };
  expect(opLinks(filled).map((l) => l.label)).toEqual(["View request on Orb", "View keeper's fill on Orb"]);
  expect(opSays(filled)).toBe(ON_CHAIN.lend);
  const drawn = { ...op, kind: "draw" as const, status: "landed" as const, expiry: null, signature: sig("D") };
  expect(opLinks(drawn)[0]!.label).toBe("View transaction on Orb");
  expect(opSays(drawn)).toBe(ON_CHAIN.lend);
});

test("a list states each sentence once, whatever the number of rows", () => {
  const rows = [1, 2, 3].map(() => ({ links: orderLinks({ ...order, makeSignature: sig("M") }), says: ON_CHAIN.swap }));
  const out = renderToStaticMarkup(createElement(OnChainNotes, { rows }));
  expect(out.split(ON_CHAIN.swap)).toHaveLength(2);
});
