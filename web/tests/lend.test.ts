// The lend client's flows against a fake keeper (which verifies every signed request as docs/lend-api.md says)
// and a fake chain. Proving and note hashing are fakes too; their real versions are the WASM's.
import { createHash } from "node:crypto";
import {
  AccountRole,
  address,
  appendTransactionMessageInstructions,
  compileTransaction,
  createKeyPairFromPrivateKeyBytes,
  createTransactionMessage,
  getAddressEncoder,
  getAddressFromPublicKey,
  getBase64Decoder,
  getSignatureFromTransaction,
  getTransactionDecoder,
  getTransactionEncoder,
  pipe,
  setTransactionMessageFeePayer,
  setTransactionMessageLifetimeUsingBlockhash,
  signBytes,
  type Address,
  type Blockhash,
  type Instruction,
  type Transaction,
} from "@solana/kit";
import { encodeTransactInstructionData } from "@heliuslabs/spr/internal";
import { beforeEach, describe, expect, it } from "vitest";
import { KeelerError, type LendStep } from "../src/keeler/index";
import { hex, ticketMessage, verifyRequest } from "../src/keeler/lend-auth";
import { COMPUTE_BUDGET_PROGRAM } from "../src/keeler/lend-check";
import { ARGS, SEED_DOMAIN, SLOT, TAG, u64At, u64Bytes } from "../src/keeler/lend-layout";
import { createLend, keeperError, parseOpening, vaultPrice, type FoundIntent, type LendDeps, type LendServerConfig, type NoteState } from "../src/keeler/lend";
import { lendStore } from "../src/keeler/lend-store";

const NETWORK = "devnet";
const PROGRAM = address("A7qaBKJ1gJRcKwMWyyRdr9YhmbBmnMaemM9Sijah3EbC");
const TUSD = "CktRuQ2mttgRGkXJtyksdKHjUdc2C4TgDzyB98oEzy8";
const TSOL = "8qbHbw2BbbTHBW1sbeqakYXVKRQM8Ne7pLK7m6CVfeR";
const T0 = 1_800_000_000;
const assets = [
  { mint: TUSD, symbol: "tUSD", decimals: 6 },
  { mint: TSOL, symbol: "tSOL", decimals: 9 },
];
const config: LendServerConfig = {
  programId: PROGRAM,
  pools: [{ mint: TUSD, curve: [{ utilisationBps: 0, rateBps: 0 }, { utilisationBps: 10_000, rateBps: 1500 }], feeBps: 1000, maxUtilisationBps: 9500, minSupply: "1000000" }],
  vaults: [{ id: 1, collateralMint: TSOL, debtMint: TUSD, cfBps: 8000, ltBps: 8500, lmlBps: 9000, penaltyBps: 100, minCollateral: "50000000", minDebt: "5000000" }],
};
const USER_ADDR = new Uint8Array(64).fill(1);
const PDA_ADDR = new Uint8Array(64).fill(2);
const RESERVE_NF = new Uint8Array(32).fill(0xaa);
const INDEX = 1_100_000_000_000n; // 1.1 tUSD per share
const TSOL_PRICE = { price: "14237000000", expo: -8, publishTime: T0 }; // $142.37

const sha = (...parts: (Uint8Array | string)[]) => {
  const h = createHash("sha256");
  parts.forEach((p) => h.update(typeof p === "string" ? new TextEncoder().encode(p) : p));
  return new Uint8Array(h.digest());
};
const mintBytes = (m: string) => new Uint8Array(getAddressEncoder().encode(address(m)));
const wasm: LendDeps["wasm"] = {
  blinding: async (first, seed, slot) => sha(first, seed, Uint8Array.of(slot)),
  hash: async (n) => sha(n.owner, n.mint, u64Bytes(n.amount), Uint8Array.of(n.kind), n.state, n.blinding),
  seed: (domain, prev) => sha(domain, prev),
  nullifier: (hash, blinding) => sha("nf", hash, blinding),
  // Test states: 16 opaque bytes, then the pool or position id, then the shares.
  decode: async (n) => ({ ownerIsMe: n.state[0] !== 0xee, shares: u64At(n.state, n.state.length - 8), positionId: u64At(n.state, n.state.length - 16), pool: u64At(n.state, n.state.length - 16) }),
};
const state = (id: bigint, shares: bigint, owner = 0x33) => Uint8Array.of(...new Uint8Array(16).fill(owner), ...u64Bytes(id), ...u64Bytes(shares));
const opening = (amount: bigint, blinding: Uint8Array, s: Uint8Array) => getBase64Decoder().decode(Uint8Array.of(...u64Bytes(amount), ...blinding, ...s));

