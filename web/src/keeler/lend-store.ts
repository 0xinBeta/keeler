// Lend records in localStorage, one key per record, scoped by network, lend program and wallet. They hold note
// openings (amount, blinding, state), as the swap's order records do; no key material.
import type { Liquidation, LendOperation } from "./index";
import type { RawOutput } from "./swap-wasm";
import { decodeJson, encodeJson } from "./storage";

/** A note the user's PDA owns: enough to find it on chain, check it and spend it. */
export type NoteOpening = {
  hash: Uint8Array;
  mint: string;
  amount: bigint;
  blinding: Uint8Array;
  /** Borsh state (`ReceiptState`, `PositionState`). */
  state: Uint8Array;
  kind: number;
};

export type ReceiptRecord = { pool: string; shares: bigint; note: NoteOpening };
export type PositionRecord = { vault: number; id: string; collateral: bigint; debtShares: bigint; note: NoteOpening; lastLiquidation: Liquidation | null };

export type OpRecord = LendOperation & {
  /** The signed transaction, saved before it is sent. */
  tx?: { signature: string; lastValidBlockHeight: bigint };
  /** Found on chain by a rescan: its transaction landed. */
  onChain?: boolean;
  /** An intent's opening, to find and refund it. */
  intent?: { hash: Uint8Array; output: RawOutput; purpose: number; target: bigint };
  /** The note a keeper-built operation or close spends, and what it leaves for the owner once it lands. */
  spends?: Uint8Array;
  creates?: { receipt?: ReceiptRecord; position?: PositionRecord };
};

/** The last price index this page saw per pool, 1e12-scaled, and the chain time it held at. */
export type IndexRecord = { pool: string; supply?: { index: bigint; at: number }; borrow?: { index: bigint; at: number } };

type Kinds = { op: OpRecord; receipt: ReceiptRecord; position: PositionRecord; index: IndexRecord };
export type LendStore = ReturnType<typeof lendStore>;

const PREFIX = "keeler:lend:v1:";

export function lendStore(network: string, programId: string, wallet: string, storage: Storage = localStorage) {
  const scope = `${PREFIX}${network}:${programId}:${wallet}:`;
  return {
    /** Writes and reads back; throws if the record did not persist exactly. */
    put<K extends keyof Kinds>(kind: K, id: string, v: Kinds[K]): void {
      const key = `${scope}${kind}:${id}`;
      const text = encodeJson(v);
      storage.setItem(key, text);
      if (storage.getItem(key) !== text) throw new Error("the lending record did not persist");
    },
    get<K extends keyof Kinds>(kind: K, id: string): Kinds[K] | undefined {
      const text = storage.getItem(`${scope}${kind}:${id}`);
      return text === null ? undefined : decodeJson<Kinds[K]>(text);
    },
    remove(kind: keyof Kinds, id: string): void {
      storage.removeItem(`${scope}${kind}:${id}`);
    },
    all<K extends keyof Kinds>(kind: K): Kinds[K][] {
      const out: Kinds[K][] = [];
      const head = `${scope}${kind}:`;
      for (let i = 0; i < storage.length; i++) {
        const key = storage.key(i);
        const text = key?.startsWith(head) ? storage.getItem(key) : null;
        if (text === null) continue;
        try {
          out.push(decodeJson<Kinds[K]>(text));
        } catch {
          // A damaged record is skipped, not fatal for the others.
        }
      }
      return out;
    },
  };
}
