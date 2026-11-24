// Make, cancel and order status for the swap program: a port of
// `swap_sdk::instructions::{make,cancel}` and `cli/src/chain.rs`.
import {
  AccountRole,
  address,
  decompileTransactionMessage,
  getAddressDecoder,
  getAddressEncoder,
  getBase64Encoder,
  getCompiledTransactionMessageDecoder,
  getProgramDerivedAddress,
  getTransactionDecoder,
  isSignerRole,
  type Address,
  type Instruction,
  type Transaction,
} from "@solana/kit";
import { LocalKeys, SHIELDED_POOL_PROGRAM_ID, SOL_MINT, ShieldedAddress, type Wallet, type WalletUtxo } from "@heliuslabs/spr";
import { NullifierKeyProofAuthority, atSlot, type SprClient } from "@heliuslabs/spr/client";
import { bytes32, poseidon, solanaOwnerIdentity, transactOutputBlinding, outputBlindingSeed } from "@heliuslabs/spr/keypair";
import { ProgramOwner, decodeProgramTransaction, toProgramWalletUtxo } from "@heliuslabs/spr/program";
import {
  createProofOutput,
  createExternalData,
  decodeConfidential,
  decodeOutputData,
  EncryptedScheme,
  SppProofInputs,
  Mint,
} from "@heliuslabs/spr/transaction";
import { X25519PublicKey } from "@heliuslabs/spr/keypair";
import { compileUnsignedTransaction, encodeTransactInstructionData, nullifierPdaAccounts } from "@heliuslabs/spr/internal";
import { resolveRegisteredAddress } from "@heliuslabs/spr/wallet";
import { callWorker, type Circuit, type RawOutput, type RawProgramTransaction } from "./swap-wasm";

/** A swap or a split carries a proof and an SPP transact: the whole budget. */
export const COMPUTE_UNIT_LIMIT = 1_400_000;
export const TAG = { make: 2, cancel: 4 } as const;
/** Output slot of the order UTXO in the make transaction. */
const ORDER_SLOT = 1;
/** The SDK hashes discovered orders under tree id 0 only. */
const TREE_ID = 0;
const SYSTEM_PROGRAM = address("11111111111111111111111111111111");
const BN254_ORDER = 21888242871839275222246405745257275088548364400416034343698204186575808495617n;
/** `u32::from_be_bytes(*b"SWTX")` as a field element. */
const TAKE_BLINDING_DOMAIN = (() => {
  const b = new Uint8Array(32);
  b.set(new TextEncoder().encode("SWTX"), 28);
  return b;
})();

export type SwapContext = {
  client: SprClient;
  wallet: Wallet;
  keys: LocalKeys;
  /** The maker's Solana address: fee payer, signer and owner. */
  payer: Address;
  programId: Address;
  network: string;
};

/** What a browser must keep to find and cancel its order. */
export type OrderOpening = {
  orderHash: Uint8Array;
  /** The order output as the WASM gave it: the opening needed to spend it. */
  output: RawOutput;
};

export const toHex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
export const decimal = (b: Uint8Array) => BigInt(`0x${toHex(b) || "0"}`).toString();
const equal = (a: Uint8Array, b: Uint8Array) => a.length === b.length && a.every((x, i) => x === b[i]);
export const addressBytes = (a: Address) => new Uint8Array(getAddressEncoder().encode(a));

export async function orderAuthority(maker: Address, programId: Address) {
  const [pda, bump] = await getProgramDerivedAddress({
    programAddress: programId,
    seeds: [new TextEncoder().encode("order_authority"), addressBytes(maker)],
  });
  return { pda, bump, owner: new ProgramOwner(pda) };
}

/** `[tag] ‖ proof ‖ args ‖ transact`: the program reads its args, then forwards the transact bytes. */
export function ixData(tag: number, proof: Uint8Array, args: Uint8Array, transact: Uint8Array): Uint8Array {
  if (proof.length !== 128) throw new Error(`a compressed proof is 128 bytes, got ${proof.length}`);
  const out = new Uint8Array(1 + proof.length + args.length + transact.length);
  out[0] = tag;
  out.set(proof, 1);
  out.set(args, 1 + proof.length);
  out.set(transact, 1 + proof.length + args.length);
  return out;
}

/** `MakeArgs` after the proof: `viewing_pubkey: [u8; 33]`, `authority_bump: u8`. */
export function makeArgs(viewingPubkey: Uint8Array, bump: number): Uint8Array {
  if (viewingPubkey.length !== 33) throw new Error("viewing key must be 33 bytes");
  return Uint8Array.of(...viewingPubkey, bump);
}

