// What the page checks before a wallet signs a keeper-built transaction (docs/plan-lend.md, "What the page checks").
// Everything here is pure: the note hashing comes in as functions, so tests run it on hostile transactions.
import {
  AccountRole,
  address,
  decompileTransactionMessage,
  getCompiledTransactionMessageDecoder,
  getTransactionDecoder,
  type Address,
  type Transaction,
} from "@solana/kit";
import { Reader } from "@heliuslabs/spr/internal";
import { KeelerError } from "./index";
import { ARGS, CHECK, KEEPER_HEAD, TAG, u64At } from "./lend-layout";

export const COMPUTE_BUDGET_PROGRAM = address("ComputeBudget111111111111111111111111111111");

/** The parts of `wincode(TransactIxData)` the page reads (@heliuslabs/spr `encodeTransactInstructionData`, read back). */
export type Transact = {
  expiryUnixTs: bigint;
  /** The transaction viewing key and salt every output ciphertext is sealed under. */
  txViewingPk: Uint8Array;
  salt: Uint8Array;
  interfaceTransfers: number;
  outputs: Uint8Array[];
  /** Each output's inline owner tag (null for an account-index tag) and published data. */
  slots: { tag: Uint8Array | null; data?: Uint8Array }[];
  nullifiers: Uint8Array[];
};

/**
 * Decodes the transact data that follows an instruction's args; throws on anything else, trailing bytes included.
 * Only the uncached circuit kinds are read (0-3, 7): a keeper never needs the cached ones, so they are refused.
 */
export function decodeTransact(bytes: Uint8Array): Transact {
  const r = new Reader(bytes);
  const expiryUnixTs = r.u64("expiryUnixTs");
  const txViewingPk = r.bytes(33, "txViewingPk");
  const salt = r.bytes(16, "salt");
  const transfers = r.u8("interfaceTransfers.length");
  for (let i = 0; i < transfers; i++) {
    const kind = r.u8("interfaceTransfer.kind");
    r.u64("interfaceTransfer.amount");
    if (kind === 2 || kind === 3) r.u8("interfaceTransfer.splInterfaceBump");
    else if (kind > 3) throw new Error("unknown interface transfer");
  }
  r.option("dataHash", (x) => x.bytes(32, "dataHash"));
  r.option("ringDataHash", (x) => x.bytes(32, "ringDataHash"));
  const outputs: Uint8Array[] = [];
  const slots: Transact["slots"] = [];
  const outputCount = r.u8("outputs.length");
  for (let i = 0; i < outputCount; i++) {
    outputs.push(r.bytes(32, "output.utxoHash"));
    const kind = r.u8("ownerTag.kind");
    let tag: Uint8Array | null = null;
    if (kind === 0) tag = r.bytes(32, "ownerTag.value");
    else if (kind === 1) r.u8("ownerTag.index");
    else throw new Error("unknown owner tag");
    const data = r.option("output.data", (x) => x.bytes(x.u16("output.data.length"), "output.data"));
    slots.push({ tag, ...(data === undefined ? {} : { data }) });
  }
  const messages = r.u8("messages.length");
  for (let i = 0; i < messages; i++) {
    r.bytes(32, "message.viewTag");
    r.bytes(r.u16("message.data.length"), "message.data");
  }
  r.bytes(32, "privateTxHash");
  const kind = r.u16("circuit.kind");
  r.u8("circuit.inputs");
  r.u8("circuit.outputs");
  r.u8("circuit.publicAssetSlots");
  if (kind === 7) r.bytes(96, "circuit.proofData");
  else if (kind === 3) r.bytes(97, "circuit.proofData");
  else if (kind > 2) throw new Error(`circuit kind ${kind} is not one a keeper builds`);
  r.bytes(192, "proof");
  const nullifiers: Uint8Array[] = [];
  const inputs = r.u8("inputs.length");
  for (let i = 0; i < inputs; i++) {
    nullifiers.push(r.bytes(32, "input.nullifierHash"));
    r.u8("input.treeIndex");
  }
  const trees = r.u8("treeContexts.length");
  for (let i = 0; i < trees; i++) r.bytes(4, "treeContext");
  r.done();
  return { expiryUnixTs, txViewingPk, salt, interfaceTransfers: transfers, outputs, slots, nullifiers };
}

/** An output the owner must find at `slot`: its blinding comes from the transaction's first nullifier and `seed`. */
export type ExpectedNote = {
  slot: number;
  /** Shielded address bytes of the note's owner. */
  owner: Uint8Array;
  /** The mint's 32 address bytes. */
  mint: Uint8Array;
  amount: bigint;
  /** `NOTE_KIND`; 0 for a plain token note. */
  kind: number;
  /** Borsh state; empty for a token note. */
  state: Uint8Array;
  /** A token note paid to the owner's own address: its ciphertext must open, under the owner's key, to this note. */
  payout?: boolean;
};