const w = (a: Address) => ({ address: a, role: AccountRole.WRITABLE });
const ws = (a: Address) => ({ address: a, role: AccountRole.WRITABLE_SIGNER });
const rs = (a: Address) => ({ address: a, role: AccountRole.READONLY_SIGNER });

function memory(): Storage {
  const m = new Map<string, string>();
  return {
    get length() {
      return m.size;
    },
    key: (i) => [...m.keys()][i] ?? null,
    getItem: (k) => m.get(k) ?? null,
    setItem: (k, v) => void m.set(k, v),
    removeItem: (k) => void m.delete(k),
    clear: () => m.clear(),
  };
}

const wire = (feePayer: Address, ixs: Instruction[]) =>
  new Uint8Array(
    getTransactionEncoder().encode(
      compileTransaction(
        pipe(
          createTransactionMessage({ version: 0 }),
          (m) => setTransactionMessageFeePayer(feePayer, m),
          (m) => setTransactionMessageLifetimeUsingBlockhash({ blockhash: "11111111111111111111111111111111" as Blockhash, lastValidBlockHeight: 0n }, m),
          (m) => appendTransactionMessageInstructions(ixs, m),
        ),
      ),
    ),
  );

type World = Awaited<ReturnType<typeof world>>;

async function world() {
  const userKeys = await createKeyPairFromPrivateKeyBytes(new Uint8Array(32).fill(0x11));
  const keeperKeys = await createKeyPairFromPrivateKeyBytes(new Uint8Array(32).fill(0x22));
  const USER = await getAddressFromPublicKey(userKeys.publicKey);
  const KEEPER = await getAddressFromPublicKey(keeperKeys.publicKey);
  const clock = { now: T0 };
  const notes = new Map<string, NoteState>();
  const sigs = new Map<string, { err: unknown; confirmationStatus: string }>();
  const txs = new Map<string, Uint8Array>();
  const found: FoundIntent[] = [];
  const log = { signMessage: 0, signed: [] as Transaction[], sent: [] as Transaction[], requests: [] as { path: string; body: Record<string, unknown> }[], cancels: 0, closes: 0 };
  const keeper = {
    figures: [{ mint: TUSD, at: T0 - 30, size: "2400000000000", utilisationBps: 7200, supplyRateBps: 291, borrowRateBps: 450 }],
    account: { receipts: [] as unknown[], positions: [] as unknown[], intents: [] as unknown[] },
    ticket: (b: Record<string, unknown>) => ({ id: "ab".repeat(16), expiry: clock.now + 300, purpose: b.purpose, amount: b.amount }),
    ticketSigner: keeperKeys,
    build: undefined as undefined | ((b: Record<string, unknown>) => Promise<unknown>),
    status: 200,
    paused: false,
    /** Position ids the keeper knows: a deposit to one adds collateral, which its pause allows. */
    positions: new Set<string>(),
  };
  const reply = (status: number, body: unknown) => new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
  const PURPOSE: Record<string, number> = { supply: 1, deposit: 2, repay: 3 };

  const http: LendDeps["http"] = async (path, init) => {
    const [route, query] = path.split("?");
    if (route === "/api/prices") return reply(200, { [TSOL]: TSOL_PRICE });
    if (route === "/api/lend/pools") return reply(200, { pools: keeper.figures });
    const body = (init?.body ? JSON.parse(String(init.body)) : Object.fromEntries(new URLSearchParams(query))) as Record<string, string | number>;
    log.requests.push({ path: route!, body });
    if (keeper.status !== 200) return reply(keeper.status, { error: "The keeper says no." });
    const ok = await verifyRequest(NETWORK, PROGRAM, init?.method === "POST" ? "POST" : "GET", route!.slice(4), body);
    if (!ok || body.wallet !== USER) return reply(401, { error: "Bad signature." });
    if (Math.abs(Number(body.timestamp) - clock.now) > 120) return reply(401, { error: "Stale." });
    if (route === "/api/lend/account") return reply(200, keeper.account);
    // The keeper's own pause: new supply, new positions and borrowing only.
    const opening = (route === "/api/lend/ticket" && (body.purpose === "supply" || (body.purpose === "deposit" && !keeper.positions.has(String(body.position))))) || (route === "/api/lend/build" && body.op === "draw" && body.borrow !== "0");
    if (keeper.paused && opening) return reply(403, { error: "Lending is paused for new deposits and borrowing." });
    if (route === "/api/lend/ticket") {
      const t = keeper.ticket(body);
      const target = body.pool ? 1n : BigInt(String(body.position));
      const sig = await signBytes(keeper.ticketSigner.privateKey, ticketMessage({ programId: PROGRAM, wallet: USER, id: t.id, purpose: PURPOSE[String(t.purpose)]!, target, amount: BigInt(String(t.amount)), expiry: t.expiry }));
      return reply(200, { id: t.id, expiry: t.expiry, signature: hex(sig) });
    }
    if (route === "/api/lend/build") return reply(200, await keeper.build!(body));
    return reply(404, { error: "Not found." });
  };

  const sign = async (tx: Transaction): Promise<Transaction> => {
    log.signed.push(tx);
    const filled = Object.fromEntries(Object.entries(tx.signatures).map(([k, v]) => [k, v ?? new Uint8Array(64).fill(log.signed.length)]));
    return { ...tx, signatures: filled } as Transaction;
  };
  const send = async (tx: Transaction) => {
    log.sent.push(tx);
    const s = getSignatureFromTransaction(tx);
    sigs.set(s, { err: null, confirmationStatus: "confirmed" });
    txs.set(s, new Uint8Array(tx.messageBytes.length)); // replaced below for transactions whose content matters
    return 1n;
  };

  const store = lendStore(NETWORK, PROGRAM, USER, memory());
  const deps: LendDeps = {
    network: NETWORK,
    programId: PROGRAM,
    keeper: KEEPER,
    config,
    assets,
    wallet: () => USER,
    http,
    chainNow: async () => clock.now,
    sessionSeed: () => new Uint8Array(32).fill(0x44),
    signMessage: async (m) => {
      log.signMessage++;
      return new Uint8Array(await signBytes(userKeys.privateKey, m));
    },
    sign,
    send,
    sync: async () => {},
    notes: (mint) => (mint === TUSD ? [{ amount: 500_000000n, utxo: "tusd-note" }] : [{ amount: 5_000000000n, utxo: "tsol-note" }]),
    owners: async () => ({ user: USER_ADDR, pda: PDA_ADDR, viewing: new Uint8Array(33).fill(6) }),
    chain: {
      note: async (hash) => notes.get(hex(hash)) ?? {},
      signatureStatus: async (s) => sigs.get(s) ?? null,
      blockHeight: async () => 100n,
      transaction: async (s) => txs.get(s) ?? null,
      scanIntents: async () => found,
    },
    build: {
      intent: async (p, onProving) => {
        onProving();
        const hash = sha("intent", p.mint, u64Bytes(p.amount), u64Bytes(p.target), u64Bytes(BigInt(p.expiry)));
        const transaction = decode(wire(USER, [{ programAddress: PROGRAM, accounts: [ws(USER)], data: Uint8Array.of(TAG.intent, ...hash) }]));
        return { transaction, lastValidBlockHeight: 200n, hash, output: { asset: { asset: p.mint, assetId: 1n }, amount: p.amount, blinding: sha("b", hash) } };
      },
      cancel: async () => {
        log.cancels++;
        return { transaction: decode(wire(USER, [{ programAddress: PROGRAM, accounts: [ws(USER)], data: Uint8Array.of(TAG.cancelIntent) }])), lastValidBlockHeight: 200n };
      },
      close: async () => {
        log.closes++;
        return { transaction: decode(wire(USER, [{ programAddress: PROGRAM, accounts: [ws(USER)], data: Uint8Array.of(TAG.close) }])), lastValidBlockHeight: 200n };
      },
    },
    wasm,
    store: () => store,
  };
  return { deps, lend: createLend(deps), clock, notes, sigs, txs, found, log, keeper, store, USER, KEEPER, keeperKeys };
}