/** `CancelArgs` after the proof: `order_expiry: u64` little-endian, `authority_bump: u8`. */
export function cancelArgs(expiry: bigint, bump: number): Uint8Array {
  const out = new Uint8Array(9);
  new DataView(out.buffer).setBigUint64(0, expiry, true);
  out[8] = bump;
  return out;
}

const w = (a: Address) => ({ address: a, role: AccountRole.WRITABLE });
const ws = (a: Address) => ({ address: a, role: AccountRole.WRITABLE_SIGNER });
const r = (a: Address) => ({ address: a, role: AccountRole.READONLY });
const rs = (a: Address) => ({ address: a, role: AccountRole.READONLY_SIGNER });

/** The account list of `sdk/src/instructions/make/instruction.rs`. */
export function makeAccounts(payer: Address, tree: Address, nullifierPdas: Address[], authority: Address) {
  return [
    ws(payer), ws(payer), w(tree), r(SHIELDED_POOL_PROGRAM_ID), r(SYSTEM_PROGRAM), w(tree),
    ...nullifierPdas.map(w), rs(payer), r(authority),
  ];
}

/** The account list of `sdk/src/instructions/cancel/instruction.rs`. */
export function cancelAccounts(payer: Address, maker: Address, tree: Address, nullifierPdas: Address[], authority: Address) {
  return [
    ws(payer), rs(maker), ws(payer), w(tree), r(SHIELDED_POOL_PROGRAM_ID), r(SYSTEM_PROGRAM), w(tree),
    ...nullifierPdas.map(w), r(authority),
  ];
}

/** The marker message's data, Borsh `MarkerData { order_utxo_hash, maker_pubkey }`. */
export function markerData(orderHash: Uint8Array, maker: Address): Uint8Array {
  return Uint8Array.of(...orderHash, ...addressBytes(maker));
}

export async function prove(ctx: SwapContext, circuit: Circuit, raw: RawProgramTransaction): Promise<Uint8Array> {
  if (BigInt(`0x${toHex(raw.publicHash)}`) >= BN254_ORDER) throw new Error("public hash is not a field element");
  const { proof, ms } = await callWorker<{ proof: Uint8Array; ms: number }>({
    op: "prove",
    circuit,
    proofInputs: raw.proofInputs,
    publicHash: raw.publicHash,
  });
  lastProofMs = ms;
  return proof;
}
/** Proving time of the last swap proof in the worker, for tests and the interface. */
export let lastProofMs = 0;

/** Decodes and checks a WASM transaction: sender, payer, commitments. */
export async function decode(ctx: SwapContext, raw: RawProgramTransaction) {
  const tx = await decodeProgramTransaction(raw.finalizedTx, ctx.wallet.registry);
  if (tx.payer !== ctx.payer) throw new Error("the swap transaction names another payer");
  if (!equal(tx.sender.toBytes(), ctx.keys.address().toBytes())) throw new Error("the swap transaction names another sender");
  return tx;
}

type Decoded = Awaited<ReturnType<typeof decode>>;

/** The SPP inputs with the outputs encrypted, plus `messages` (the SDK's own encrypt sends none). */
export async function sppInputs(ctx: SwapContext, tx: Decoded, messages: { viewTag: ReturnType<typeof bytes32>; data: Uint8Array }[]) {
  const [key] = await ctx.keys.transactionKeys([
    { viewingPublicKey: tx.sender.viewingPublicKey, firstNullifier: tx.firstNullifier },
  ]);
  if (!key) throw new Error("no transaction viewing key");
  try {
    const ext = tx.encrypt(key);
    return new SppProofInputs({
      payer: tx.payer,
      inputUtxos: tx.inputs,
      outputs: tx.outputs,
      externalData: createExternalData({
        instructionDiscriminator: ext.instructionDiscriminator,
        expiryUnixTs: ext.expiryUnixTs,
        interfaceTransfers: ext.interfaceTransfers,
        txViewingPublicKey: ext.txViewingPublicKey,
        salt: ext.salt,
        outputs: ext.outputs,
        resolvedOwnerTags: ext.resolvedOwnerTags,
        messages,
        ...(ext.dataHash ? { dataHash: ext.dataHash } : {}),
        ...(ext.ringDataHash ? { ringDataHash: ext.ringDataHash } : {}),
      }),
      blindingSeed: tx.blindingSeed,
      outputTreeId: tx.outputTreeId,
    });
  } finally {
    key.destroy();
  }
}