export type Expected = {
  op: "redeem" | "draw";
  programId: Address;
  /** The keepers compiled into the site: the reply's `keeper` must be one of them. */
  keepers: readonly Address[];
  /** The owner's wallet: must sign read-only and nowhere else. */
  owner: Address;
  chainNow: number;
  /** Nullifier of the owner's note the keeper must spend first (the receipt, the position): the output blindings derive from it. */
  spends: Uint8Array;
  /** The owner's 33-byte viewing key: the new receipt or position is encrypted to it (`owner_viewing`). */
  ownerViewing: Uint8Array;
  /** The view tags the owner's wallet looks for a payment under (its viewing key's x, its identity's tag). */
  payoutTags: Uint8Array[];
  /** Blinding seed of the outputs below (`SEED_DOMAIN`). */
  seed: Uint8Array;
  notes: ExpectedNote[];
  /** draw: the reference price, 1e12-scaled; the bound must be within `CHECK.boundMarginBps` of it. */
  reference?: bigint;
};

export type Hashing = {
  blinding(firstNullifier: Uint8Array, seed: Uint8Array, slot: number): Promise<Uint8Array>;
  hash(note: ExpectedNote & { blinding: Uint8Array }): Promise<Uint8Array>;
  /**
   * Opens an output's published data as the owner's wallet sync would (the transfer cipher under the owner's
   * viewing key); undefined if it does not open. `plain`: no data records and no ring, as a token note has.
   */
  openPayout(p: { data: Uint8Array; txViewingPk: Uint8Array; salt: Uint8Array; slotIndex: number }): Promise<{ mint: Uint8Array; amount: bigint; blinding: Uint8Array; plain: boolean } | undefined>;
};

/** The keeper's `/lend/build` reply, decoded; `keeper` is the keeper that built and signed it. */
export type BuildReply = { transaction: Uint8Array; outputs: Uint8Array[]; now: number; bound?: bigint; keeper: Address };

export type Checked = { transaction: Transaction; now: bigint; bound?: bigint; outputs: Uint8Array[]; firstNullifier: Uint8Array };

const equal = (a: Uint8Array, b: Uint8Array) => a.length === b.length && a.every((x, i) => x === b[i]);
const refuse = (why: string): never => {
  throw new KeelerError("network", `The keeper's transaction does not match your request (${why}), so your wallet was not asked to sign it. Nothing was sent.`);
};

/**
 * The lend-program instructions of a landed transaction and the signers they name: what decides whether the owner's
 * own cancel or a keeper's liquidation spent a note. Lend transactions use no lookup tables, so the static accounts are all.
 */
export function lendInstructions(wire: Uint8Array, programId: Address): { tag: number; data: Uint8Array; signers: Address[] }[] {
  const tx = getTransactionDecoder().decode(wire);
  const message = decompileTransactionMessage(getCompiledTransactionMessageDecoder().decode(tx.messageBytes));
  return message.instructions
    .filter((ix) => ix.programAddress === programId && (ix.data?.length ?? 0) > 0)
    .map((ix) => ({
      tag: ix.data![0]!,
      data: new Uint8Array(ix.data!),
      signers: (ix.accounts ?? []).filter((a) => a.role === AccountRole.READONLY_SIGNER || a.role === AccountRole.WRITABLE_SIGNER).filter((a) => tx.signatures[a.address] != null).map((a) => a.address),
    }));
}