const decode = (bytes: Uint8Array): Transaction => getTransactionDecoder().decode(bytes);

/** The keeper's honest build of a redeem or draw for `spent`, the note it spends; `tamper` changes a note first. */
async function keeperBuild(W: World, op: "redeem" | "draw", spent: { hash: Uint8Array; blinding: Uint8Array }, notes: { slot: number; owner: Uint8Array; mint: Uint8Array; amount: bigint; kind: number; state: Uint8Array }[], extra: Instruction[] = [], bound?: bigint) {
  const seed = wasm.seed(SEED_DOMAIN[op], spent.blinding);
  const first = wasm.nullifier(spent.hash, spent.blinding);
  const outputs = await Promise.all([0, 1, 2, 3].map(async (slot) => {
    const n = notes.find((x) => x.slot === slot);
    return n ? wasm.hash({ ...n, blinding: await wasm.blinding(first, seed, slot) }) : sha("pad", Uint8Array.of(slot));
  }));
  const transact = encodeTransactInstructionData({
    expiryUnixTs: BigInt(W.clock.now + 60),
    txViewingPk: new Uint8Array(33).fill(2),
    salt: new Uint8Array(16),
    interfaceTransfers: [],
    outputs: outputs.map((utxoHash) => ({ utxoHash, ownerTag: { kind: "inline" as const, value: new Uint8Array(32) } })),
    messages: [],
    privateTxHash: new Uint8Array(32),
    circuit: { kind: "confidentialSigner", inputs: 2, outputs: 4, publicAssetSlots: 0 },
    proof: { a: new Uint8Array(32), b: new Uint8Array(128), c: new Uint8Array(32) },
    inputs: [{ nullifierHash: first, treeIndex: 0 }, { nullifierHash: RESERVE_NF, treeIndex: 0 }],
    treeContexts: [{ utxoTreeRootIndex: 0, nullifierTreeRootIndex: 0 }],
  } as never);
  const layout = op === "redeem" ? ARGS.redeem : ARGS.draw;
  const args = new Uint8Array(layout.length);
  args.set(u64Bytes(BigInt(W.clock.now)), layout.now);
  args.set(new Uint8Array(33).fill(6), layout.ownerViewing);
  if (op === "draw") args.set(u64Bytes(bound!), ARGS.draw.bound);
  const price = op === "draw" ? [{ address: address("SysvarS1otHashes111111111111111111111111111"), role: AccountRole.READONLY }] : [];
  const ix: Instruction = { programAddress: PROGRAM, accounts: [ws(W.KEEPER), rs(W.USER), ...price, ws(W.KEEPER), w(address(TUSD))], data: Uint8Array.of(op === "redeem" ? TAG.redeem : TAG.draw, ...args, ...transact) };
  const tx = wire(W.KEEPER, [{ programAddress: COMPUTE_BUDGET_PROGRAM, data: Uint8Array.of(2, 0, 0, 0x15, 0) }, ix, ...extra]);
  return { transaction: getBase64Decoder().decode(tx), outputs: outputs.map(hex), now: W.clock.now, index: INDEX.toString(), ...(bound === undefined ? {} : { bound: bound.toString() }) };
}