export async function proveSpp(ctx: SwapContext, tx: Decoded, spp: SppProofInputs, authority: Parameters<SprClient["proveTransact"]>[1]) {
  const data = await ctx.client.proveTransact(spp, authority);
  if (!equal(data.privateTxHash, tx.paddingIndependentPrivateTxHash)) {
    throw new Error("the SPP proof and the swap proof cover different transactions");
  }
  return data;
}

/** The transaction and the last block height at which it can still land. */
export async function compile(ctx: SwapContext, ix: Instruction): Promise<{ transaction: Transaction; lastValidBlockHeight: bigint }> {
  const lifetime = await ctx.client.getLatestBlockhash();
  const transaction = await compileUnsignedTransaction({ feePayer: ctx.payer, lifetime, instructions: [ix], computeUnitLimit: COMPUTE_UNIT_LIMIT });
  return { transaction, lastValidBlockHeight: lifetime.lastValidBlockHeight };
}

export type MakeParams = {
  /** One unspent default-ring note of the sell mint covering `sellAmount`. */
  source: WalletUtxo;
  sellAmount: bigint;
  buyMint: Address;
  buyAmount: bigint;
  expiry: bigint;
  /** The dealer's Solana address; its shielded address comes from the registry. */
  dealer: Address;
};

/** Builds and proves a make; the caller has the wallet sign and send it. */
export async function buildMake(ctx: SwapContext, p: MakeParams, onProving: () => void) {
  const taker = await resolveRegisteredAddress({ rpc: ctx.client, owner: p.dealer });
  if (!taker) throw new Error(`dealer ${p.dealer} is not in the user registry`);
  const sender = ctx.keys.address();
  const authority = await orderAuthority(ctx.payer, ctx.programId);
  const registry = ctx.wallet.registry;
  const inputs = {
    private: {
      txContext: { outputTreeId: TREE_ID },
      source: toProgramWalletUtxo(p.source, { nullifierPublicKey: sender.nullifierPublicKey, treeId: TREE_ID, assets: registry }),
      sourceAmount: p.sellAmount,
      destinationMint: { asset: p.buyMint, assetId: registry.assetId(p.buyMint) },
      destinationAmount: p.buyAmount,
      expiry: p.expiry,
      takerPkFe: Array.from(solanaOwnerIdentity(addressBytes(p.dealer))),
    },
    public: {
      makerOwnerPkField: decimal(solanaOwnerIdentity(addressBytes(ctx.payer))),
      orderOwner: authority.owner.address(taker.address.viewingPublicKey).toBytes(),
    },
  };
  onProving();
  const raw = await callWorker<RawProgramTransaction>({ op: "make", inputs, sender: sender.toBytes(), payer: ctx.payer });
  const tx = await decode(ctx, raw);
  const orderHash = tx.outputHashes[ORDER_SLOT];
  const output = raw.finalizedTx.outputUtxos[ORDER_SLOT];
  if (!orderHash || !output) throw new Error("the make transaction has no order output");
  const spp = await sppInputs(ctx, tx, [
    { viewTag: taker.address.signingPublicKey.confidentialViewTag(), data: markerData(orderHash, ctx.payer) },
  ]);
  const transact = await proveSpp(ctx, tx, spp, ctx.keys);
  const proof = await prove(ctx, "make", raw);
  const pdas = (await nullifierPdaAccounts(ctx.client.tree, transact.inputs.map((i) => i.nullifierHash))).map((m) => m.address);
  const ix: Instruction = {
    programAddress: ctx.programId,
    accounts: makeAccounts(ctx.payer, ctx.client.tree, pdas, authority.pda),
    data: ixData(TAG.make, proof, makeArgs(taker.address.viewingPublicKey.toBytes(), authority.bump), encodeTransactInstructionData(transact)),
  };
  return { ...(await compile(ctx, ix)), opening: { orderHash, output } satisfies OrderOpening };
}

/** The order UTXO as an SPP input at `leafIndex`, owned by the maker's order authority. */
async function orderInput(ctx: SwapContext, o: OrderOpening, leafIndex: bigint) {
  const { owner } = await orderAuthority(ctx.payer, ctx.programId);
  return {
    utxo: {
      owner: owner.publicKey().toBytes(),
      asset: o.output.asset,
      amount: o.output.amount,
      blinding: o.output.blinding,
      data: o.output.data ?? { records: [] },
    },
    nullifierPubkey: ProgramOwner.nullifierPublicKey(),
    utxoHash: o.orderHash,
    nullifier: orderNullifier(o),
    ...(o.output.dataHash ? { dataHash: o.output.dataHash } : {}),
    treeId: TREE_ID,
    leafIndex,
  };
}

