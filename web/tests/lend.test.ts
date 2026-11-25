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
import { ARGS, SEED_DOMAIN, SLOT, TAG, u64At, u64Bytes, withLastU64 } from "../src/keeler/lend-layout";
import { createLend, keeperError, parseOpening, vaultPrice, type FoundIntent, type LendDeps, type LendServerConfig, type NoteState } from "../src/keeler/lend";
import { lendStore } from "../src/keeler/lend-store";

const NETWORK = "devnet";
const ORIGIN = "https://keeler.test";
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
const USER_TAG = new Uint8Array(32).fill(1);
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
  // A fake payout ciphertext: mint ‖ amount ‖ blinding (see keeperBuild).
  openPayout: async ({ data }) => (data.length === 72 ? { mint: data.slice(0, 32), amount: u64At(data, 32), blinding: data.slice(40), plain: true } : undefined),
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
  // A second keeper of the compiled set, and a key outside it.
  const keeper2Keys = await createKeyPairFromPrivateKeyBytes(new Uint8Array(32).fill(0x23));
  const strangerKeys = await createKeyPairFromPrivateKeyBytes(new Uint8Array(32).fill(0x99));
  const USER = await getAddressFromPublicKey(userKeys.publicKey);
  const KEEPER = await getAddressFromPublicKey(keeperKeys.publicKey);
  const KEEPER2 = await getAddressFromPublicKey(keeper2Keys.publicKey);
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
      return reply(200, { id: t.id, expiry: t.expiry, signature: hex(sig), keeper: await getAddressFromPublicKey(keeper.ticketSigner.publicKey) });
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
    origin: ORIGIN,
    programId: PROGRAM,
    keepers: [KEEPER, KEEPER2],
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
    owners: async () => ({ user: USER_ADDR, pda: PDA_ADDR, viewing: new Uint8Array(33).fill(6), tags: [USER_TAG] }),
    chain: {
      note: async (hash) => notes.get(hex(hash)) ?? {},
      signatureStatus: async (s) => sigs.get(s) ?? null,
      blockHeight: async () => 100n,
      transaction: async (s) => txs.get(s) ?? null,
      scanIntents: async () => found,
      testPrice: async () => null,
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
  return { deps, lend: createLend(deps), clock, notes, sigs, txs, found, log, keeper, store, USER, KEEPER, keeperKeys, keeper2Keys, strangerKeys };
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
  // Each note's ciphertext opens to it, under the owner's tag.
  const data = await Promise.all([0, 1, 2, 3].map(async (slot) => {
    const n = notes.find((x) => x.slot === slot);
    return n ? Uint8Array.of(...n.mint, ...u64Bytes(n.amount), ...(await wasm.blinding(first, seed, slot))) : new Uint8Array(72);
  }));
  const transact = encodeTransactInstructionData({
    expiryUnixTs: BigInt(W.clock.now + 60),
    txViewingPk: new Uint8Array(33).fill(2),
    salt: new Uint8Array(16),
    interfaceTransfers: [],
    outputs: outputs.map((utxoHash, i) => ({ utxoHash, ownerTag: { kind: "inline" as const, value: USER_TAG }, data: data[i] })),
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
  return { transaction: getBase64Decoder().decode(tx), outputs: outputs.map(hex), now: W.clock.now, index: INDEX.toString(), keeper: W.KEEPER, ...(bound === undefined ? {} : { bound: bound.toString() }) };
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

  it("prices a vault in debt base units per collateral base unit, and values the debt token at exactly 1, as the program does", () => {
    const sol = assets[1]!;
    const usd = assets[0]!;
    expect(vaultPrice({ [TSOL]: TSOL_PRICE }, sol, usd).price).toBe(142_370_000_000n);
    expect(vaultPrice({ [TSOL]: TSOL_PRICE, [TUSD]: { price: "98500000", expo: -8, publishTime: T0 - 5 } }, sol, usd)).toEqual({ price: 142_370_000_000n, priceTime: T0 });
    expect(() => vaultPrice({}, sol, usd)).toThrow(/no reference price/);
  });

  it("still gives the pool's parameters and the price when the keeper is offline, with empty figures", async () => {
    const http = W.deps.http;
    W.deps.http = async (url, init) => (String(url).includes("/api/lend/pools") ? new Response("{}", { status: 502 }) : http(url, init));
    const { pools, vaults } = await createLend(W.deps).pools();
    expect(pools[0]).toMatchObject({ id: TUSD, supplyAprBps: 0, utilisationBps: 0, size: 0n, minSupply: 1_000000n, feeBps: 1000 });
    expect(vaults[0]).toMatchObject({ id: 1, price: 142_370_000_000n, confidence: 0n });
  });

  it("takes the vault's price from its test price account when there is one, as the program does", async () => {
    W.deps.chain.testPrice = async () => ({ price: 10_000_000_000n, confidence: 10_000_000n, publishTime: T0 - 3 });
    expect((await createLend(W.deps).pools()).vaults[0]).toMatchObject({ price: 10_000_000_000n, confidence: 10_000_000n, priceTime: T0 - 3 });
  });

  it("shows price 0 when neither the price account nor the reference prices answer", async () => {
    W.deps.http = async () => new Response("{}", { status: 502 });
    expect((await createLend(W.deps).pools()).vaults[0]).toMatchObject({ price: 0n, confidence: 0n });
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

  it("takes a ticket from a second compiled keeper, and refuses one a stranger signed or names", async () => {
    W.keeper.ticketSigner = W.keeper2Keys;
    const op = await W.lend.supply(TUSD, 250_000000n, () => {});
    expect(W.store.get("op", op.id)?.ticket?.keeper).toBe(W.deps.keepers[1]);
    W.keeper.ticketSigner = W.strangerKeys;
    expect((await refusal(W.lend.supply(TUSD, 250_000000n, () => {}))).message).toMatch(/not validly signed by a keeper this site was built with/);
    // Signed by a compiled keeper but naming another: verified against the one it names, so refused.
    W.keeper.ticketSigner = W.keeperKeys;
    const http = W.deps.http;
    W.deps.http = async (path, init) => {
      const res = await http(path, init);
      if (path !== "/api/lend/ticket") return res;
      return new Response(JSON.stringify({ ...(await res.json()), keeper: W.deps.keepers[1] }), { status: 200 });
    };
    expect((await refusal(createLend(W.deps).supply(TUSD, 250_000000n, () => {}))).message).toMatch(/not validly signed/);
    expect(W.log.signed).toHaveLength(1);
  });

  it("sends its open tickets, as the keeper issued them, with every account read", async () => {
    const op = await W.lend.supply(TUSD, 250_000000n, () => {});
    await W.lend.lendAccount();
    const read = W.log.requests.filter((r) => r.path === "/api/lend/account").at(-1)!.body;
    const held = { id: "ab".repeat(16), expiry: T0 + 300, keeper: W.KEEPER, purpose: "supply", amount: "250000000", pool: TUSD };
    expect(read.tickets).toEqual([{ ...held, signature: W.store.get("op", op.id)!.ticket!.signature }]);
    // The session names this page's origin, and the signature covers it.
    expect(read.origin).toBe(ORIGIN);
    // Once the intent is filled, or the ticket expired, it is no longer sent.
    W.clock.now = T0 + 301;
    await W.lend.lendAccount();
    expect(W.log.requests.filter((r) => r.path === "/api/lend/account").at(-1)!.body.tickets).toEqual([]);
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

  /**
   * Position 42 held (4 tSOL, 100 tUSD of debt), then spent at `at` by a liquidation at `bound`; the keeper's view
   * then lists `next` (its latest note, which may come after later operations), or nothing, as the real keeper does
   * for a position left empty.
   */
  async function liquidated(bound: bigint, next?: [bigint, bigint], at = T0 - 20, nextWrittenByIt = false) {
    await W.lend.supply(TUSD, 1_000000n, () => {}); // opens the session
    const held = await givePosition(W, 42n, 4_000000000n, 100_000000n);
    await W.lend.lendAccount();
    W.keeper.account.positions = [];
    const after = next ? await givePosition(W, 42n, next[0], next[1]) : undefined;
    const sig = "7".repeat(88);
    const args = new Uint8Array(ARGS.liquidate.length);
    args.set(u64Bytes(BigInt(at)), ARGS.liquidate.now);
    args.set(u64Bytes(bound), ARGS.liquidate.bound);
    const transfer = nextWrittenByIt && after ? liquidationTransfer(after.hash) : new Uint8Array();
    W.txs.set(sig, wire(W.KEEPER, [{ programAddress: PROGRAM, accounts: [ws(W.KEEPER)], data: Uint8Array.of(TAG.liquidate, ...args, ...transfer) }]));
    W.notes.set(hex(held.hash), { leafIndex: 9n, spentBy: sig });
    return after;
  }
  const liquidationTransfer = (position: Uint8Array) =>
    encodeTransactInstructionData({
      expiryUnixTs: BigInt(T0),
      txViewingPk: new Uint8Array(33).fill(2),
      salt: new Uint8Array(16),
      interfaceTransfers: [],
      outputs: [sha("reserve"), position].map((utxoHash) => ({ utxoHash, ownerTag: { kind: "inline" as const, value: new Uint8Array(32) } })),
      messages: [],
      privateTxHash: new Uint8Array(32),
      circuit: { kind: "confidentialSigner", inputs: 2, outputs: 2, publicAssetSlots: 0 },
      proof: { a: new Uint8Array(32), b: new Uint8Array(128), c: new Uint8Array(32) },
      inputs: [{ nullifierHash: new Uint8Array(32).fill(1), treeIndex: 0 }, { nullifierHash: RESERVE_NF, treeIndex: 0 }],
      treeContexts: [{ utxoTreeRootIndex: 0, nullifierTreeRootIndex: 0 }],
    } as never);
  // At $28.50 a tSOL, as lend-math's `liquidate` (the program's) computes it.
  const PARTIAL = { at: T0 - 20, price: 28_500_000_000n, collateralSold: 776_393192n, debtRepaid: 21_908125n, collateralLeft: 3_223_606808n, debtLeft: 78_091875n };

  it("records what a liquidation left, and keeps that after a later repayment", async () => {
    const after = (await liquidated(PARTIAL.price, [PARTIAL.collateralLeft, PARTIAL.debtLeft]))!;
    expect((await W.lend.lendAccount()).positions).toEqual([{ vault: 1, id: "42", collateral: PARTIAL.collateralLeft, debt: PARTIAL.debtLeft, lastLiquidation: PARTIAL }]);
    // The owner then repays everything: the position shows no debt, the liquidation still says what it left.
    W.notes.set(hex(after.hash), { leafIndex: 9n, spentBy: "8".repeat(88) });
    W.txs.set("8".repeat(88), wire(W.KEEPER, [{ programAddress: PROGRAM, accounts: [ws(W.KEEPER)], data: Uint8Array.of(TAG.repay) }]));
    W.keeper.account.positions = [];
    await givePosition(W, 42n, PARTIAL.collateralLeft, 0n);
    expect((await W.lend.lendAccount()).positions).toEqual([{ vault: 1, id: "42", collateral: PARTIAL.collateralLeft, debt: 0n, lastLiquidation: PARTIAL }]);
  });

  it("shows a liquidation smaller than the largest with its own figures, read from the position it wrote", async () => {
    await liquidated(PARTIAL.price, [3_500000000n, 90_000000n], T0 - 20, true);
    const [p] = (await W.lend.lendAccount()).positions;
    expect(p!.lastLiquidation).toEqual({ at: T0 - 20, price: PARTIAL.price, collateralSold: 500_000000n, debtRepaid: 10_000000n, collateralLeft: 3_500000000n, debtLeft: 90_000000n });
  });

  it("says what the liquidation did even when a top-up landed before the page read", async () => {
    await liquidated(PARTIAL.price, [PARTIAL.collateralLeft + 1_000000000n, PARTIAL.debtLeft]);
    expect((await W.lend.lendAccount()).positions).toEqual([{ vault: 1, id: "42", collateral: PARTIAL.collateralLeft + 1_000000000n, debt: PARTIAL.debtLeft, lastLiquidation: PARTIAL }]);
  });

  it("does not credit the liquidation with a repayment that landed before the page read", async () => {
    await liquidated(PARTIAL.price, [PARTIAL.collateralLeft, 0n]);
    const [p] = (await W.lend.lendAccount()).positions;
    expect(p).toEqual({ vault: 1, id: "42", collateral: PARTIAL.collateralLeft, debt: 0n, lastLiquidation: PARTIAL });
    expect(p!.lastLiquidation!.debtLeft).toBe(78_091875n);
  });

  it("lists a position a full liquidation left with nothing as emptied, though the keeper lists no such position", async () => {
    // At $25.25 a tSOL the 4 tSOL pay exactly 100 tUSD and its 1%.
    await liquidated(25_250_000_000n);
    expect(W.keeper.account.positions).toEqual([]);
    const a = await W.lend.lendAccount();
    expect(a.positions).toEqual([]);
    expect(a.emptied).toEqual([{ vault: 1, id: "42", collateral: 0n, debt: 0n, lastLiquidation: { at: T0 - 20, price: 25_250_000_000n, collateralSold: 4_000000000n, debtRepaid: 100_000000n, collateralLeft: 0n, debtLeft: 0n } }]);
    expect((await W.lend.lendAccount()).emptied).toHaveLength(1);
  });

  it("does not call a partial liquidation followed by a close emptied", async () => {
    await liquidated(PARTIAL.price);
    const a = await W.lend.lendAccount();
    expect(a.positions).toEqual([]);
    expect(a.emptied).toEqual([]);
  });

  it("after a reload, reads the keeper for an intent on chain whose ticket waits, so another keeper can fill it", async () => {
    const op = await W.lend.supply(TUSD, 250_000000n, () => {});
    await W.lend.lendAccount(); // the intent lands: on chain, waiting for the keeper
    expect(W.store.get("op", op.id)).toMatchObject({ onChain: true, status: "pending" });
    const reads = () => W.log.requests.filter((r) => r.path === "/api/lend/account");
    const before = reads().length;
    // A reload: a new client, no session, nothing spent, nothing lost.
    await createLend(W.deps).lendAccount();
    expect(reads()).toHaveLength(before + 1);
    expect((reads().at(-1)!.body.tickets as { id: string }[]).map((t) => t.id)).toEqual([W.store.get("op", op.id)!.ticket!.id]);
    // Without one waiting there is nothing to ask a keeper about.
    W.clock.now = T0 + 301;
    await createLend(W.deps).lendAccount();
    expect(reads()).toHaveLength(before + 1);
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

  it("sends nothing and saves nothing when the block height cannot be read", async () => {
    const r = await withReceipt();
    W.keeper.build = (b) => keeperBuild(W, "redeem", r, redeemNotes(r, BigInt(String(b.amount)), 227_272727n - 90_909091n));
    W.deps.chain.blockHeight = async () => {
      throw new Error("rpc down");
    };
    expect((await refusal(W.lend.redeem(TUSD, 100_000000n, () => {}))).code).toBe("network");
    expect(W.log.sent).toHaveLength(1); // the supply that opened the session
    expect(W.store.all("op").filter((o) => o.kind === "redeem")).toEqual([]);
  });

  it("withdraws everything by shares: pays their worth at the keeper's index and leaves a zero-share receipt", async () => {
    const r = await withReceipt();
    // 227.272727 shares at 1.1 are worth floor(249.9999997) = 249.999999 tUSD.
    W.keeper.build = () => keeperBuild(W, "redeem", r, redeemNotes(r, 249_999999n, 0n));
    const op = await W.lend.redeem(TUSD, "all", () => {});
    expect(op).toMatchObject({ status: "landed", amounts: [{ amount: 249_999999n }] });
    expect(W.log.requests.at(-1)?.body).toMatchObject({ op: "redeem", pool: TUSD, shares: "227272727" });
    expect(W.log.requests.at(-1)?.body).not.toHaveProperty("amount");
    expect(W.log.requests.at(-1)?.body).toMatchObject({ receipt: hex(r.hash) });
    // A receipt of no shares is not kept.
    expect(W.store.all("receipt")).toEqual([]);
  });

  it("refuses a redeem of everything that pays less than the shares' worth or leaves shares behind", async () => {
    const r = await withReceipt();
    for (const notes of [redeemNotes(r, 249_999998n, 0n), redeemNotes(r, 249_999999n, 1n)]) {
      W.keeper.build = () => keeperBuild(W, "redeem", r, notes);
      expect((await refusal(W.lend.redeem(TUSD, "all", () => {}))).message).toMatch(/does not match your request/);
    }
    expect(W.log.signed).toHaveLength(1); // only the supply that opened the session
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
    // The keeper's index of a refused transaction is not kept.
    expect(W.store.get("index", TUSD)).toBeUndefined();
  });

  it("names the receipt it spends: the smallest one worth the amount", async () => {
    const big = await withReceipt();
    const small = await giveReceipt(W, 100_000000n);
    await W.lend.lendAccount();
    W.keeper.build = () => keeperBuild(W, "redeem", small, redeemNotes(small, 100_000000n, 100_000000n - 90_909091n));
    await W.lend.redeem(TUSD, 100_000000n, () => {});
    expect(W.log.requests.at(-1)?.body).toMatchObject({ op: "redeem", receipt: hex(small.hash), amount: "100000000" });
    expect(W.store.all("receipt").map((r) => r.shares).sort()).toEqual([100_000000n - 90_909091n, 227_272727n].sort());
    expect(W.store.get("receipt", hex(big.hash))).toBeTruthy();
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

  it("values the debt token at 1 when /api/prices says it is off its peg: a draw at the program's price is accepted", async () => {
    const http = W.deps.http;
    W.deps.http = async (path, init) => (path === "/api/prices" ? new Response(JSON.stringify({ [TSOL]: TSOL_PRICE, [TUSD]: { price: "98500000", expo: -8, publishTime: T0 } }), { status: 200 }) : http(path, init));
    W.lend = createLend(W.deps);
    // The program's price, from which health is computed: tSOL in tUSD at 1, not 142.37 / 0.985.
    expect((await W.lend.pools()).vaults[0]).toMatchObject({ price: 142_370_000_000n });
    await W.lend.supply(TUSD, 1_000000n, () => {});
    const p = await givePosition(W, 42n, 4_000000000n, 0n);
    await W.lend.lendAccount();
    const shares = (100_000000n * 1_000_000_000_000n + INDEX - 1n) / INDEX;
    const notes = [
      { slot: SLOT.draw.position, owner: PDA_ADDR, mint: mintBytes(TSOL), amount: 4_000000000n, kind: 3, state: Uint8Array.of(...p.state.slice(0, -8), ...u64Bytes(shares)) },
      { slot: SLOT.draw.debtPayout, owner: USER_ADDR, mint: mintBytes(TUSD), amount: 100_000000n, kind: 0, state: new Uint8Array() },
      { slot: SLOT.draw.collateralPayout, owner: USER_ADDR, mint: mintBytes(TSOL), amount: 0n, kind: 0, state: new Uint8Array() },
    ];
    // 141: within 2% of 142.37, more than 2% below the 144.54 a priced debt token would give.
    W.keeper.build = () => keeperBuild(W, "draw", p, notes, [], 141_000_000_000n);
    expect(await W.lend.draw("42", 100_000000n, 0n, () => {})).toMatchObject({ kind: "draw", status: "landed" });
  });
});

describe("several receipts, empty notes, indices", () => {
  it("keeps every receipt in a pool: a second supply's receipt adds to the first", async () => {
    await W.lend.supply(TUSD, 1_000000n, () => {});
    await giveReceipt(W, 227_272727n);
    await giveReceipt(W, 50_000000n);
    expect((await W.lend.lendAccount()).receipts.map((r) => r.shares)).toEqual(expect.arrayContaining([227_272727n, 50_000000n]));
    expect((await W.lend.lendAccount()).receipts).toHaveLength(2);
  });

  it("takes a new receipt after a withdrawal of everything", async () => {
    await W.lend.supply(TUSD, 1_000000n, () => {});
    const r = await giveReceipt(W, 227_272727n);
    await W.lend.lendAccount();
    W.keeper.build = () =>
      keeperBuild(W, "redeem", r, [
        { slot: SLOT.redeem.payout, owner: USER_ADDR, mint: mintBytes(TUSD), amount: 249_999999n, kind: 0, state: new Uint8Array() },
        { slot: SLOT.redeem.receipt, owner: PDA_ADDR, mint: mintBytes(TUSD), amount: 0n, kind: 2, state: Uint8Array.of(...r.state.slice(0, -8), ...u64Bytes(0n)) },
      ]);
    await W.lend.redeem(TUSD, "all", () => {});
    expect(W.store.all("receipt")).toEqual([]);
    W.notes.set(hex(r.hash), { leafIndex: 7n, spentBy: "redeemed" });
    W.keeper.account.receipts = [];
    await giveReceipt(W, 80_000000n);
    expect((await W.lend.lendAccount()).receipts.map((x) => x.shares)).toEqual([80_000000n]);
  });

  it("drops a zero-share receipt or a position with no collateral and no debt the keeper offers", async () => {
    await W.lend.supply(TUSD, 1_000000n, () => {});
    await giveReceipt(W, 0n);
    await givePosition(W, 5n, 0n, 0n);
    const a = await W.lend.lendAccount();
    expect(a.receipts).toEqual([]);
    expect(a.positions).toEqual([]);
  });

  it("takes the keeper's indices only between the last seen and that grown at the curve's top rate", async () => {
    await W.lend.supply(TUSD, 1_000000n, () => {});
    const view = W.keeper.account as typeof W.keeper.account & { index?: { supply: string; borrow: string; at: number } };
    view.index = { supply: "1100000000000", borrow: "1200000000000", at: T0 };
    await W.lend.lendAccount();
    expect(W.store.get("index", TUSD)?.supply?.index).toBe(1_100_000_000_000n);
    // A year at 15% at most: 1.1 grows to 1.265 at most, never shrinks.
    W.clock.now = T0 + 31_536_000;
    for (const supply of ["1300000000000", "1000000000000"]) {
      view.index = { supply, borrow: "1200000000000", at: W.clock.now };
      await W.lend.lendAccount();
      expect(W.store.get("index", TUSD)?.supply?.index).toBe(1_100_000_000_000n);
    }
    view.index = { supply: "1260000000000", borrow: "1200000000000", at: W.clock.now };
    await W.lend.lendAccount();
    expect(W.store.get("index", TUSD)?.supply?.index).toBe(1_260_000_000_000n);
  });
});

describe("a keeper fill with the keeper gone", () => {
  /** The successor note a keeper deposit-add or repay makes of `p`, put on chain. */
  async function successorOnChain(p: { hash: Uint8Array; blinding: Uint8Array; state: Uint8Array }, collateral: bigint, debtShares: bigint) {
    const blinding = await wasm.blinding(wasm.nullifier(p.hash, p.blinding), wasm.seed(SEED_DOMAIN.draw, p.blinding), SLOT.draw.position);
    const hash = await wasm.hash({ slot: SLOT.draw.position, owner: PDA_ADDR, mint: mintBytes(TSOL), amount: collateral, kind: 3, state: withLastU64(p.state, debtShares), blinding });
    W.notes.set(hex(hash), { leafIndex: 11n });
  }

  it("derives the position after a deposit-add and keeps it once the chain has it", async () => {
    await W.lend.supply(TUSD, 1_000000n, () => {});
    const p = await givePosition(W, 42n, 4_000000000n, 1_000000n);
    await W.lend.lendAccount();
    await W.lend.deposit(1, "42", 1_000000000n, () => {});
    W.notes.set(hex(p.hash), { leafIndex: 9n, spentBy: "fill" });
    await successorOnChain(p, 5_000000000n, 1_000000n);
    W.keeper.status = 502;
    const a = await W.lend.lendAccount();
    expect(a.positions).toMatchObject([{ id: "42", collateral: 5_000000000n }]);
  });

  it("derives the debt-free position after a full repay, so it can close", async () => {
    await W.lend.supply(TUSD, 1_000000n, () => {});
    const p = await givePosition(W, 42n, 4_000000000n, 1_000000n);
    await W.lend.lendAccount();
    await W.lend.repay("42", 2_000000n, () => {});
    W.notes.set(hex(p.hash), { leafIndex: 9n, spentBy: "fill" });
    await successorOnChain(p, 4_000000000n, 0n);
    W.keeper.status = 502;
    const a = await W.lend.lendAccount();
    expect(a.positions).toMatchObject([{ id: "42", collateral: 4_000000000n, debt: 0n }]);
  });

  it("keeps a spent position it cannot replace until the keeper's view says what replaced it", async () => {
    await W.lend.supply(TUSD, 1_000000n, () => {});
    const p = await givePosition(W, 42n, 4_000000000n, 1_000000n);
    await W.lend.lendAccount();
    W.notes.set(hex(p.hash), { leafIndex: 9n, spentBy: "fill" });
    W.keeper.status = 502;
    expect((await W.lend.lendAccount()).positions).toMatchObject([{ id: "42", collateral: 4_000000000n }]);
    // The keeper is back and no longer lists it: it is gone (an empty successor).
    W.keeper.status = 200;
    W.keeper.account.positions = [];
    expect((await W.lend.lendAccount()).positions).toEqual([]);
  });

  it("drops a spent receipt only once the keeper's view was read", async () => {
    await W.lend.supply(TUSD, 1_000000n, () => {});
    const r = await giveReceipt(W, 5_000000n);
    await W.lend.lendAccount();
    W.notes.set(hex(r.hash), { leafIndex: 7n, spentBy: "elsewhere" });
    W.keeper.status = 502;
    expect((await W.lend.lendAccount()).receipts).toHaveLength(1);
    W.keeper.status = 200;
    W.keeper.account.receipts = [];
    expect((await W.lend.lendAccount()).receipts).toEqual([]);
  });
});

describe("sessions, prompts, and one opening per vault", () => {
  it("never asks the wallet on a read with prompt false, and says the keeper's view is due", async () => {
    const p = await givePosition(W, 42n, 4_000000000n, 1_000000n);
    W.store.put("position", hex(p.hash), { vault: 1, id: "42", collateral: 4_000000000n, debtShares: 1_000000n, note: { hash: p.hash, blinding: p.blinding, amount: 4_000000000n, state: p.state } as never, lastLiquidation: null });
    W.notes.set(hex(p.hash), { leafIndex: 9n, spentBy: "fill" });
    const a = await W.lend.lendAccount({ prompt: false });
    expect(W.log.signMessage).toBe(0);
    expect(a.needsSession).toBe(true);
    expect((await W.lend.lendAccount()).needsSession).toBeUndefined();
    expect(W.log.signMessage).toBe(1);
  });

  it("starts a new session after a wallet switch", async () => {
    await W.lend.supply(TUSD, 1_000000n, () => {});
    W.deps.wallet = () => "Other1111111111111111111111111111111111111" as never;
    await W.lend.lendAccount({ prompt: false });
    W.deps.wallet = () => W.USER;
    await W.lend.supply(TUSD, 1_000000n, () => {});
    expect(W.log.signMessage).toBe(2);
  });

  it("tries a failed rescan again on the next read", async () => {
    let scans = 0;
    W.deps.chain.scanIntents = async () => {
      if (++scans === 1) throw new Error("rpc down");
      return [];
    };
    await W.lend.lendAccount();
    await W.lend.lendAccount();
    await W.lend.lendAccount();
    expect(scans).toBe(2);
  });

  it("refuses a second opening deposit to a vault while the first waits", async () => {
    await W.lend.deposit(1, null, 1_000000000n, () => {});
    expect((await refusal(createLend(W.deps).deposit(1, null, 1_000000000n, () => {}))).code).toBe("busy");
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