/** A receipt of `shares` the keeper's account view offers, with its note on chain. */
function giveReceipt(W: World, shares: bigint, opts: { onChain?: boolean; owner?: number } = {}) {
  const blinding = sha("receipt-blinding", u64Bytes(shares));
  const s = state(1n, shares, opts.owner);
  W.keeper.account.receipts.push({ pool: TUSD, shares: shares.toString(), message: opening(0n, blinding, s) });
  return wasm.hash({ slot: 0, owner: PDA_ADDR, mint: mintBytes(TUSD), amount: 0n, kind: 2, state: s, blinding }).then((hash) => {
    if (opts.onChain !== false) W.notes.set(hex(hash), { leafIndex: 7n });
    return { hash, blinding, state: s };
  });
}

function givePosition(W: World, id: bigint, collateral: bigint, debtShares: bigint) {
  const blinding = sha("position-blinding", u64Bytes(id));
  const s = state(id, debtShares);
  W.keeper.account.positions.push({ vault: 1, position: id.toString(), collateral: collateral.toString(), debtShares: debtShares.toString(), message: opening(collateral, blinding, s) });
  return wasm.hash({ slot: 0, owner: PDA_ADDR, mint: mintBytes(TSOL), amount: collateral, kind: 3, state: s, blinding }).then((hash) => {
    W.notes.set(hex(hash), { leafIndex: 9n });
    return { hash, blinding, state: s };
  });
}