export function orderNullifier(o: OrderOpening): Uint8Array {
  return ProgramOwner.nullifierKey().nullifier(bytes32(o.orderHash), bytes32(o.output.blinding));
}

/** The order's leaf index. An indexer error propagates: it is never read as "not on chain". */
export async function leafIndex(ctx: SwapContext, o: OrderOpening): Promise<bigint | undefined> {
  const res = await ctx.client.getMerkleProofs(ctx.client.tree, [bytes32(o.orderHash)]);
  return res.proofs[0]?.leafIndex;
}

export type OrderTerms = {
  destinationMint: unknown;
  destinationAmount: bigint;
  maker: unknown;
  expiry: bigint;
  takerPkFe: Uint8Array;
};

/** Builds and proves the cancel of an expired order. */
export async function buildCancel(ctx: SwapContext, o: OrderOpening, onProving: () => void) {
  const leaf = await leafIndex(ctx, o);
  if (leaf === undefined) throw new Error("the order is not indexed yet");
  const orderUtxo = await orderInput(ctx, o, leaf);
  const { data: terms } = await callWorker<TermsUtxo>({ op: "orderTerms", utxo: orderUtxo });
  const authority = await orderAuthority(ctx.payer, ctx.programId);
  const destination = mintOf(ctx, terms.destinationMint);
  const inputs = {
    private: {
      txContext: { outputTreeId: TREE_ID },
      terms: {
        destinationMint: { asset: destination, assetId: ctx.wallet.registry.assetId(destination) },
        destinationAmount: terms.destinationAmount,
        maker: terms.maker,
        expiry: terms.expiry,
        takerPkFe: Array.from(terms.takerPkFe),
      },
      orderUtxo,
    },
    public: {
      expiry: terms.expiry,
      makerOwnerPkField: decimal(solanaOwnerIdentity(addressBytes(ctx.payer))),
    },
  };
  onProving();
  const raw = await callWorker<RawProgramTransaction>({
    op: "cancel",
    inputs,
    sender: ctx.keys.address().toBytes(),
    payer: ctx.payer,
  });
  const tx = await decode(ctx, raw);
  const spp = await sppInputs(ctx, tx, []);
  const nk = ProgramOwner.nullifierKey();
  const transact = await proveSpp(ctx, tx, spp, new NullifierKeyProofAuthority(nk, ctx.client.proofService));
  const proof = await prove(ctx, "cancel", raw);
  const pdas = (await nullifierPdaAccounts(ctx.client.tree, transact.inputs.map((i) => i.nullifierHash))).map((m) => m.address);
  const ix: Instruction = {
    programAddress: ctx.programId,
    accounts: cancelAccounts(ctx.payer, ctx.payer, ctx.client.tree, pdas, authority.pda),
    data: ixData(TAG.cancel, proof, cancelArgs(terms.expiry, authority.bump), encodeTransactInstructionData(transact)),
  };
  return (await compile(ctx, ix)).transaction;
}

/** The terms' destination asset: all zeros is SOL, any other is the mint's address bytes. */
export function mintOf(ctx: SwapContext, asset: unknown): Address {
  const bytes = Array.isArray(asset) ? Uint8Array.from(asset as number[]) : asset;
  if (!(bytes instanceof Uint8Array) || bytes.length !== 32) throw new Error("unreadable destination mint in the order terms");
  const mint = bytes.every((b) => b === 0) ? SOL_MINT : getAddressDecoder().decode(bytes);
  ctx.wallet.registry.assetId(mint); // throws for a mint the registry does not know
  return mint;
}

/** `taken-unmatched`: spent by a transaction that is not this maker's cancel, with no payout that matched. */
export type ChainStatus = "open" | "settled" | "cancelled" | "taken-unmatched";

/**
 * Spent or not, and if spent whether by a take (the maker's derived payout is
 * a tree-0 leaf the spending transaction appended) or by this maker's cancel.
 * A read error propagates, so a caller never takes a failed read for a status.
 */
