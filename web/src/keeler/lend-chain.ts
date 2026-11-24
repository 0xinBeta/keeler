// The real `LendChain`, `LendBuilder` and `LendWasm` behind lend.ts: the swap client's patterns (swap.ts) applied
// to the lend program's user PDA. A port of lend/sdk/src/builders.rs (`intent`, `cancel_intent`); `close` follows
// the assumed layout in lend-layout.ts. Every read error propagates: a failed read is never taken for a status.
import {
  AccountRole,
  address,
  getBase64Encoder,
  getProgramDerivedAddress,
  signature as toSignature,
  type Address,
  type Instruction,
} from "@solana/kit";
import { SHIELDED_POOL_PROGRAM_ID } from "@heliuslabs/spr";
import { NullifierKeyProofAuthority, atSlot, type SprClient } from "@heliuslabs/spr/client";
import { bytes32, poseidon, solanaOwnerIdentity, X25519PublicKey } from "@heliuslabs/spr/keypair";
import { ProgramOwner, toProgramWalletUtxo } from "@heliuslabs/spr/program";
import { decodeConfidential, decodeOutputData, EncryptedScheme } from "@heliuslabs/spr/transaction";
import { encodeTransactInstructionData, nullifierPdaAccounts } from "@heliuslabs/spr/internal";
import { resolveRegisteredAddress } from "@heliuslabs/spr/wallet";
import type { FoundIntent, LendBuilder, LendChain, LendWasm } from "./lend";
import { NOTE_KIND, SLOT, TAG, TREE_ID, domainField, u64Bytes } from "./lend-layout";
import type { NoteOpening } from "./lend-store";
import { callWorker, type RawOutput, type RawProgramTransaction } from "./swap-wasm";
import {
  addressBytes,
  compile,
  decimal,
  decode as decodeTx,
  ixData,
  markerData,
  prove,
  proveSpp,
  sppInputs,
  termsNameMaker,
  toHex,
  writerKey,
  type SwapContext,
} from "./swap";

const SYSTEM_PROGRAM = address("11111111111111111111111111111111");
const USER_SEED = new TextEncoder().encode("user");
const equal = (a: Uint8Array, b: Uint8Array) => a.length === b.length && a.every((x, i) => x === b[i]);

/** The swap context with the lend program and its keeper in place of the swap program. */
export type LendContext = SwapContext & { keeper: Address };

const w = (a: Address) => ({ address: a, role: AccountRole.WRITABLE });
const ws = (a: Address) => ({ address: a, role: AccountRole.WRITABLE_SIGNER });
const r = (a: Address) => ({ address: a, role: AccountRole.READONLY });
const rs = (a: Address) => ({ address: a, role: AccountRole.READONLY_SIGNER });

/** `["user", wallet]`: owns every intent, receipt and position of `wallet`. */
export async function userPda(wallet: Address, programId: Address) {
  const [pda, bump] = await getProgramDerivedAddress({ programAddress: programId, seeds: [USER_SEED, addressBytes(wallet)] });
  return { pda, bump, owner: new ProgramOwner(pda) };
}

/**
 * The account list of `lend/sdk/src/lib.rs` `instruction`: the user (writable signer), then the SPP transact
 * accounts with one tree context, and the owner signers (`owners`; the PDA goes in as a non-signer).
 */
export function userAccounts(user: Address, tree: Address, nullifierPdas: Address[], owners: { address: Address; role: AccountRole }[]) {
  return [ws(user), ws(user), w(tree), r(SHIELDED_POOL_PROGRAM_ID), r(SYSTEM_PROGRAM), w(tree), ...nullifierPdas.map(w), ...owners];
}

/** `mint_halves`: the mint's 32 bytes as two big-endian u128s. */
export function mintHalves(mint: Address): { mintHi: bigint; mintLo: bigint } {
  const b = addressBytes(mint);
  return { mintHi: BigInt(`0x${toHex(b.subarray(0, 16))}`), mintLo: BigInt(`0x${toHex(b.subarray(16))}`) };
}