const refusal = async (p: Promise<unknown>) => {
  const e = await p.then(() => undefined, (x: unknown) => x);
  expect(e).toBeInstanceOf(KeelerError);
  return e as KeelerError;
};

let W: World;
beforeEach(async () => {
  W = await world();
});

describe("pools", () => {
  it("joins the config's parameters, the keeper's coarse figures and the reference price", async () => {
    const { pools, vaults } = await W.lend.pools();
    expect(pools[0]).toMatchObject({ id: TUSD, supplyAprBps: 291, borrowAprBps: 450, utilisationBps: 7200, size: 2_400_000_000000n, minSupply: 1_000000n, feeBps: 1000, asOf: T0 - 30 });
    expect(vaults[0]).toMatchObject({ id: 1, pool: TUSD, collateralFactorBps: 8000, price: 142_370_000_000n, priceTime: T0, minCollateral: 50_000000n });
  });

  it("prices a vault in debt base units per collateral base unit, and values a debt token without a price at 1", () => {
    const sol = assets[1]!;
    const usd = assets[0]!;
    expect(vaultPrice({ [TSOL]: TSOL_PRICE }, sol, usd).price).toBe(142_370_000_000n);
    expect(vaultPrice({ [TSOL]: TSOL_PRICE, [TUSD]: { price: "200000000", expo: -8, publishTime: T0 - 5 } }, sol, usd)).toEqual({ price: 71_185_000_000n, priceTime: T0 - 5 });
    expect(() => vaultPrice({}, sol, usd)).toThrow(/no reference price/);
  });

  it("says the keeper is offline when it is", async () => {
    W.deps.http = async () => new Response("{}", { status: 502 });
    expect((await refusal(createLend(W.deps).pools())).code).toBe("keeper-offline");
  });
});

