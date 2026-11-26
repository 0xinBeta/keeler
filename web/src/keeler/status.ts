// An order record's next status, from chain evidence only. A read that fails throws, so a
// failed RPC or indexer call never turns an order terminal; the record stays as it was.
import type { Order } from "./index";
import type { Spend } from "./swap";
import type { OrderRecord } from "./storage";

export type SignatureState = { err: unknown; confirmationStatus?: string | null } | null;

export type ChainReads = {
  /** `getSignatureStatuses` with history search: null when the cluster has no record of it. */
  signatureStatus(signature: string): Promise<SignatureState>;
  blockHeight(): Promise<bigint>;
  /** What spent the order's nullifier, if anything. */
  spend(r: OrderRecord): Promise<Spend>;
};

export const NEVER_SENT = "This order was never sent (the wallet did not sign it); nothing is locked.";
export const NEVER_LANDED = "The order transaction expired before it reached the chain; nothing is locked.";
export const FAILED_ON_CHAIN = "The order transaction failed on chain; nothing is locked.";
export const TAKEN_UNMATCHED =
  "The dealer took this order, but its payment to you could not be matched to your keys. Check your private balance; if the amount is missing, report it.";

const final = (r: OrderRecord) => r.status === "settled" || r.status === "refunded" || r.status === "failed";

/** The record with its status read from the chain at chain time `now`; the same object when nothing changed. */
export async function nextRecord(r: OrderRecord, now: number, chain: ChainReads): Promise<OrderRecord> {
  if (final(r)) return r;
  if (r.status === "pending") {
    // The make is sent only after `tx` is saved, so a record without it was never sent.
    if (!r.tx) return now > r.expiry ? { ...r, status: "failed", error: NEVER_SENT } : r;
    // Height first: a signature still unknown once the height passed its last valid one can never land.
    const height = await chain.blockHeight();
    const s = await chain.signatureStatus(r.tx.signature);
    if (s === null) return height > r.tx.lastValidBlockHeight ? { ...r, status: "failed", error: NEVER_LANDED } : r;
    // Confirmation first: an error seen only at processed level may be from a fork that is dropped.
    if (s.confirmationStatus !== "confirmed" && s.confirmationStatus !== "finalized") return r;
    if (s.err !== null && s.err !== undefined) return { ...r, status: "failed", error: FAILED_ON_CHAIN };
  }
  const { status: spend, spentBy } = await chain.spend(r);
  const status = spend === "settled" || spend === "taken-unmatched" ? "settled" : spend === "cancelled" ? "refunded" : now > r.expiry ? "refundable" : "locked";
  const error = spend === "taken-unmatched" ? TAKEN_UNMATCHED : undefined;
  if (status === r.status && error === r.error && spentBy === r.spentBy) return r;
  const { error: _old, spentBy: _was, ...rest } = r;
  return { ...rest, status, ...(error ? { error } : {}), ...(spentBy ? { spentBy } : {}) };
}

/** The order as the page shows it; the make's signature only once the make has landed. */
export const toOrder = (r: OrderRecord): Order => ({
  id: r.id,
  sell: r.sell,
  buy: r.buy,
  sellAmount: r.sellAmount,
  buyAmount: r.buyAmount,
  expiry: r.expiry,
  status: r.status,
  ...(r.error ? { error: r.error } : {}),
  ...(r.tx && r.status !== "pending" && r.status !== "failed" ? { makeSignature: r.tx.signature } : {}),
  ...(r.spentBy ? { spentBy: r.spentBy } : {}),
});

/**
 * Every record refreshed at chain time; never rejects. If chain time cannot be read the stored
 * records come back unchanged, and a record whose own read fails keeps its stored status, so a
 * failed read never hides an order (or its refund button) that may hold locked funds.
 */
export async function refreshAll(records: OrderRecord[], chainNow: () => Promise<number>, refresh: (r: OrderRecord, now: number) => Promise<OrderRecord>): Promise<OrderRecord[]> {
  let now: number;
  try {
    now = await chainNow();
  } catch {
    return records;
  }
  return Promise.all(records.map((r) => refresh(r, now).catch(() => r)));
}

/** Why `r` cannot be refunded now, or undefined when it can. */
export function refundRefusal(r: OrderRecord): string | undefined {
  if (r.status === "refundable") return undefined;
  if (r.status === "locked") return `The order can be refunded after its expiry, ${new Date(r.expiry * 1000).toLocaleTimeString()} (chain time).`;
  if (r.status === "pending") return "The order has not been seen on chain yet. Try again in a minute.";
  return `The order is ${r.status}; there is nothing to refund.`;
}

/** Polls `read` until it exceeds `before`; false when it has not after `tries` reads spaced `waitMs` apart. */
export async function rose(read: () => Promise<bigint>, before: bigint, tries = 30, waitMs = 2000): Promise<boolean> {
  for (let i = 0; i < tries; i++) {
    if ((await read()) > before) return true;
    if (i + 1 < tries) await new Promise((r) => setTimeout(r, waitMs));
  }
  return false;
}
