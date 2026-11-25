import { describe, expect, it } from "vitest";
import { NEVER_LANDED, NEVER_SENT, FAILED_ON_CHAIN, TAKEN_UNMATCHED, nextRecord, refreshAll, refundRefusal, rose, type ChainReads, type SignatureState } from "../src/keeler/status";
import type { OrderRecord } from "../src/keeler/storage";
import type { ChainStatus } from "../src/keeler/swap";

const EXPIRY = 1_000;
const asset = { mint: "m", symbol: "M", decimals: 0 };
const record = (over: Partial<OrderRecord> = {}): OrderRecord => ({
  id: "o",
  dealer: "d",
  sell: asset,
  buy: asset,
  sellAmount: 1n,
  buyAmount: 1n,
  expiry: EXPIRY,
  status: "pending",
  createdAt: 0,
  opening: { orderHash: new Uint8Array(32), output: { asset: { asset: "m", assetId: 0n }, amount: 1n, blinding: new Uint8Array(32) } },
  tx: { signature: "sig", lastValidBlockHeight: 500n },
  ...over,
});

/** A chain that answers from these values; a thrown value models an RPC or indexer failure. */
function chain(o: { sig?: SignatureState | Error; height?: bigint | Error; spend?: ChainStatus | Error }): ChainReads {
  const answer = <T>(v: T | Error | undefined, name: string): Promise<T> =>
    v instanceof Error ? Promise.reject(v) : v === undefined ? Promise.reject(new Error(`unexpected ${name} read`)) : Promise.resolve(v);
  return {
    signatureStatus: () => answer(o.sig, "signature"),
    blockHeight: () => answer(o.height, "height"),
    spend: () => answer(o.spend, "spend"),
  };
}

const confirmed: SignatureState = { err: null, confirmationStatus: "confirmed" };

describe("a pending order", () => {
  it("whose send timed out but later confirmed becomes locked", async () => {
    // The client gave up waiting (the record keeps its error); the chain has it now.
    const r = record({ error: "timed out" });
    expect((await nextRecord(r, 10, chain({ height: 100n, sig: null }))).status).toBe("pending");
    const next = await nextRecord(r, 10, chain({ height: 120n, sig: confirmed, spend: "open" }));
    expect(next.status).toBe("locked");
    expect(next.error).toBeUndefined();
  });

  it("confirmed after its expiry is refundable, not failed", async () => {
    expect((await nextRecord(record(), EXPIRY + 1, chain({ height: 900n, sig: confirmed, spend: "open" }))).status).toBe("refundable");
  });

  it("with an error seen only at processed level stays pending (a dropped fork), and locks if it later confirms", async () => {
    const forked = await nextRecord(record(), 10, chain({ height: 100n, sig: { err: { InstructionError: [0, "x"] }, confirmationStatus: "processed" } }));
    expect(forked.status).toBe("pending");
    expect((await nextRecord(forked, 10, chain({ height: 120n, sig: confirmed, spend: "open" }))).status).toBe("locked");
  });

  it("stays pending while processed but not confirmed", async () => {
    expect((await nextRecord(record(), 10, chain({ height: 100n, sig: { err: null, confirmationStatus: "processed" } }))).status).toBe("pending");
  });

  it("fails only once the block height passed its last valid one with no signature seen", async () => {
    // Past expiry is not enough: the transaction could still land.
    expect((await nextRecord(record(), EXPIRY + 500, chain({ height: 500n, sig: null }))).status).toBe("pending");
    const next = await nextRecord(record(), EXPIRY + 500, chain({ height: 501n, sig: null }));
    expect(next).toMatchObject({ status: "failed", error: NEVER_LANDED });
  });

  it("fails when its transaction failed on chain", async () => {
    expect(await nextRecord(record(), 10, chain({ height: 1n, sig: { err: { InstructionError: [0, "x"] }, confirmationStatus: "confirmed" } }))).toMatchObject({
      status: "failed",
      error: FAILED_ON_CHAIN,
    });
  });

  it("without a saved signature was never sent: failed after expiry only", async () => {
    const { tx: _tx, ...unsent } = record();
    expect((await nextRecord(unsent, EXPIRY, chain({}))).status).toBe("pending");
    expect(await nextRecord(unsent, EXPIRY + 1, chain({}))).toMatchObject({ status: "failed", error: NEVER_SENT });
  });
});