describe("intents: supply, deposit, repay", () => {
  it("asks for a signed ticket, proves, saves the record before signing, and resolves at the keeper step", async () => {
    const steps: LendStep[] = [];
    const op = await W.lend.supply(TUSD, 250_000000n, (s) => steps.push(s));
    expect(steps).toEqual(["proving", "signing", "keeper"]);
    expect(op).toMatchObject({ kind: "supply", status: "pending", step: "keeper", expiry: T0 + 300, pool: TUSD });
    const ticket = W.log.requests.find((r) => r.path === "/api/lend/ticket")!.body;
    expect(ticket).toMatchObject({ purpose: "supply", amount: "250000000", pool: TUSD, wallet: W.USER });
    expect(W.log.sent).toHaveLength(1);
    expect(W.store.get("op", op.id)?.tx?.signature).toBeTruthy();
    // One wallet prompt authorises the session for every later request.
    await W.lend.supply(TUSD, 100_000000n, () => {});
    expect(W.log.signMessage).toBe(1);
  });

  it("refuses a ticket the keeper did not sign, or one that locks funds too long, before proving", async () => {
    W.keeper.ticketSigner = await createKeyPairFromPrivateKeyBytes(new Uint8Array(32).fill(0x99));
    expect((await refusal(W.lend.supply(TUSD, 250_000000n, () => {}))).message).toMatch(/not validly signed/);
    W.keeper.ticketSigner = W.keeperKeys;
    W.keeper.ticket = (b) => ({ id: "ab".repeat(16), expiry: W.clock.now + 7200, purpose: b.purpose, amount: b.amount });
    expect((await refusal(W.lend.supply(TUSD, 250_000000n, () => {}))).message).toMatch(/expiry/);
    expect(W.log.signed).toHaveLength(0);
  });

  it("checks minimums and notes before asking anyone", async () => {
    expect((await refusal(W.lend.supply(TUSD, 999_999n, () => {}))).code).toBe("below-minimum");
    expect((await refusal(W.lend.supply(TUSD, 600_000000n, () => {}))).code).toBe("insufficient-note");
    expect((await refusal(W.lend.deposit(1, null, 10_000000n, () => {}))).code).toBe("below-minimum");
    expect(W.log.requests).toHaveLength(0);
  });

  it("shows the keeper's own pause, which still lets repayments through", async () => {
    await W.lend.supply(TUSD, 1_000000n, () => {}); // opens the session, so the account view brings the position in
    await givePosition(W, 42n, 4_000000000n, 1_000000n);
    await W.lend.lendAccount();
    W.keeper.paused = true;
    W.keeper.positions.add("42");
    const e = await refusal(W.lend.supply(TUSD, 250_000000n, () => {}));
    expect(e.code).toBe("paused");
    expect(e.message).toMatch(/paused for new deposits and borrowing/);
    const borrow = await refusal(W.lend.draw("42", 1_000000n, 0n, () => {}));
    expect(borrow.code).toBe("paused");
    // A repayment is not the Worker's or the page's to refuse: its ticket comes back signed and the intent is sent.
    const op = await W.lend.repay("42", 1_000000n, () => {});
    expect(op).toMatchObject({ kind: "repay", step: "keeper", positionId: "42" });
  });

  it("names the new position in the ticket and the intent", async () => {
    const op = await W.lend.deposit(1, null, 1_000000000n, () => {});
    const t = W.log.requests.find((r) => r.path === "/api/lend/ticket")!.body;
    expect(t).toMatchObject({ purpose: "deposit", vault: 1, position: op.positionId });
    expect(W.store.get("op", op.id)?.intent?.target.toString()).toBe(op.positionId);
  });

  it("marks a declined signature failed, and sends nothing", async () => {
    W.deps.sign = async () => {
      throw new KeelerError("wallet-rejected", "You declined.");
    };
    const lend = createLend(W.deps);
    expect((await refusal(lend.supply(TUSD, 250_000000n, () => {}))).code).toBe("wallet-rejected");
    const [r] = W.store.all("op");
    expect(r?.status).toBe("failed");
    expect(W.log.sent).toHaveLength(0);
  });

  it("tracks an intent to refundable after expiry, refunds it, and reads the refund from the chain", async () => {
    const op = await W.lend.supply(TUSD, 250_000000n, () => {});
    expect((await W.lend.lendAccount()).operations[0]).toMatchObject({ status: "pending", step: "keeper" });
    expect((await refusal(W.lend.refundIntent(op.id))).code).toBe("quote-expired");
    W.clock.now = T0 + 301;
    W.notes.set(op.id, { leafIndex: 3n });
    expect((await W.lend.lendAccount()).operations[0]?.status).toBe("refundable");
    // The cancel this wallet signs spends the intent.
    W.deps.send = async (tx) => {
      const sig = getSignatureFromTransaction(tx);
      W.sigs.set(sig, { err: null, confirmationStatus: "confirmed" });
      W.txs.set(sig, new Uint8Array(getTransactionEncoder().encode(tx)));
      W.notes.set(op.id, { leafIndex: 3n, spentBy: sig });
      return 1n;
    };
    await createLend(W.deps).refundIntent(op.id);
    expect(W.log.cancels).toBe(1);
    expect(W.store.get("op", op.id)?.status).toBe("refunded");
  });

  it("reads a fill as landed and then adopts the keeper's receipt, verified on chain", async () => {
    const op = await W.lend.supply(TUSD, 250_000000n, () => {});
    const fill = "5".repeat(88);
    W.notes.set(op.id, { leafIndex: 3n, spentBy: fill });
    W.txs.set(fill, wire(W.KEEPER, [{ programAddress: PROGRAM, accounts: [ws(W.KEEPER)], data: Uint8Array.of(TAG.supply) }]));
    await giveReceipt(W, 227_272727n);
    const a = await W.lend.lendAccount();
    expect(a.operations[0]?.status).toBe("landed");
    expect(a.receipts).toEqual([{ pool: TUSD, shares: 227_272727n, value: 227_272727n }]);
  });
});