export async function orderStatus(ctx: SwapContext, o: OrderOpening, buy: { mint: Address; amount: bigint }): Promise<ChainStatus> {
  const nullifier = orderNullifier(o);
  const slot = await ctx.client.getSlot();
  const page = await ctx.client.getShieldedTransactionsByNullifiers({ nullifiers: [nullifier as never] }, atSlot(slot));
  const spends = (e: (typeof page.data)[number]["events"][number]) => e.nullifiers.some((n) => equal(n, nullifier));
  const group = page.data.find((g) => g.events.some(spends));
  const spend = group?.events.find(spends);
  if (!group || !spend) return "open";
  const payout = derivedPayoutHash(ctx, o, nullifier, buy);
  if (spend.outputSlots.some((s) => equal(s.outputContext.hash, payout))) return "settled";
  // No matching payout: only a cancel this maker signed counts as a refund.
  const tx = await ctx.client.solanaRpc
    .getTransaction(group.signature, { encoding: "base64", maxSupportedTransactionVersion: 1, commitment: "confirmed" })
    .send();
  if (!tx) throw new Error("the RPC has no copy of the transaction that spent this order yet");
  const wire = new Uint8Array(getBase64Encoder().encode(tx.transaction[0]));
  return isMakersCancel(wire, ctx.programId, ctx.payer) ? "cancelled" : "taken-unmatched";
}

/** Whether a wire transaction holds a swap-program cancel whose maker account is `maker`, signed by it. */
export function isMakersCancel(wire: Uint8Array, programId: Address, maker: Address): boolean {
  const tx = getTransactionDecoder().decode(wire);
  if (tx.signatures[maker] == null) return false;
  // `compile` uses no address lookup tables, so the static accounts are all of them.
  const message = decompileTransactionMessage(getCompiledTransactionMessageDecoder().decode(tx.messageBytes));
  return message.instructions.some((ix) => {
    const m = ix.accounts?.[1];
    return ix.programAddress === programId && ix.data?.[0] === TAG.cancel && m?.address === maker && isSignerRole(m.role);
  });
}

/** The maker's payout of the take whose first nullifier is the order's: output slot 1. */
export function derivedPayoutHash(ctx: SwapContext, o: OrderOpening, nullifier: Uint8Array, buy: { mint: Address; amount: bigint }) {
  const takeSeed = poseidon([TAKE_BLINDING_DOMAIN, o.output.blinding]);
  const seed = outputBlindingSeed(nullifier as never, takeSeed as never);
  const blinding = transactOutputBlinding(nullifier as never, seed, 1);
  return createProofOutput({
    asset: buy.mint,
    mint: Mint.fromRegistry(ctx.wallet.registry, buy.mint),
    amount: buy.amount,
    blinding,
    ownerAddress: ShieldedAddress.fromBytes(ctx.keys.address().toBytes()),
  } as never).hash(TREE_ID);
}

/** What the WASM `orderTermsDataUtxo` returns: `DataUtxo::try_from`, which decodes the terms and checks that they and the opening reproduce `utxoHash`. */
type TermsUtxo = { utxo: { dataHash?: Uint8Array }; data: OrderTerms };

export type FoundOrder = {
  opening: OrderOpening;
  terms: OrderTerms;
  sellMint: Address;
  buyMint: Address;
  /** The dealer's viewing key, as the order slot embeds it. */
  takerViewingKey: Uint8Array;
};

type Indexed = Awaited<ReturnType<SprClient["getShieldedTransactionsByTags"]>>["data"][number]["events"][number];

/**
 * The maker's orders rebuilt from the chain: a port of `sdk/src/index/maker.rs`
 * (`index_maker`, `scan_maker`). A transaction viewing key re-derived from the
 * maker's viewing key and an input nullifier proves the maker wrote the
 * transaction; it decrypts each confidential slot from the sender side, and an
 * opening counts only if the WASM decodes it as order terms whose commitment is
 * the slot's leaf under the maker's order authority.
 */
export async function scanMaker(ctx: SwapContext): Promise<FoundOrder[]> {
  const { owner } = await orderAuthority(ctx.payer, ctx.programId);
  const tag = owner.publicKey().confidentialViewTag();
  const found: FoundOrder[] = [];
  let paginationToken: Parameters<SprClient["getShieldedTransactionsByTags"]>[0]["paginationToken"];
  do {
    const page = await ctx.client.getShieldedTransactionsByTags({ tags: [tag], ...(paginationToken ? { paginationToken } : {}) });
    for (const tx of page.data.flatMap((g) => g.events)) {
      const order = await scanOne(ctx, tx, owner);
      if (order) found.push(order);
    }
    paginationToken = page.paginationToken ?? undefined;
  } while (paginationToken);
  return found;
}

