// The signing check on keeper-built transactions, fed honest and hostile ones (docs/plan-lend.md).
import { createHash } from "node:crypto";
import {
  AccountRole,
  address,
  appendTransactionMessageInstruction,
  appendTransactionMessageInstructions,
  compileTransaction,
  compressTransactionMessageUsingAddressLookupTables,
  createTransactionMessage,
  getAddressEncoder,
  getTransactionEncoder,
  pipe,
  setTransactionMessageFeePayer,
  setTransactionMessageLifetimeUsingBlockhash,
  setTransactionMessageLifetimeUsingDurableNonce,
  type Address,
  type Blockhash,
  type Instruction,
  type Nonce,
} from "@solana/kit";
import { encodeTransactInstructionData } from "@heliuslabs/spr/internal";
import { describe, expect, it } from "vitest";
import { KeelerError } from "../src/keeler/index";
import { checkKeeperTransaction, COMPUTE_BUDGET_PROGRAM, decodeTransact, type ExpectedNote, type Expected, type Hashing } from "../src/keeler/lend-check";
import { ARGS, SLOT, TAG, u64At, u64Bytes } from "../src/keeler/lend-layout";

const PROGRAM = address("A7qaBKJ1gJRcKwMWyyRdr9YhmbBmnMaemM9Sijah3EbC");
const KEEPER = address("GgBaCs3NCBuZN12kCJgAW63ydqohFkHEdfdEXBPzLHq");
const USER = address("AKnL4NNf3DGWZJS6cPknBuEGnVsV4A4m5tgebLHaRSZ9");
const OTHER = address("8qbHbw2BbbTHBW1sbeqakYXVKRQM8Ne7pLK7m6CVfeR");
const TREE = address("SysvarC1ock11111111111111111111111111111111");
const SPP = address("SysvarRent111111111111111111111111111111111");
const SYSTEM = address("11111111111111111111111111111111");
const TOKEN = address("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const NOW = 1_800_000_000;
const MINT = new Uint8Array(getAddressEncoder().encode(address("CktRuQ2mttgRGkXJtyksdKHjUdc2C4TgDzyB98oEzy8")));
const SOL_MINT = new Uint8Array(32).fill(9);
const USER_ADDR = new Uint8Array(64).fill(1); // shielded address bytes; the fake hash takes any
const PDA_ADDR = new Uint8Array(64).fill(2);
const RESERVE_NULLIFIER = new Uint8Array(32).fill(0xaa);
const RECEIPT_NULLIFIER = new Uint8Array(32).fill(0xbb);
const SEED = new Uint8Array(32).fill(5);
const VIEWING = new Uint8Array(33).fill(6);
const PRICE = address("SysvarS1otHashes111111111111111111111111111");
const REFERENCE = 142_370_000_000n;

const sha = (...parts: Uint8Array[]) => {
  const h = createHash("sha256");
  parts.forEach((p) => h.update(p));
  return new Uint8Array(h.digest());
};
/** A fake payout ciphertext: mint ‖ amount ‖ blinding ‖ plain flag, "sealed" by being prefixed with its slot index. */
const seal = (slotIndex: number, mint: Uint8Array, amount: bigint, blinding: Uint8Array, plain = true) => Uint8Array.of(slotIndex, ...mint, ...u64Bytes(amount), ...blinding, plain ? 1 : 0);
const hashing: Hashing = {
  blinding: async (first, seed, slot) => sha(first, seed, Uint8Array.of(slot)),
  hash: async (n) => sha(n.owner, n.mint, u64Bytes(n.amount), Uint8Array.of(n.kind), n.state, n.blinding),
  // Opens only under the slot index it was sealed for, as the real cipher.
  openPayout: async ({ data, slotIndex }) =>
    data.length === 74 && data[0] === slotIndex ? { mint: data.slice(1, 33), amount: u64At(data, 33), blinding: data.slice(41, 73), plain: data[73] === 1 } : undefined,
};
const MY_TAG = new Uint8Array(32).fill(7);

const w = (a: Address) => ({ address: a, role: AccountRole.WRITABLE });
const ws = (a: Address) => ({ address: a, role: AccountRole.WRITABLE_SIGNER });
const r = (a: Address) => ({ address: a, role: AccountRole.READONLY });
const rs = (a: Address) => ({ address: a, role: AccountRole.READONLY_SIGNER });

type Op = "redeem" | "draw";
/** The notes an honest keeper creates for the request below. */
function notes(op: Op): ExpectedNote[] {
  if (op === "redeem") {
    return [
      { slot: SLOT.redeem.payout, owner: USER_ADDR, mint: MINT, amount: 250_000000n, kind: 0, state: new Uint8Array(), payout: true },
      { slot: SLOT.redeem.receipt, owner: PDA_ADDR, mint: MINT, amount: 0n, kind: 2, state: new Uint8Array(40).fill(3) },
    ];
  }
  return [
    { slot: SLOT.draw.position, owner: PDA_ADDR, mint: SOL_MINT, amount: 3_000000000n, kind: 3, state: new Uint8Array(48).fill(4) },
    { slot: SLOT.draw.debtPayout, owner: USER_ADDR, mint: MINT, amount: 100_000000n, kind: 0, state: new Uint8Array(), payout: true },
    { slot: SLOT.draw.collateralPayout, owner: USER_ADDR, mint: SOL_MINT, amount: 0n, kind: 0, state: new Uint8Array(), payout: true },
  ];
}
const expected = (op: Op, over: Partial<Expected> = {}): Expected => ({
  op,
  programId: PROGRAM,
  keeper: KEEPER,
  owner: USER,
  chainNow: NOW,
  spends: RECEIPT_NULLIFIER,
  ownerViewing: VIEWING,
  payoutTags: [new Uint8Array(32).fill(1), MY_TAG],
  seed: SEED,
  notes: notes(op),
  ...(op === "draw" ? { reference: REFERENCE } : {}),
  ...over,
});

type Build = {
  op?: Op;
  notes?: ExpectedNote[];
  tag?: number;
  now?: number;
  bound?: bigint;
  expiry?: number;
  transfers?: boolean;
  feePayer?: Address;
  ownerRole?: AccountRole;
  extra?: Instruction[];
  lut?: boolean;
  nonce?: boolean;
  viewing?: Uint8Array;
  spentSecond?: boolean;
  /** Replaces what slot `slot` publishes (its ciphertext); null publishes none. */
  data?: (slot: number, honest: Uint8Array) => Uint8Array | null;
  ownerTag?: Uint8Array;
};

async function build(o: Build = {}) {
  const op = o.op ?? "redeem";
  const now = o.now ?? NOW;
  const bound = o.bound ?? (REFERENCE * 99n) / 100n;
  // Every slot holds some hash; the owner's slots hold the honest (or given) notes.
  const outputs = await Promise.all(
    [0, 1, 2, 3].map(async (slot) => {
      const n = (o.notes ?? notes(op)).find((x) => x.slot === slot);
      return n ? hashing.hash({ ...n, blinding: await hashing.blinding(RECEIPT_NULLIFIER, SEED, slot) }) : sha(Uint8Array.of(slot));
    }),
  );
  // Every slot publishes a ciphertext (the SDK's 1:1 assembly); the owner's payouts open to their notes.
  const published = await Promise.all(
    [0, 1, 2, 3].map(async (slot) => {
      const n = (o.notes ?? notes(op)).find((x) => x.slot === slot);
      const honest = n ? seal(slot, n.mint, n.amount, await hashing.blinding(RECEIPT_NULLIFIER, SEED, slot)) : new Uint8Array(74);
      return o.data ? o.data(slot, honest) : honest;
    }),
  );
  const transact = encodeTransactInstructionData({
    expiryUnixTs: BigInt(o.expiry ?? NOW + 60),
    txViewingPk: new Uint8Array(33).fill(2),
    salt: new Uint8Array(16),
    interfaceTransfers: o.transfers ? [{ kind: "solWithdrawal", amount: 5n }] : [],
    outputs: outputs.map((utxoHash, i) => ({ utxoHash, ownerTag: { kind: "inline" as const, value: o.ownerTag ?? MY_TAG }, ...(published[i] ? { data: published[i] } : {}) })),
    messages: [],
    privateTxHash: new Uint8Array(32),
    circuit: { kind: "confidentialSigner", inputs: 2, outputs: 4, publicAssetSlots: 0 },
    proof: { a: new Uint8Array(32), b: new Uint8Array(128), c: new Uint8Array(32) },
    // The owner's note is the first input: the output blindings derive from its nullifier.
    inputs: (o.spentSecond ? [RESERVE_NULLIFIER, RECEIPT_NULLIFIER] : [RECEIPT_NULLIFIER, RESERVE_NULLIFIER]).map((nullifierHash) => ({ nullifierHash, treeIndex: 0 })),
    treeContexts: [{ utxoTreeRootIndex: 0, nullifierTreeRootIndex: 0 }],
  } as never);
  const layout = op === "redeem" ? ARGS.redeem : ARGS.draw;
  const args = new Uint8Array(layout.length);
  args.set(u64Bytes(BigInt(now)), layout.now);
  args.set(o.viewing ?? VIEWING, layout.ownerViewing);
  if (op === "draw") args.set(u64Bytes(bound), ARGS.draw.bound);
  const data = Uint8Array.of(o.tag ?? (op === "redeem" ? TAG.redeem : TAG.draw), ...args, ...transact);
  const lend: Instruction = {
    programAddress: PROGRAM,
    accounts: [ws(KEEPER), { address: USER, role: o.ownerRole ?? AccountRole.READONLY_SIGNER }, ...(op === "draw" ? [r(PRICE)] : []), ws(KEEPER), w(TREE), r(SPP), r(SYSTEM), w(TREE), r(OTHER)],
    data,
  };
  const budget: Instruction = { programAddress: COMPUTE_BUDGET_PROGRAM, data: Uint8Array.of(2, 0x80, 0x5c, 0x15, 0) };
  let message = pipe(
    createTransactionMessage({ version: 0 }),
    (m) => setTransactionMessageFeePayer(o.feePayer ?? KEEPER, m),
    (m) =>
      o.nonce
        ? setTransactionMessageLifetimeUsingDurableNonce({ nonce: "11111111111111111111111111111111" as Nonce, nonceAccountAddress: TREE, nonceAuthorityAddress: KEEPER }, m)
        : setTransactionMessageLifetimeUsingBlockhash({ blockhash: "11111111111111111111111111111111" as Blockhash, lastValidBlockHeight: 0n }, m),
    (m) => appendTransactionMessageInstructions([budget, lend, ...(o.extra ?? [])], m),
  );
  if (o.lut) message = compressTransactionMessageUsingAddressLookupTables(message, { [SPP]: [OTHER] }) as never;
  const wire = new Uint8Array(getTransactionEncoder().encode(compileTransaction(message as never)));
  return { transaction: wire, outputs, now, ...(op === "draw" ? { bound } : {}) };
}

const refusal = async (p: Promise<unknown>) => {
  const e = await p.then(() => undefined, (x: unknown) => x);
  expect(e).toBeInstanceOf(KeelerError);
  return (e as KeelerError).message;
};

describe("the signing check", () => {
  it("accepts the honest redeem and draw", async () => {
    const redeem = await checkKeeperTransaction(await build(), expected("redeem"), hashing);
    expect(redeem.now).toBe(BigInt(NOW));
    expect(redeem.firstNullifier).toEqual(RECEIPT_NULLIFIER);
    const draw = await checkKeeperTransaction(await build({ op: "draw" }), expected("draw"), hashing);
    expect(draw.bound).toBe((REFERENCE * 99n) / 100n);
  });

  it("refuses an added token transfer", async () => {
    const transfer: Instruction = { programAddress: TOKEN, accounts: [w(OTHER), w(TREE), rs(USER)], data: Uint8Array.of(3, ...u64Bytes(1_000n)) };
    expect(await refusal(checkKeeperTransaction(await build({ extra: [transfer] }), expected("redeem"), hashing))).toMatch(/another program/);
  });

  it("refuses an added approve", async () => {
    const approve: Instruction = { programAddress: TOKEN, accounts: [w(OTHER), r(KEEPER), rs(USER)], data: Uint8Array.of(4, ...u64Bytes(1n << 60n)) };
    expect(await refusal(checkKeeperTransaction(await build({ extra: [approve] }), expected("redeem"), hashing))).toMatch(/another program/);
  });

  it("refuses a system transfer", async () => {
    const sys: Instruction = { programAddress: SYSTEM, accounts: [ws(USER), w(KEEPER)], data: Uint8Array.of(2, 0, 0, 0, ...u64Bytes(1_000_000n)) };
    expect(await refusal(checkKeeperTransaction(await build({ extra: [sys] }), expected("redeem"), hashing))).toMatch(/another program|read-only/);
  });

  it("refuses a second lend instruction", async () => {
    const honest = await build();
    const second: Instruction = { programAddress: PROGRAM, accounts: [ws(KEEPER)], data: Uint8Array.of(TAG.supply) };
    expect(await refusal(checkKeeperTransaction(await build({ extra: [second] }), expected("redeem"), hashing))).toMatch(/2 lending instructions/);
    expect(honest.transaction.length).toBeGreaterThan(0);
  });

  it("refuses a different tag", async () => {
    expect(await refusal(checkKeeperTransaction(await build({ tag: TAG.liquidate }), expected("redeem"), hashing))).toMatch(/not a redeem/);
    // A redeem offered where a draw was asked for.
    expect(await refusal(checkKeeperTransaction(await build(), expected("draw"), hashing))).toMatch(/not a draw/);
  });

  it("refuses the user as fee payer, or as a writable account", async () => {
    expect(await refusal(checkKeeperTransaction(await build({ feePayer: USER }), expected("redeem"), hashing))).toMatch(/fee payer/);
    expect(await refusal(checkKeeperTransaction(await build({ ownerRole: AccountRole.WRITABLE_SIGNER }), expected("redeem"), hashing))).toMatch(/read-only/);
  });

  it("refuses a lookup table and a durable nonce", async () => {
    expect(await refusal(checkKeeperTransaction(await build({ lut: true }), expected("redeem"), hashing))).toMatch(/lookup table/);
    expect(await refusal(checkKeeperTransaction(await build({ nonce: true }), expected("redeem"), hashing))).toMatch(/durable-nonce/);
  });

  it("refuses a different payout amount", async () => {
    const less = notes("redeem").map((n) => (n.slot === SLOT.redeem.payout ? { ...n, amount: 249_000000n } : n));
    expect(await refusal(checkKeeperTransaction(await build({ notes: less }), expected("redeem"), hashing))).toMatch(/output 2/);
  });

  it("refuses a payout to a different owner, and a receipt with fewer shares", async () => {
    const theirs = notes("draw").map((n) => (n.slot === SLOT.draw.debtPayout ? { ...n, owner: new Uint8Array(64).fill(9) } : n));
    expect(await refusal(checkKeeperTransaction(await build({ op: "draw", notes: theirs }), expected("draw"), hashing))).toMatch(/output 2/);
    const fewer = notes("redeem").map((n) => (n.slot === SLOT.redeem.receipt ? { ...n, state: new Uint8Array(40).fill(1) } : n));
    expect(await refusal(checkKeeperTransaction(await build({ notes: fewer }), expected("redeem"), hashing))).toMatch(/output 0/);
  });

  it("refuses a stale or future now, and an expiry out of range", async () => {
    expect(await refusal(checkKeeperTransaction(await build({ now: NOW - 600 }), expected("redeem"), hashing))).toMatch(/chain's time/);
    expect(await refusal(checkKeeperTransaction(await build({ now: NOW + 60 }), expected("redeem"), hashing))).toMatch(/chain's time/);
    expect(await refusal(checkKeeperTransaction(await build({ expiry: NOW + 3600 }), expected("redeem"), hashing))).toMatch(/expiry/);
    expect(await refusal(checkKeeperTransaction(await build({ expiry: NOW }), expected("redeem"), hashing))).toMatch(/expiry/);
  });

  it("refuses an off-market price bound", async () => {
    const low = (REFERENCE * 95n) / 100n;
    expect(await refusal(checkKeeperTransaction(await build({ op: "draw", bound: low }), expected("draw"), hashing))).toMatch(/off the market/);
    expect(await refusal(checkKeeperTransaction(await build({ op: "draw" }), expected("draw", { reference: undefined }), hashing))).toMatch(/reference price/);
  });

  it("refuses a public withdrawal, outputs that differ from the transfer, and a transfer that spends another note", async () => {
    expect(await refusal(checkKeeperTransaction(await build({ transfers: true }), expected("redeem"), hashing))).toMatch(/out of the private pool/);
    const honest = await build();
    const lying = { ...honest, outputs: [...honest.outputs].reverse() };
    expect(await refusal(checkKeeperTransaction(lying, expected("redeem"), hashing))).toMatch(/outputs it lists/);
    expect(await refusal(checkKeeperTransaction(honest, expected("redeem", { spends: new Uint8Array(32).fill(1) }), hashing))).toMatch(/does not spend your note/);
    expect(await refusal(checkKeeperTransaction({ ...honest, now: NOW - 1 }, expected("redeem"), hashing))).toMatch(/states differs/);
    expect(await refusal(checkKeeperTransaction(await build({ spentSecond: true }), expected("redeem"), hashing))).toMatch(/does not spend your note first/);
  });

  it("refuses a payout the wallet would never find: another tag, no ciphertext, or one that opens to another note", async () => {
    expect(await refusal(checkKeeperTransaction(await build({ ownerTag: new Uint8Array(32).fill(8) }), expected("redeem"), hashing))).toMatch(/output 2 is not addressed to your wallet/);
    expect(await refusal(checkKeeperTransaction(await build({ data: (slot, h) => (slot === 2 ? null : h) }), expected("redeem"), hashing))).toMatch(/output 2 is not addressed/);
    const blinding = await hashing.blinding(RECEIPT_NULLIFIER, SEED, SLOT.redeem.payout);
    // Correct hash, but the ciphertext says less, another token, another blinding, carries data, or is sealed for another slot.
    for (const lie of [
      seal(2, MINT, 1n, blinding),
      seal(2, SOL_MINT, 250_000000n, blinding),
      seal(2, MINT, 250_000000n, new Uint8Array(32)),
      seal(2, MINT, 250_000000n, blinding, false),
      seal(1, MINT, 250_000000n, blinding),
      new Uint8Array(10),
    ]) {
      expect(await refusal(checkKeeperTransaction(await build({ data: (slot, h) => (slot === 2 ? lie : h) }), expected("redeem"), hashing))).toMatch(/output 2 would not show up in your private balance/);
    }
    // The slot index counts data-bearing slots only.
    const skip = await build({ data: (slot, h) => (slot === 0 ? null : slot === 2 ? Uint8Array.of(1, ...h.subarray(1)) : h) });
    expect((await checkKeeperTransaction(skip, expected("redeem"), hashing)).now).toBe(BigInt(NOW));
    // A payout of nothing (the draw's collateral slot here) has nothing to find.
    expect((await checkKeeperTransaction(await build({ op: "draw", data: (slot, h) => (slot === 3 ? null : h) }), expected("draw"), hashing)).bound).toBeDefined();
  });

  it("refuses a new receipt or position encrypted to another viewing key", async () => {
    const theirs = new Uint8Array(33).fill(8);
    expect(await refusal(checkKeeperTransaction(await build({ viewing: theirs }), expected("redeem"), hashing))).toMatch(/another key/);
    expect(await refusal(checkKeeperTransaction(await build({ op: "draw", viewing: theirs }), expected("draw"), hashing))).toMatch(/another key/);
  });

  it("decodes what the SDK encodes and refuses trailing bytes", async () => {
    const { transaction } = await build();
    expect(await refusal(checkKeeperTransaction({ transaction: transaction.slice(0, 40), outputs: [], now: NOW }, expected("redeem"), hashing))).toMatch(/decode/);
    const appended = await build();
    // A lend instruction whose transfer has a byte after it.
    const noisy = pipe(
      createTransactionMessage({ version: 0 }),
      (m) => setTransactionMessageFeePayer(KEEPER, m),
      (m) => setTransactionMessageLifetimeUsingBlockhash({ blockhash: "11111111111111111111111111111111" as Blockhash, lastValidBlockHeight: 0n }, m),
      (m) => appendTransactionMessageInstruction({ programAddress: PROGRAM, accounts: [ws(KEEPER), rs(USER)], data: Uint8Array.of(TAG.redeem, ...new Uint8Array(ARGS.redeem.length), 0) }, m),
    );
    const wire = new Uint8Array(getTransactionEncoder().encode(compileTransaction(noisy)));
    expect(await refusal(checkKeeperTransaction({ ...appended, transaction: wire }, expected("redeem"), hashing))).toMatch(/shielded transfer/);
    expect(() => decodeTransact(new Uint8Array(10))).toThrow();
  });
});