describe("the keeper account view", () => {
  it("is a hint: an opening not on chain, spent, or not owned by this wallet is ignored", async () => {
    await W.lend.supply(TUSD, 250_000000n, () => {}); // opens the session
    await giveReceipt(W, 10n, { onChain: false });
    const spent = await giveReceipt(W, 20n);
    W.notes.set(hex(spent.hash), { leafIndex: 1n, spentBy: "x" });
    await giveReceipt(W, 30n, { owner: 0xee });
    expect((await W.lend.lendAccount()).receipts).toEqual([]);
  });

  it("brings back intents and positions after the browser lost its records", async () => {
    W.found.push({ hash: sha("lost"), output: { asset: { asset: TSOL, assetId: 2n }, amount: 1_000000000n, blinding: sha("lb") }, mint: TSOL, amount: 1_000000000n, purpose: 2, target: 42n, expiry: T0 + 100, signature: "s" });
    await givePosition(W, 42n, 1_000000000n, 0n);
    const a = await W.lend.lendAccount();
    expect(a.operations[0]).toMatchObject({ kind: "deposit", vault: 1, positionId: "42", status: "pending" });
    expect(a.positions).toEqual([{ vault: 1, id: "42", collateral: 1_000000000n, debt: 0n, lastLiquidation: null }]);
    expect(W.log.signMessage).toBe(1); // the one prompt that recovers the openings
  });
});

describe("keeper-built operations: redeem and draw", () => {
  async function withReceipt() {
    await W.lend.supply(TUSD, 1_000000n, () => {}); // opens the session
    const r = await giveReceipt(W, 227_272727n);
    await W.lend.lendAccount();
    return r;
  }
  const redeemNotes = (r: { state: Uint8Array }, amount: bigint, left: bigint, owner = USER_ADDR) => [
    { slot: SLOT.redeem.payout, owner, mint: mintBytes(TUSD), amount, kind: 0, state: new Uint8Array() },
    { slot: SLOT.redeem.receipt, owner: PDA_ADDR, mint: mintBytes(TUSD), amount: 0n, kind: 2, state: Uint8Array.of(...r.state.slice(0, -8), ...u64Bytes(left)) },
  ];

  it("signs and sends an honest redeem, and keeps the new receipt", async () => {
    const r = await withReceipt();
    // 100 tUSD at 1.1 per share burns ceil(90.909...) shares.
    W.keeper.build = (b) => keeperBuild(W, "redeem", r, redeemNotes(r, BigInt(String(b.amount)), 227_272727n - 90_909091n));
    const steps: LendStep[] = [];
    const op = await W.lend.redeem(TUSD, 100_000000n, (s) => steps.push(s));
    expect(steps).toEqual(["keeper", "signing", "landed"]);
    expect(op.status).toBe("landed");
    expect(W.log.requests.at(-1)?.body).toMatchObject({ op: "redeem", pool: TUSD, amount: "100000000" });
    const [receipt] = W.store.all("receipt");
    expect(receipt?.shares).toBe(227_272727n - 90_909091n);
    expect(W.store.get("index", TUSD)?.supply?.index).toBe(INDEX);
  });

  it("refuses a redeem paying less, paying someone else, or burning more shares, without asking the wallet", async () => {
    const r = await withReceipt();
    const cases = [
      redeemNotes(r, 99_000000n, 227_272727n - 90_909091n),
      redeemNotes(r, 100_000000n, 227_272727n - 90_909091n, new Uint8Array(64).fill(7)),
      redeemNotes(r, 100_000000n, 227_272727n - 95_000000n),
    ];
    for (const notes of cases) {
      W.keeper.build = () => keeperBuild(W, "redeem", r, notes);
      expect((await refusal(W.lend.redeem(TUSD, 100_000000n, () => {}))).message).toMatch(/does not match your request/);
    }
    W.keeper.build = () => keeperBuild(W, "redeem", r, redeemNotes(r, 100_000000n, 227_272727n - 90_909091n), [{ programAddress: address("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"), accounts: [rs(W.USER)], data: Uint8Array.of(3) }]);
    expect((await refusal(W.lend.redeem(TUSD, 100_000000n, () => {}))).message).toMatch(/another program/);
    expect(W.log.signed).toHaveLength(1); // only the supply that opened the session
  });

  it("draws against a position with the bound checked against the reference price", async () => {
    await W.lend.supply(TUSD, 1_000000n, () => {});
    const p = await givePosition(W, 42n, 4_000000000n, 0n);
    await W.lend.lendAccount();
    const shares = (100_000000n * 1_000_000_000_000n + INDEX - 1n) / INDEX;
    const notes = (bound: bigint) => [
      { slot: SLOT.draw.position, owner: PDA_ADDR, mint: mintBytes(TSOL), amount: 4_000000000n, kind: 3, state: Uint8Array.of(...p.state.slice(0, -8), ...u64Bytes(shares)) },
      { slot: SLOT.draw.debtPayout, owner: USER_ADDR, mint: mintBytes(TUSD), amount: 100_000000n, kind: 0, state: new Uint8Array() },
      { slot: SLOT.draw.collateralPayout, owner: USER_ADDR, mint: mintBytes(TSOL), amount: 0n, kind: 0, state: new Uint8Array() },
    ].map((n) => ({ ...n, bound }));
    W.keeper.build = () => keeperBuild(W, "draw", p, notes(0n), [], 120_000_000_000n);
    expect((await refusal(W.lend.draw("42", 100_000000n, 0n, () => {}))).message).toMatch(/off the market/);
    W.keeper.build = () => keeperBuild(W, "draw", p, notes(0n), [], 141_000_000_000n);
    const op = await W.lend.draw("42", 100_000000n, 0n, () => {});
    expect(op).toMatchObject({ kind: "draw", status: "landed", positionId: "42" });
    const a = await W.lend.lendAccount();
    expect(a.positions[0]).toMatchObject({ id: "42", collateral: 4_000000000n, debt: 100_000001n }); // debt rounds up
  });
});

