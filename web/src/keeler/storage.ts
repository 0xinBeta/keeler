// Order records in localStorage, one key per order, so writing one order can
// never clobber another. Bytes and bigints survive the JSON round trip.
import type { Asset, OrderStatus } from "./index";
import type { OrderOpening } from "./swap";

export type OrderRecord = {
  /** The order hash in hex, so a record found again on chain merges with the local one. */
  id: string;
  /** The dealer's address; empty for an order found on chain from a dealer this service no longer lists. */
  dealer: string;
  /** The dealer's quote signature (128 hex) over these terms; absent for an order found on chain. */
  signature?: string;
  sell: Asset;
  buy: Asset;
  sellAmount: bigint;
  buyAmount: bigint;
  expiry: number;
  status: OrderStatus;
  error?: string;
  /** The signed make, saved before it is sent: its signature and the last block height it can land at. */
  tx?: { signature: string; lastValidBlockHeight: bigint };
  /** The transaction that spent the order once it is settled or refunded: the dealer's settle, or the maker's cancel. */
  spentBy?: string;
  createdAt: number;
  opening: OrderOpening;
};

const PREFIX = "keeler:order:v1:";

function replacer(this: Record<string, unknown>, key: string): unknown {
  const v = this[key];
  if (typeof v === "bigint") return { $n: v.toString() };
  if (v instanceof Uint8Array) return { $b: Array.from(v, (x) => x.toString(16).padStart(2, "0")).join("") };
  return v;
}

function reviver(_key: string, v: unknown): unknown {
  if (v && typeof v === "object") {
    const o = v as Record<string, unknown>;
    if (typeof o.$n === "string" && Object.keys(o).length === 1) return BigInt(o.$n);
    if (typeof o.$b === "string" && Object.keys(o).length === 1) {
      return Uint8Array.from(o.$b.match(/../g) ?? [], (b) => parseInt(b, 16));
    }
  }
  return v;
}

/** JSON that keeps bigints and bytes; the lend records use it too. */
export const encodeJson = (v: unknown) => JSON.stringify(v, replacer);
export const decodeJson = <T>(s: string) => JSON.parse(s, reviver) as T;
export const encodeRecord = (r: OrderRecord) => encodeJson(r);
export const decodeRecord = (s: string) => decodeJson<OrderRecord>(s);

/** Orders of one wallet on one network. */
export function orderStore(network: string, owner: string, storage: Storage = localStorage) {
  const scope = `${PREFIX}${network}:${owner}:`;
  return {
    /** Writes and reads back; throws if the record did not persist exactly. */
    put(r: OrderRecord): void {
      const key = scope + r.id;
      const text = encodeRecord(r);
      storage.setItem(key, text);
      if (storage.getItem(key) !== text) throw new Error("the order record did not persist");
    },
    get(id: string): OrderRecord | undefined {
      const text = storage.getItem(scope + id);
      return text === null ? undefined : decodeRecord(text);
    },
    all(): OrderRecord[] {
      const out: OrderRecord[] = [];
      for (let i = 0; i < storage.length; i++) {
        const key = storage.key(i);
        if (!key?.startsWith(scope)) continue;
        const text = storage.getItem(key);
        if (text === null) continue;
        try {
          out.push(decodeRecord(text));
        } catch {
          // A damaged record is skipped, not fatal for the others.
        }
      }
      return out.sort((a, b) => b.createdAt - a.createdAt);
    },
  };
}