describe("read errors never make an order terminal", () => {
  const down = new Error("HTTP 429");
  it("pending: height, signature or spend reads that fail throw", async () => {
    await expect(nextRecord(record(), EXPIRY + 9_999, chain({ height: down }))).rejects.toThrow("429");
    await expect(nextRecord(record(), EXPIRY + 9_999, chain({ height: 9_999n, sig: down }))).rejects.toThrow("429");
    await expect(nextRecord(record(), EXPIRY + 9_999, chain({ height: 9_999n, sig: confirmed, spend: down }))).rejects.toThrow("429");
  });
  it("locked: an indexer error throws instead of reading as open, settled or refunded", async () => {
    await expect(nextRecord(record({ status: "locked" }), EXPIRY + 1, chain({ spend: new Error("indexer down") }))).rejects.toThrow("indexer down");
  });
});

describe("a locked order", () => {
  it("follows what spent it", async () => {
    const locked = record({ status: "locked" });
    expect((await nextRecord(locked, 10, chain({ spend: "open" }))).status).toBe("locked");
    expect((await nextRecord(locked, 10, chain({ spend: "settled" }))).status).toBe("settled");
    expect((await nextRecord(locked, EXPIRY + 1, chain({ spend: "cancelled" }))).status).toBe("refunded");
  });
  it("spent by something other than its maker's cancel, with no matching payout, is settled with a plain warning, never refunded", async () => {
    expect(await nextRecord(record({ status: "locked" }), EXPIRY + 1, chain({ spend: "taken-unmatched" }))).toMatchObject({ status: "settled", error: TAKEN_UNMATCHED });
  });
  it("returns the same record when nothing changed", async () => {
    const locked = record({ status: "locked" });
    expect(await nextRecord(locked, 10, chain({ spend: "open" }))).toBe(locked);
  });
  it("final records are not read again", async () => {
    for (const status of ["settled", "refunded", "failed"] as const) {
      const r = record({ status });
      expect(await nextRecord(r, EXPIRY + 1, chain({}))).toBe(r);
    }
  });
});

describe("listing orders", () => {
  it("returns the stored records unchanged when chain time cannot be read", async () => {
    const stored = [record({ id: "a", status: "locked" }), record({ id: "b", status: "refundable" })];
    const out = await refreshAll(stored, () => Promise.reject(new Error("RPC down")), () => Promise.reject(new Error("not reached")));
    expect(out).toEqual(stored);
  });
  it("keeps each record whose own read fails, and refreshes the rest", async () => {
    const a = record({ id: "a", status: "locked" });
    const b = record({ id: "b", status: "locked" });
    const out = await refreshAll([a, b], async () => EXPIRY + 1, (r, now) => (r.id === "a" ? Promise.reject(new Error("indexer down")) : nextRecord(r, now, chain({ spend: "open" }))));
    expect(out.map((r) => [r.id, r.status])).toEqual([["a", "locked"], ["b", "refundable"]]);
  });
});

describe("refunds", () => {
  it("are refused before expiry, by chain time", async () => {
    const before = await nextRecord(record({ status: "locked" }), EXPIRY, chain({ spend: "open" }));
    expect(before.status).toBe("locked");
    expect(refundRefusal(before)).toMatch(/after its expiry/);
    const after = await nextRecord(record({ status: "locked" }), EXPIRY + 1, chain({ spend: "open" }));
    expect(after.status).toBe("refundable");
    expect(refundRefusal(after)).toBeUndefined();
  });
  it("are refused for pending and final orders", () => {
    expect(refundRefusal(record())).toMatch(/not been seen on chain/);
    expect(refundRefusal(record({ status: "settled" }))).toMatch(/nothing to refund/);
  });
});

describe("rose", () => {
  it("is true as soon as a read exceeds the starting value", async () => {
    const reads = [5n, 5n, 6n];
    let n = 0;
    expect(await rose(async () => reads[n++]!, 5n, 5, 0)).toBe(true);
    expect(n).toBe(3);
  });
  it("is false when no read exceeds it within the tries", async () => {
    let n = 0;
    expect(await rose(async () => (n++, 5n), 5n, 3, 0)).toBe(false);
    expect(n).toBe(3);
  });
});