/** A note the user PDA owns, as the WASM's `WalletUtxo` input. */
function pdaUtxo(ctx: LendContext, owner: ProgramOwner, n: { hash: Uint8Array; mint: string; amount: bigint; blinding: Uint8Array; state?: Uint8Array; dataHash?: Uint8Array; data?: RawOutput["data"] }, leafIndex: bigint | undefined) {
  const mint = address(n.mint);
  return {
    utxo: {
      owner: owner.publicKey().toBytes(),
      asset: { asset: mint, assetId: ctx.wallet.registry.assetId(mint) },
      amount: n.amount,
      blinding: n.blinding,
      // shortcut: the state record kind is assumed to be `utxoData` (as the program's notes write it); check it against a real note once the keeper fills one.
      data: n.data ?? { records: n.state ? [{ kind: "utxoData", bytes: n.state }] : [] },
    },
    nullifierPubkey: ProgramOwner.nullifierPublicKey(),
    utxoHash: n.hash,
    nullifier: ProgramOwner.nullifierKey().nullifier(bytes32(n.hash), bytes32(n.blinding)),
    ...(n.dataHash ? { dataHash: n.dataHash } : {}),
    treeId: TREE_ID,
    ...(leafIndex === undefined ? {} : { leafIndex }),
  };
}