describe("close", () => {
  it("refuses a position with debt and proves the close of one without", async () => {
    await W.lend.supply(TUSD, 1_000000n, () => {});
    await givePosition(W, 7n, 1_000000000n, 5n);
    await givePosition(W, 8n, 2_000000000n, 0n);
    await W.lend.lendAccount();
    expect((await refusal(W.lend.closePosition("7", () => {}))).code).toBe("unhealthy");
    const steps: LendStep[] = [];
    const op = await W.lend.closePosition("8", (s) => steps.push(s));
    expect(steps).toEqual(["proving", "signing", "landed"]);
    expect(op.amounts[0]?.amount).toBe(2_000000000n);
    expect(W.log.closes).toBe(1);
    expect(W.store.all("position").map((p) => p.id)).toEqual(["7"]);
  });
});

describe("keeper errors", () => {
  it("maps statuses to the page's codes", () => {
    const cases: [number, string, KeelerError["code"]][] = [
      [429, "", "rate-limited"],
      [502, "", "keeper-offline"],
      [503, "Swaps are paused.", "paused"],
      [503, "No lending keeper is configured.", "keeper-offline"],
      [403, "Lending is paused.", "paused"],
      [404, "", "storage"],
      [409, "", "busy"],
      [410, "", "no-liquidity"],
      [412, "", "price-stale"],
      [422, "", "below-minimum"],
      [423, "", "unhealthy"],
      [400, "", "network"],
    ];
    for (const [status, said, code] of cases) expect(keeperError(status, said).code, String(status)).toBe(code);
    expect(keeperError(410, "The pool has 3 tUSD idle").message).toBe("The pool has 3 tUSD idle. Nothing was sent.");
  });

  it("re-authorises once after a 401, then gives up", async () => {
    let first = true;
    const http = W.deps.http;
    W.deps.http = async (path, init) => {
      if (path === "/api/lend/ticket" && first) {
        first = false;
        return new Response(JSON.stringify({ error: "Session expired." }), { status: 401 });
      }
      return http(path, init);
    };
    await createLend(W.deps).supply(TUSD, 250_000000n, () => {});
    expect(W.log.signMessage).toBe(2);
  });

  it("reads a keeper opening as amount, blinding, state", () => {
    const o = parseOpening(opening(5n, new Uint8Array(32).fill(3), state(1n, 9n)));
    expect(o.amount).toBe(5n);
    expect(o.blinding).toEqual(new Uint8Array(32).fill(3));
    expect(u64At(o.state, o.state.length - 8)).toBe(9n);
  });
});
