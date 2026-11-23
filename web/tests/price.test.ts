// The Rust test vectors of core/src/price.rs and core/src/rfq.rs.
import { describe, expect, it } from "vitest";
import { bestQuote, fairBuyAmount, vsFairBps, withinBand, type RawQuote } from "../src/keeler/price";

const SOL = { price: 15_000_000_000n, expo: -8 }; // $150
const USDC = { price: 100_000_000n, expo: -8 }; // $1
const one = { price: 1n, expo: 0 };
const U64_MAX = (1n << 64n) - 1n;
const I64_MAX = (1n << 63n) - 1n;

describe("fairBuyAmount", () => {
  it("negative e: SOL to USDC", () => expect(fairBuyAmount(2_000_000_000n, SOL, USDC, 9, 6)).toBe(300_000_000n));
  it("positive e: USDC to SOL", () => expect(fairBuyAmount(300_000_000n, USDC, SOL, 6, 9)).toBe(2_000_000_000n));
  it("mixed expo", () => {
    const usdc6 = { price: 1_000_000n, expo: -6 };
    expect(fairBuyAmount(2_000_000_000n, SOL, usdc6, 9, 6)).toBe(300_000_000n);
    expect(fairBuyAmount(300_000_000n, usdc6, SOL, 6, 9)).toBe(2_000_000_000n);
  });
  it("floors and rejects zero", () => {
    const three = { price: 3n, expo: 0 };
    expect(fairBuyAmount(10n, one, three, 0, 0)).toBe(3n);
    expect(fairBuyAmount(2n, one, three, 0, 0)).toBeUndefined();
    expect(fairBuyAmount(0n, one, one, 0, 0)).toBeUndefined();
  });
  it("overflow", () => {
    expect(fairBuyAmount(U64_MAX, { price: I64_MAX, expo: 0 }, one, 0, 30)).toBeUndefined();
    expect(fairBuyAmount(U64_MAX, { price: 2n, expo: 0 }, one, 0, 0)).toBeUndefined();
    expect(fairBuyAmount(1n, { price: 1n, expo: 2 ** 31 - 1 }, one, 0, 0)).toBeUndefined();
    expect(fairBuyAmount(1n, { price: 1n, expo: -(2 ** 31) }, one, 0, 0)).toBeUndefined();
  });
  it("zero and negative price", () => {
    for (const bad of [0n, -1n]) {
      const p = { price: bad, expo: 0 };
      expect(fairBuyAmount(100n, p, USDC, 6, 6)).toBeUndefined();
      expect(fairBuyAmount(100n, USDC, p, 6, 6)).toBeUndefined();
    }
  });
});

describe("withinBand", () => {
  it("edges", () => {
    expect(withinBand(9_900n, 10_000n, 100)).toBe(true);
    expect(withinBand(9_899n, 10_000n, 100)).toBe(false);
    expect(withinBand(10_100n, 10_000n, 100)).toBe(true);
    expect(withinBand(10_101n, 10_000n, 100)).toBe(false);
    expect(withinBand(10_000n, 10_000n, 0)).toBe(true);
    expect(withinBand(10_001n, 10_000n, 0)).toBe(false);
  });
  it("does not floor the threshold", () => {
    expect(withinBand(2n, 3n, 100)).toBe(false);
    expect(withinBand(3n, 3n, 100)).toBe(true);
  });
  it("bps over 10000", () => {
    expect(withinBand(0n, 10n, 10_000)).toBe(true);
    expect(withinBand(10n, 10n, 10_001)).toBe(false);
  });
  it("vsFairBps sign", () => {
    expect(vsFairBps(9_970n, 10_000n)).toBe(-30);
    expect(vsFairBps(10_020n, 10_000n)).toBe(20);
  });
});

describe("bestQuote", () => {
  const NOW = 1_000;
  const q = (buyAmount: bigint, expiry: number): RawQuote => ({ buyAmount, expiry });
  it("picks max, lowest index on tie", () => {
    expect(bestQuote([q(990n, 1_030), q(995n, 1_030), null, q(995n, 1_030)], 1_000n, 100, NOW, 10, 60)).toBe(1);
  });
  it("empty and all none", () => {
    expect(bestQuote([], 1_000n, 100, NOW, 10, 60)).toBeUndefined();
    expect(bestQuote([null, null], 1_000n, 100, NOW, 10, 60)).toBeUndefined();
  });
  it("ttl bounds", () => {
    const at = (expiry: number) => bestQuote([q(1_000n, expiry)], 1_000n, 100, NOW, 10, 60);
    expect(at(NOW + 10)).toBe(0);
    expect(at(NOW + 9)).toBeUndefined();
    expect(at(NOW + 60)).toBe(0);
    expect(at(NOW + 61)).toBeUndefined();
    expect(at(NOW)).toBeUndefined();
    expect(at(NOW - 1)).toBeUndefined();
    expect(bestQuote([q(1_000n, NOW)], 1_000n, 100, NOW, 0, 60)).toBeUndefined();
  });
  it("out of band both sides", () => {
    const quotes = [q(800n, 1_030), q(1_020n, 1_030), q(995n, 1_030)];
    expect(bestQuote(quotes, 1_000n, 100, NOW, 10, 60)).toBe(2);
    expect(bestQuote(quotes.slice(0, 2), 1_000n, 100, NOW, 10, 60)).toBeUndefined();
  });
});