export function createLendSeams(ctx: () => Promise<LendContext>) {
  let keeperReg: Promise<NonNullable<Awaited<ReturnType<typeof resolveRegisteredAddress>>>> | undefined;
  /** The keeper's registered shielded address: intents and notes are encrypted to its viewing key. */
  const keeperAddress = (c: LendContext) =>
    (keeperReg ??= resolveRegisteredAddress({ rpc: c.client, owner: c.keeper }).then((k) => {
      if (!k) throw new Error(`the keeper ${c.keeper} is not in the user registry`);
      return k;
    })).catch((e) => {
      keeperReg = undefined;
      throw e;
    });

  const pdaOf = async (c: LendContext) => {
    const u = await userPda(c.payer, c.programId);
    const viewing = (await keeperAddress(c)).address.viewingPublicKey;
    return { ...u, viewing, shielded: u.owner.address(viewing).toBytes() };
  };

  const makerIdentity = (c: LendContext) => decimal(solanaOwnerIdentity(addressBytes(c.payer)));

  const intentInstruction = async (c: LendContext, tag: number, args: Uint8Array, raw: RawProgramTransaction, circuit: "intent" | "cancel_intent" | "close", owners: { address: Address; role: AccountRole }[], marker?: Uint8Array) => {
    const tx = await decodeTx(c, raw);
    const keeper = await keeperAddress(c);
    const spp = await sppInputs(c, tx, marker ? [{ viewTag: keeper.address.signingPublicKey.confidentialViewTag(), data: marker }] : []);
    // An intent spends the user's own note; a cancel or close spends a PDA note, completed under the zero nullifier key.
    const authority = circuit === "intent" ? c.keys : new NullifierKeyProofAuthority(ProgramOwner.nullifierKey(), c.client.proofService);
    const transact = await proveSpp(c, tx, spp, authority);
    const proof = await prove(c, circuit, raw);
    const pdas = (await nullifierPdaAccounts(c.client.tree, transact.inputs.map((i) => i.nullifierHash))).map((m) => m.address);
    const ix: Instruction = {
      programAddress: c.programId,
      accounts: userAccounts(c.payer, c.client.tree, pdas, owners),
      data: ixData(tag, proof, args, encodeTransactInstructionData(transact)),
    };
    return { tx, built: await compile(c, ix) };
  };

  const build: LendBuilder = {
    async intent(p, onProving) {
      const c = await ctx();
      const sender = c.keys.address();
      const user = await pdaOf(c);
      const mint = address(p.mint);
      const inputs = {
        private: {
          txContext: { outputTreeId: TREE_ID },
          source: toProgramWalletUtxo(p.source as never, { nullifierPublicKey: sender.nullifierPublicKey, treeId: TREE_ID, assets: c.wallet.registry }),
          amount: p.amount,
          purpose: p.purpose,
          target: p.target,
          expiry: BigInt(p.expiry),
        },
        public: { userOwner: user.shielded, makerIdentity: makerIdentity(c), ...mintHalves(mint) },
      };
      onProving();
      const raw = await callWorker<RawProgramTransaction>({ op: "lendIntent", inputs, sender: sender.toBytes(), payer: c.payer });
      const hash = raw.finalizedTx.outputHashes[SLOT.intent.intent];
      const output = raw.finalizedTx.outputUtxos[SLOT.intent.intent];
      if (!hash || !output) throw new Error("the intent transaction has no intent output");
      const args = Uint8Array.of(...addressBytes(mint), ...user.viewing.toBytes(), user.bump);
      // The marker on the keeper's view tag tells it which wallet's PDA to scan, as the swap's make does for the dealer.
      const { built } = await intentInstruction(c, TAG.intent, args, raw, "intent", [rs(c.payer), r(user.pda)], markerData(hash, c.payer));
      return { ...built, hash: new Uint8Array(hash), output };
    },

    async cancel(intent, onProving) {
      const c = await ctx();
      const user = await pdaOf(c);
      const intentUtxo = pdaUtxo(c, user.owner, { ...intent, mint: intent.output.asset.asset, amount: intent.output.amount, blinding: intent.output.blinding, ...(intent.output.dataHash ? { dataHash: intent.output.dataHash } : {}), ...(intent.output.data ? { data: intent.output.data } : {}) }, intent.leafIndex);
      const { data: state } = await callWorker<{ data: { maker: unknown; purpose: number; target: bigint; expiry: bigint } }>({ op: "lendState", kind: "intent", utxo: intentUtxo });
      const inputs = {
        private: { txContext: { outputTreeId: TREE_ID }, intentUtxo, intentState: { maker: state.maker, purpose: state.purpose, target: state.target, expiry: state.expiry } },
        public: { userOwner: user.shielded, makerIdentity: makerIdentity(c), expiry: state.expiry },
      };
      onProving();
      const raw = await callWorker<RawProgramTransaction>({ op: "lendCancel", inputs, sender: c.keys.address().toBytes(), payer: c.payer });
      const args = Uint8Array.of(...user.viewing.toBytes(), user.bump, ...u64Bytes(state.expiry));
      return (await intentInstruction(c, TAG.cancelIntent, args, raw, "cancel_intent", [r(user.pda)])).built;
    },

    async close(position, onProving) {
      const c = await ctx();
      const user = await pdaOf(c);
      const positionUtxo = pdaUtxo(c, user.owner, position, position.leafIndex);
      const { data: state } = await callWorker<{ data: unknown }>({ op: "lendState", kind: "position", utxo: positionUtxo });
      // shortcut: the close circuit's inputs are assumed to mirror cancel_intent's; change them when Vault builds close.
      const inputs = {
        private: { txContext: { outputTreeId: TREE_ID }, positionUtxo, positionState: state },
        public: { userOwner: user.shielded, makerIdentity: makerIdentity(c) },
      };
      onProving();
      const raw = await callWorker<RawProgramTransaction>({ op: "lendClose", inputs, sender: c.keys.address().toBytes(), payer: c.payer });
      const args = Uint8Array.of(...addressBytes(address(position.mint)), ...user.viewing.toBytes(), user.bump);
      return (await intentInstruction(c, TAG.close, args, raw, "close", [r(user.pda)])).built;
    },
  };

  type Indexed = Awaited<ReturnType<SprClient["getShieldedTransactionsByTags"]>>["data"][number]["events"][number];

  /** One intent slot of a transaction this wallet wrote: decrypted, decoded as an intent and checked to name this wallet. */
  const openIntent = async (c: LendContext, owner: ProgramOwner, slot: Indexed["outputSlots"][number], body: Uint8Array, txKey: NonNullable<Awaited<ReturnType<typeof writerKey>>>, salt: NonNullable<Indexed["salt"]>, slotIndex: number) => {
    const plain = decodeConfidential(txKey.decryptSlotEphemeral(X25519PublicKey.fromBytes(body.slice(0, 33)), body.slice(33), salt, slotIndex));
    const mint = c.wallet.registry.resolve(plain.assetId);
    const data = { records: plain.data.records().map((x) => ({ kind: x.kind, bytes: new Uint8Array(x.bytes) })) };
    const decoded = await callWorker<{ utxo: { dataHash?: Uint8Array }; data: { maker: unknown; purpose: number; target: bigint; expiry: bigint } }>({
      op: "lendState",
      kind: "intent",
      utxo: pdaUtxo(c, owner, { hash: slot.outputContext.hash, mint, amount: plain.amount, blinding: plain.blinding, data }, slot.outputContext.leafIndex),
    });
    if (!termsNameMaker(decoded.data.maker, c.payer, c.keys.address().nullifierPublicKey)) return undefined;
    const output: RawOutput = {
      asset: { asset: mint, assetId: plain.assetId },
      amount: plain.amount,
      blinding: new Uint8Array(plain.blinding),
      data,
      ...(decoded.utxo.dataHash ? { dataHash: new Uint8Array(decoded.utxo.dataHash) } : {}),
    };
    return { hash: new Uint8Array(slot.outputContext.hash), output, mint, amount: plain.amount, purpose: decoded.data.purpose, target: BigInt(decoded.data.target), expiry: Number(decoded.data.expiry) };
  };

  const chain: LendChain = {
    async note(hash, blinding) {
      const c = await ctx();
      const leafIndex = (await c.client.getMerkleProofs(c.client.tree, [bytes32(hash)])).proofs[0]?.leafIndex;
      const nullifier = ProgramOwner.nullifierKey().nullifier(bytes32(hash), bytes32(blinding));
      const page = await c.client.getShieldedTransactionsByNullifiers({ nullifiers: [nullifier as never] }, atSlot(await c.client.getSlot()));
      const group = page.data.find((g) => g.events.some((e) => e.nullifiers.some((n) => equal(n, nullifier))));
      return { ...(leafIndex === undefined ? {} : { leafIndex }), ...(group ? { spentBy: group.signature } : {}) };
    },
    async signatureStatus(sig) {
      const c = await ctx();
      return (await c.client.solanaRpc.getSignatureStatuses([toSignature(sig)], { searchTransactionHistory: true }).send()).value[0] ?? null;
    },
    async blockHeight() {
      return (await ctx()).client.solanaRpc.getBlockHeight().send();
    },
    async transaction(sig) {
      const c = await ctx();
      const tx = await c.client.solanaRpc.getTransaction(toSignature(sig), { encoding: "base64", maxSupportedTransactionVersion: 1, commitment: "confirmed" }).send();
      return tx ? new Uint8Array(getBase64Encoder().encode(tx.transaction[0])) : null;
    },
    /** A port of the swap's `scanMaker` over the user PDA's view tag. */
    async scanIntents() {
      const c = await ctx();
      const { owner } = await userPda(c.payer, c.programId);
      const tag = owner.publicKey().confidentialViewTag();
      const found: FoundIntent[] = [];
      let paginationToken: Parameters<SprClient["getShieldedTransactionsByTags"]>[0]["paginationToken"];
      do {
        const page = await c.client.getShieldedTransactionsByTags({ tags: [tag], ...(paginationToken ? { paginationToken } : {}) });
        for (const group of page.data) {
          for (const tx of group.events) {
            if (!tx.salt) continue;
            const txKey = await writerKey(c.keys, tx);
            if (!txKey) continue;
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
                const intent = await openIntent(c, owner, slot, frame.body, txKey, tx.salt, slotIndex).catch(() => undefined);
                if (intent) found.push({ ...intent, signature: group.signature });
              }
            } finally {
              txKey.destroy();
            }
          }
        }
        paginationToken = page.paginationToken ?? undefined;
      } while (paginationToken);
      return found;
    },
  };

  const wasm: LendWasm = {
    blinding: (firstNullifier, seed, slot) => callWorker<Uint8Array>({ op: "lendBlinding", firstNullifier, seed, slot }),
    hash: (n) => callWorker<Uint8Array>({ op: "lendNoteHash", owner: n.owner, mint: n.mint, amount: n.amount, kind: n.kind, state: n.state, blinding: n.blinding, treeId: TREE_ID }),
    seed: (domain, previous) => new Uint8Array(poseidon([domainField(domain), previous])),
    nullifier: (hash, blinding) => new Uint8Array(ProgramOwner.nullifierKey().nullifier(bytes32(hash), bytes32(blinding))),
    async decode(note: NoteOpening) {
      const c = await ctx();
      const { owner } = await userPda(c.payer, c.programId);
      const kind = note.kind === NOTE_KIND.receipt ? "receipt" : note.kind === NOTE_KIND.position ? "position" : undefined;
      if (!kind) throw new Error(`note kind ${note.kind} is not a receipt or a position`);
      // Throws unless the opening, owned by this wallet's user PDA, reproduces `note.hash`.
      const { data } = await callWorker<{ data: { owner: unknown; pool?: bigint; supplyShares?: bigint; vault?: bigint; positionId?: bigint; debtShares?: bigint } }>({
        op: "lendState",
        kind,
        utxo: pdaUtxo(c, owner, note, undefined),
      });
      const ownerIsMe = termsNameMaker(data.owner, c.payer, c.keys.address().nullifierPublicKey);
      return kind === "receipt"
        ? { ownerIsMe, pool: BigInt(data.pool!), shares: BigInt(data.supplyShares!) }
        : { ownerIsMe, vault: BigInt(data.vault!), positionId: BigInt(data.positionId!), shares: BigInt(data.debtShares!) };
    },
  };

  const owners = async () => {
    const c = await ctx();
    return { user: c.keys.address().toBytes(), pda: (await pdaOf(c)).shielded };
  };

  return { build, chain, wasm, owners };
}