/** Accepts the keeper's transaction only in the exact shape the page asked for; returns it ready to sign. */
export async function checkKeeperTransaction(reply: BuildReply, e: Expected, h: Hashing): Promise<Checked> {
  const keeper = reply.keeper;
  if (!e.keepers.includes(keeper)) refuse("it comes from a keeper this site was not built with");
  let transaction: Transaction;
  let compiled: ReturnType<ReturnType<typeof getCompiledTransactionMessageDecoder>["decode"]>;
  try {
    transaction = getTransactionDecoder().decode(reply.transaction);
    compiled = getCompiledTransactionMessageDecoder().decode(transaction.messageBytes);
  } catch {
    return refuse("it does not decode");
  }
  if ("addressTableLookups" in compiled && (compiled.addressTableLookups?.length ?? 0) > 0) refuse("it uses an address lookup table");
  let message: ReturnType<typeof decompileTransactionMessage>;
  try {
    message = decompileTransactionMessage(compiled);
  } catch {
    return refuse("its message does not decode");
  }
  if ("nonce" in message.lifetimeConstraint) refuse("it is a durable-nonce transaction");
  if (message.feePayer.address !== keeper) refuse("the keeper is not its fee payer");
  if (message.feePayer.address === e.owner) refuse("it makes your wallet pay its fees");

  const lend = message.instructions.filter((ix) => ix.programAddress === e.programId);
  const other = message.instructions.filter((ix) => ix.programAddress !== e.programId && ix.programAddress !== COMPUTE_BUDGET_PROGRAM);
  if (other.length > 0) refuse(`it has an instruction for another program, ${other[0]!.programAddress}`);
  if (lend.length !== 1) refuse(`it has ${lend.length} lending instructions, not one`);
  if (message.instructions.some((ix) => ix.programAddress === COMPUTE_BUDGET_PROGRAM && (ix.accounts?.length ?? 0) > 0)) {
    refuse("a compute-budget instruction names accounts");
  }
  const ix = lend[0]!;
  const data = ix.data ?? new Uint8Array();
  const tag = e.op === "redeem" ? TAG.redeem : TAG.draw;
  if (data[0] !== tag) refuse(`its instruction is not a ${e.op}`);

  // The owner: one read-only signer slot at the head of the lend instruction, nowhere else, never writable.
  const accounts = ix.accounts ?? [];
  const mine = accounts.flatMap((a, i) => (a.address === e.owner ? [{ i, role: a.role }] : []));
  if (mine.length !== 1 || mine[0]!.i !== KEEPER_HEAD.owner || mine[0]!.role !== AccountRole.READONLY_SIGNER) {
    refuse("your wallet is not exactly one read-only signer in it");
  }
  if (accounts[KEEPER_HEAD.keeper]?.address !== keeper) refuse("the keeper does not sign it");

  const layout = e.op === "redeem" ? ARGS.redeem : ARGS.draw;
  if (data.length < 1 + layout.length) refuse("its instruction data is too short");
  const args = data.subarray(1, 1 + layout.length);
  let transact: Transact;
  try {
    transact = decodeTransact(data.subarray(1 + layout.length));
  } catch {
    return refuse("its shielded transfer does not decode");
  }
  if (!equal(args.subarray(layout.ownerViewing, layout.ownerViewing + 33), e.ownerViewing)) refuse("it encrypts your new note to another key");
  const now = u64At(args, layout.now);
  if (BigInt(reply.now) !== now) refuse("the time it states differs from its instruction");
  const chain = BigInt(e.chainNow);
  if (now < chain - BigInt(CHECK.nowBehind) || now > chain + BigInt(CHECK.nowAhead)) refuse("its time is not the chain's time");
  if (transact.expiryUnixTs <= chain || transact.expiryUnixTs > chain + BigInt(CHECK.txExpiryMax)) refuse("its expiry is not the one expected");
  if (transact.interfaceTransfers !== 0) refuse("it moves funds out of the private pool");

  let bound: bigint | undefined;
  if (e.op === "draw") {
    bound = u64At(args, ARGS.draw.bound);
    if (reply.bound !== bound) refuse("the price bound it states differs from its instruction");
    const ref = e.reference;
    if (ref === undefined || ref <= 0n) return refuse("there is no reference price to check its bound against");
    const off = bound > ref ? bound - ref : ref - bound;
    if (off * 10_000n > ref * BigInt(CHECK.boundMarginBps)) refuse("its price bound is off the market price");
  }

  if (reply.outputs.length !== transact.outputs.length || reply.outputs.some((o, i) => !equal(o, transact.outputs[i]!))) {
    refuse("the outputs it lists differ from its transfer");
  }
  const first = transact.nullifiers[0];
  if (!first || !equal(first, e.spends)) return refuse("it does not spend your note first");
  for (const note of e.notes) {
    const got = transact.outputs[note.slot];
    const blinding = await h.blinding(first, e.seed, note.slot);
    const want = await h.hash({ ...note, blinding });
    if (!got || !equal(got, want)) refuse(`output ${note.slot} is not the note you asked for`);
    // The hash alone does not make a payout findable: the wallet finds it by its tag and opens its ciphertext.
    if (note.payout && note.amount > 0n) {
      const slot = transact.slots[note.slot]!;
      if (!slot.tag || !e.payoutTags.some((t) => equal(t, slot.tag!)) || !slot.data) refuse(`output ${note.slot} is not addressed to your wallet`);
      // Ciphertext slot indexes count data-bearing slots only, as the wallet's sync does.
      const slotIndex = transact.slots.slice(0, note.slot).filter((x) => x.data).length;
      const opened = await h.openPayout({ data: slot.data!, txViewingPk: transact.txViewingPk, salt: transact.salt, slotIndex }).catch(() => undefined);
      if (!opened || !opened.plain || opened.amount !== note.amount || !equal(opened.mint, note.mint) || !equal(opened.blinding, blinding)) {
        refuse(`output ${note.slot} would not show up in your private balance`);
      }
    }
  }
  return { transaction, now, outputs: transact.outputs, firstNullifier: first, ...(bound === undefined ? {} : { bound }) };
}