/**
 * The transaction viewing key this maker would have used for `tx`, re-derived from its viewing
 * key and each input nullifier; undefined when another wallet wrote the transaction.
 */
export async function writerKey(keys: Pick<LocalKeys, "address" | "transactionKeys">, tx: Pick<Indexed, "txViewingPublicKey" | "nullifiers">) {
  const { txViewingPublicKey } = tx;
  if (!txViewingPublicKey) return undefined;
  const viewingPublicKey = keys.address().viewingPublicKey;
  const candidates = tx.nullifiers.length
    ? await keys.transactionKeys(tx.nullifiers.map((firstNullifier) => ({ viewingPublicKey, firstNullifier })))
    : [];
  const txKey = candidates.find((k) => equal(k.publicKey().toBytes(), txViewingPublicKey.toBytes()));
  for (const k of candidates) if (k !== txKey) k.destroy();
  return txKey;
}

/** `from_data_utxo`: the terms must name this maker, by its Solana key and its nullifier key. */
export function termsNameMaker(maker: unknown, payer: Address, nullifierPublicKey: Uint8Array): boolean {
  const m = maker as { key?: { bytes?: Record<string, number> }; nullifierPk?: string };
  const makerKey = Uint8Array.from({ length: 32 }, (_, i) => m.key?.bytes?.[i] ?? -1);
  return equal(makerKey, addressBytes(payer)) && m.nullifierPk === decimal(nullifierPublicKey);
}

export async function scanOne(ctx: SwapContext, tx: Indexed, owner: ProgramOwner): Promise<FoundOrder | undefined> {
  if (!tx.salt) return undefined;
  const salt = tx.salt;
  const txKey = await writerKey(ctx.keys, tx);
  if (!txKey) return undefined;
  try {
    // Ciphertext slot indexes count data-bearing slots only, as `unified_slots` does.
    let slotIndex = -1;
    for (const slot of tx.outputSlots) {
      let frame: ReturnType<typeof decodeOutputData>;
      try {
        frame = decodeOutputData(slot.payload);
      } catch {
        continue;
      }
      slotIndex++;
      if (frame.encoding !== "encrypted" || frame.scheme !== EncryptedScheme.confidential || frame.body.length < 33) continue;
      const found = await openSlot(ctx, owner, slot, frame.body, txKey, salt, slotIndex).catch(() => undefined);
      if (found) return found;
    }
    return undefined;
  } finally {
    txKey.destroy();
  }
}

async function openSlot(
  ctx: SwapContext,
  owner: ProgramOwner,
  slot: Indexed["outputSlots"][number],
  body: Uint8Array,
  txKey: Awaited<ReturnType<LocalKeys["transactionKeys"]>>[number],
  salt: NonNullable<Indexed["salt"]>,
  slotIndex: number,
): Promise<FoundOrder | undefined> {
  const takerViewingKey = body.slice(0, 33);
  const plain = decodeConfidential(txKey.decryptSlotEphemeral(X25519PublicKey.fromBytes(takerViewingKey), body.slice(33), salt, slotIndex));
  const sellMint = ctx.wallet.registry.resolve(plain.assetId);
  const asset = { asset: sellMint, assetId: plain.assetId };
  const data = { records: plain.data.records().map((r) => ({ kind: r.kind, bytes: new Uint8Array(r.bytes) })) };
  // Throws unless the opening, owned by this maker's order authority, reproduces the slot's leaf.
  const decoded = await callWorker<TermsUtxo>({
    op: "orderTerms",
    utxo: {
      utxo: { owner: owner.publicKey().toBytes(), asset, amount: plain.amount, blinding: plain.blinding, data },
      nullifierPubkey: ProgramOwner.nullifierPublicKey(),
      utxoHash: slot.outputContext.hash,
      nullifier: new Uint8Array(32),
      treeId: slot.outputContext.treeId ?? TREE_ID,
      leafIndex: slot.outputContext.leafIndex,
    },
  });
  if (!termsNameMaker(decoded.data.maker, ctx.payer, ctx.keys.address().nullifierPublicKey)) return undefined;
  const output: RawOutput = {
    asset,
    amount: plain.amount,
    blinding: new Uint8Array(plain.blinding),
    data,
    ...(decoded.utxo.dataHash ? { dataHash: new Uint8Array(decoded.utxo.dataHash) } : {}),
  };
  return {
    opening: { orderHash: new Uint8Array(slot.outputContext.hash), output },
    terms: decoded.data,
    sellMint,
    buyMint: mintOf(ctx, decoded.data.destinationMint),
    takerViewingKey,
  };
}
