// Wire layouts of the lend program as the page reads and writes them, in one table so integrating the
// instructions still being built is a constants change. `built` rows match lend/program/src as merged;
// `assumed` rows are this client's assumption until the Earn and Vault pieces land (docs/lend-api.md).
//
// Instruction data is `[tag] ‖ args ‖ wincode(TransactIxData)` (lend/program/src/instructions/mod.rs).
// Every args struct starts with the 128-byte compressed proof; offsets below count from the start of args.

export const TAG = {
  intent: 2,
  cancelIntent: 3,
  supply: 4,
  redeem: 5,
  deposit: 6,
  draw: 7,
  repay: 8,
  liquidate: 9,
  close: 10,
} as const;

/** What an intent's funds are for (`circuits::purpose`). Built. */
export const PURPOSE = { supply: 1, deposit: 2, repay: 3 } as const;
export type Purpose = keyof typeof PURPOSE;

/** `ReceiptState` / `PositionState` kinds of the WASM `noteOutputHash` (`client::NoteKind`); 0 is a plain token note. Built. */
export const NOTE_KIND = { token: 0, reserve: 1, receipt: 2, position: 3, intent: 4 } as const;

const PROOF = 128;

/**
 * Byte length of each instruction's args and the offsets of the u64 fields the page checks.
 * - intent (built): proof, mint [32], viewing [33], user_bump u8.
 * - cancelIntent (built): proof, viewing [33], user_bump u8, expiry u64.
 * - redeem (assumed): proof, mint [32], pool_bump u8, user_bump u8, now u64.
 * - draw (assumed): proof, vault u64, pool_bump u8, user_bump u8, now u64, bound u64.
 * - liquidate (assumed): proof, vault u64, pool_bump u8, user_bump u8, now u64, bound u64 (only `bound` is read, to report a liquidation).
 * - close (assumed, built like intent): proof, mint [32], viewing [33], user_bump u8.
 */
export const ARGS = {
  intent: { length: PROOF + 32 + 33 + 1 },
  cancelIntent: { length: PROOF + 33 + 1 + 8, expiry: PROOF + 34 },
  redeem: { length: PROOF + 32 + 1 + 1 + 8, now: PROOF + 34 },
  draw: { length: PROOF + 8 + 1 + 1 + 8 + 8, now: PROOF + 10, bound: PROOF + 18 },
  liquidate: { length: PROOF + 8 + 1 + 1 + 8 + 8, bound: PROOF + 18 },
  close: { length: PROOF + 32 + 33 + 1 },
} as const;

/**
 * Account heads (before the shielded pool's transact accounts) of keeper-built instructions (assumed):
 * keeper (writable signer, also the fee payer), then the owner (read-only signer).
 */
export const KEEPER_HEAD = { keeper: 0, owner: 1 } as const;

/**
 * Output slots. intent (built): change 0, intent 1. cancelIntent (built): refund 0.
 * redeem (assumed, the plan's order): reserve 0, payout 1, receipt 2.
 * draw (assumed): reserve 0, position 1, debt payout 2, collateral payout 3.
 * close (assumed): collateral 0.
 * A zero amount is still an output at its slot (the plan pads every instruction to a fixed count).
 */
export const SLOT = {
  intent: { change: 0, intent: 1 },
  cancelIntent: { refund: 0 },
  redeem: { reserve: 0, payout: 1, receipt: 2 },
  draw: { reserve: 0, position: 1, debtPayout: 2, collateralPayout: 3 },
  close: { collateral: 0 },
} as const;

/**
 * Blinding seeds of the outputs an owner follows (assumed): `poseidon([domain, previous note's blinding])`, the domain
 * a big-endian u32 of four ASCII bytes in a field element, as the swap's take ("SWTX"). Each output's blinding is then
 * `outputBlinding(first nullifier, seed, slot)`. redeem seeds from the receipt, draw from the position.
 */
export const SEED_DOMAIN = { redeem: "LRDM", draw: "LDRW" } as const;

/** The tree every output goes to (the SDK indexes tree 0). */
export const TREE_ID = 0;

/** A proof plus an SPP transact: the whole budget, as for swaps. */
export const COMPUTE_UNIT_LIMIT = 1_400_000;

/** Signing-check windows, in seconds and basis points. */
export const CHECK = {
  /** A keeper `now` may trail chain time by this much (the program allows 120 s; the page keeps margin to land). */
  nowBehind: 60,
  /** ... and lead it by this much (clock skew between RPC nodes). */
  nowAhead: 10,
  /** The transact expiry must be after chain time and at most this far ahead. */
  txExpiryMax: 300,
  /** The draw price bound must be within this of the reference price. */
  boundMarginBps: 200,
  /** An intent's expiry (the keeper's ticket) must leave at least this long to fill... */
  intentMinSecs: 60,
  /** ... and lock funds for at most this long. */
  intentMaxSecs: 900,
} as const;

/** The longest a session authorisation may run (docs/lend-api.md). */
export const SESSION_MAX_SECS = 24 * 3600;

/** A big-endian u32 domain in a 32-byte field element. */
export function domainField(domain: string): Uint8Array {
  const b = new Uint8Array(32);
  b.set(new TextEncoder().encode(domain), 28);
  return b;
}

/** Reads a u64 LE at `offset` of `bytes`. */
export function u64At(bytes: Uint8Array, offset: number): bigint {
  if (offset + 8 > bytes.length) throw new Error("instruction data too short");
  return new DataView(bytes.buffer, bytes.byteOffset + offset, 8).getBigUint64(0, true);
}

export function u64Bytes(v: bigint): Uint8Array {
  if (v < 0n || v > 0xffff_ffff_ffff_ffffn) throw new RangeError("value outside u64");
  const out = new Uint8Array(8);
  new DataView(out.buffer).setBigUint64(0, v, true);
  return out;
}

/** A receipt's or position's Borsh state with its last u64 (supply shares, debt shares) replaced. */
export function withLastU64(state: Uint8Array, v: bigint): Uint8Array {
  if (state.length < 8) throw new Error("note state too short");
  const out = state.slice();
  out.set(u64Bytes(v), state.length - 8);
  return out;
}
