import { describe, expect, it } from "vitest";
import { decodeRecord, encodeRecord, orderStore, type OrderRecord } from "../src/keeler/storage";

/** A Map-backed Storage; `failWrites` makes setItem silently drop, as a full quota sometimes does. */
function memory(failWrites = false): Storage {
  const m = new Map<string, string>();
  return {
    get length() {
      return m.size;
    },
    key: (i) => [...m.keys()][i] ?? null,
    getItem: (k) => m.get(k) ?? null,
    setItem: (k, v) => {
      if (!failWrites) m.set(k, v);
    },
    removeItem: (k) => void m.delete(k),
    clear: () => m.clear(),
  };
}

const asset = { mint: "So11111111111111111111111111111111111111112", symbol: "SOL", decimals: 9, maxSell: 5n };
const record = (id: string, createdAt = 1): OrderRecord => ({
  id,
  dealer: "D",
  signature: "ab".repeat(64),
  sell: asset,
  buy: asset,
  sellAmount: 18_446_744_073_709_551_615n,
  buyAmount: 1n,
  expiry: 1_763_900_030,
  status: "locked",
  createdAt,
  opening: {
    orderHash: Uint8Array.from({ length: 32 }, (_, i) => i),
    output: { asset: { asset: "x", assetId: 3n }, amount: 9n, blinding: new Uint8Array([0, 255]), data: { records: [{ kind: "utxoData", bytes: new Uint8Array([1]) }] } },
  },
});

describe("order records", () => {
  it("round-trip bigints and bytes exactly", () => {
    const r = record("a");
    expect(decodeRecord(encodeRecord(r))).toEqual(r);
  });

  it("put refuses when the write did not persist", () => {
    expect(() => orderStore("local", "W", memory(true)).put(record("a"))).toThrow(/did not persist/);
  });

  it("keep wallets and networks apart, newest first, one key per order", () => {
    const s = memory();
    const mine = orderStore("local", "W", s);
    mine.put(record("old", 1));
    mine.put(record("new", 2));
    orderStore("devnet", "W", s).put(record("other-net"));
    orderStore("local", "V", s).put(record("other-wallet"));
    expect(mine.all().map((r) => r.id)).toEqual(["new", "old"]);
    expect(s.length).toBe(4);
  });

  it("load a record saved before the spending transaction was kept, without it", () => {
    const s = memory();
    const mine = orderStore("local", "W", s);
    const old: OrderRecord = { ...record("old"), status: "settled", tx: { signature: "make", lastValidBlockHeight: 9n } };
    s.setItem("keeler:order:v1:local:W:old", encodeRecord(old));
    expect(mine.get("old")).toEqual(old);
    expect(mine.get("old")!.spentBy).toBeUndefined();
    mine.put({ ...old, spentBy: "settle" });
    expect(mine.get("old")!.spentBy).toBe("settle");
  });

  it("skip a damaged record and keep the rest", () => {
    const s = memory();
    const mine = orderStore("local", "W", s);
    mine.put(record("good"));
    s.setItem("keeler:order:v1:local:W:bad", "{not json");
    expect(mine.all().map((r) => r.id)).toEqual(["good"]);
  });
});
