// Keeler Lend behind the `KeelerLend` interface (docs/plan-lend.md, docs/lend-api.md). Every chain, keeper, wallet
// and proving call comes in through `LendDeps`, so the flows here run in unit tests against fakes; client.ts wires the
// real ones (lend-chain.ts). Every "now" is chain time.
import { address, getAddressEncoder, getBase64Encoder, getSignatureFromTransaction, type Address, type Transaction } from "@solana/kit";
import { KeelerError, type Asset, type KeelerLend, type LendAccount, type LendKind, type LendOperation, type LendStep, type Pool, type Position, type Receipt, type Vault } from "./index";
import { checkKeeperTransaction, lendInstructions, type BuildReply, type ExpectedNote, type Hashing } from "./lend-check";
import { authorisationMessage, fromHex, hex, sessionExpiryFor, sessionKey, signRequest, verifyTicket, type Fields, type Session } from "./lend-auth";
import { ARGS, CHECK, NOTE_KIND, PURPOSE, SEED_DOMAIN, SLOT, TAG, u64At, withLastU64, type Purpose } from "./lend-layout";
import { PRICE_SCALE, assetsOf, borrowShares, debtOf, sharesToBurn } from "./lend-math";
import type { LendStore, NoteOpening, OpRecord, PositionRecord, ReceiptRecord } from "./lend-store";
import type { RawOutput } from "./swap-wasm";

/** `/api/config` `lend`, as the Worker serves it. */
export type LendServerConfig = {
  programId: string;
  pools: { mint: string; curve: { utilisationBps: number; rateBps: number }[]; feeBps: number; maxUtilisationBps: number; minSupply: string }[];
  vaults: { id: number; collateralMint: string; debtMint: string; cfBps: number; ltBps: number; lmlBps: number; penaltyBps: number; minCollateral: string; minDebt: string }[];
};

export type Prices = Record<string, { price: string; expo: number; publishTime: number }>;

/** A note's place on chain: its leaf once appended, and the transaction that spent it. A failed read throws. */
export type NoteState = { leafIndex?: bigint; spentBy?: string };

export type FoundIntent = { hash: Uint8Array; output: RawOutput; mint: string; amount: bigint; purpose: number; target: bigint; expiry: number; signature: string };

export type LendChain = {
  note(hash: Uint8Array, blinding: Uint8Array): Promise<NoteState>;
  signatureStatus(signature: string): Promise<{ err: unknown; confirmationStatus?: string | null } | null>;
  blockHeight(): Promise<bigint>;
  /** The wire bytes of a landed transaction, or null if the RPC has none yet. */
  transaction(signature: string): Promise<Uint8Array | null>;
  /** Intents this wallet wrote, from its user PDA's transactions, decrypted with its keys. */
  scanIntents(): Promise<FoundIntent[]>;
};

export type Built = { transaction: Transaction; lastValidBlockHeight: bigint };

export type LendBuilder = {
  /** Proves an intent of `amount` from `source` (one private note); `onProving` fires as proving starts. */
  intent(p: { source: unknown; mint: string; amount: bigint; purpose: number; target: bigint; expiry: number }, onProving: () => void): Promise<Built & { hash: Uint8Array; output: RawOutput }>;
  cancel(intent: { hash: Uint8Array; output: RawOutput; leafIndex: bigint }, onProving: () => void): Promise<Built>;
  close(position: NoteOpening & { leafIndex: bigint }, onProving: () => void): Promise<Built>;
};

export type LendWasm = Hashing & {
  seed(domain: string, previousBlinding: Uint8Array): Uint8Array;
  /** The nullifier of a note the user PDA owns. */
  nullifier(hash: Uint8Array, blinding: Uint8Array): Uint8Array;
  /** Decodes and checks a state against its opening (throws unless it reproduces `note.hash`); returns the fields the page reads. */
  decode(note: NoteOpening): Promise<{ ownerIsMe: boolean; pool?: bigint; vault?: bigint; positionId?: bigint; shares: bigint }>;
};

export type Owners = { user: Uint8Array; pda: Uint8Array; viewing: Uint8Array };

export type LendDeps = {
  network: string;
  programId: Address;
  keeper: Address;
  config: LendServerConfig;
  assets: Asset[];
  wallet(): Address;
  http(path: string, init?: RequestInit): Promise<Response>;
  chainNow(): Promise<number>;
  /** The session seed derived at unlock; throws not-registered while locked. */
  sessionSeed(): Uint8Array;
  signMessage(message: Uint8Array): Promise<Uint8Array>;
  sign(tx: Transaction): Promise<Transaction>;
  send(tx: Transaction): Promise<bigint>;
  sync(): Promise<void>;
  /** Unspent private notes of `mint`, as `{ amount, utxo }`. */
  notes(mint: string): { amount: bigint; utxo: unknown }[];
  /** The user's shielded address, the user PDA's under the user's viewing key (receipts, positions), and that key. */
  owners(): Promise<Owners>;
  chain: LendChain;
  build: LendBuilder;
  wasm: LendWasm;
  store(): LendStore;
};

const err = (code: KeelerError["code"], message: string) => new KeelerError(code, message);
const mintBytes = (mint: string) => new Uint8Array(getAddressEncoder().encode(address(mint)));
const toBytes = (b64: string) => new Uint8Array(getBase64Encoder().encode(b64));
const equal = (a: Uint8Array, b: Uint8Array) => a.length === b.length && a.every((x, i) => x === b[i]);
const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** The keeper's statuses → page error codes (docs/lend-api.md, "Errors"). */
export function keeperError(status: number, said: string): KeelerError {
  const s = said ? `${said.replace(/\.?$/, ".")} ` : "";
  if (status === 429) return err("rate-limited", `${s || "Too many requests. "}Wait a minute and try again.`);
  if (status === 503 && /paused/i.test(said)) return err("paused", "Lending is paused. Your deposits and positions are safe; refunds of unfilled requests still work.");
  if (status === 502 || status === 503 || status === 504) return err("keeper-offline", "The keeper is not answering right now, so nothing was sent. Your funds have not moved; try again in a few minutes.");
  const code = ({ 403: "paused", 404: "storage", 409: "busy", 410: "no-liquidity", 412: "price-stale", 422: "below-minimum", 423: "unhealthy" } as const)[status as 403];
  if (code) return err(code, `${s}Nothing was sent.`.trim());
  if (status === 401) return err("network", `The keeper did not accept this browser's lending session. ${s}Reload the page and try again.`.replace(/\s+/g, " "));
  return err("network", `The keeper refused the request (HTTP ${status}). ${s}Nothing was sent.`.replace(/\s+/g, " "));
}

/** The vault's reference price, 1e12-scaled debt base units per collateral base unit. A debt token without a price is valued at 1 (tUSD). */
export function vaultPrice(p: Prices, collateral: Asset, debt: Asset): { price: bigint; priceTime: number } {
  const read = (a: Asset) => {
    const x = p[a.mint];
    if (!x) return undefined;
    if (typeof x.price !== "string" || !/^\d+$/.test(x.price) || !Number.isInteger(x.expo) || !Number.isSafeInteger(x.publishTime)) throw err("price-stale", `The reference price of ${a.symbol} is unreadable. Try again in a minute.`);
    return { price: BigInt(x.price), expo: x.expo, publishTime: x.publishTime };
  };
  const c = read(collateral);
  if (!c || c.price === 0n) throw err("price-stale", `There is no reference price for ${collateral.symbol} right now. Try again in a minute.`);
  const d = read(debt) ?? { price: 1n, expo: 0, publishTime: c.publishTime };
  if (d.price === 0n) throw err("price-stale", `There is no reference price for ${debt.symbol} right now. Try again in a minute.`);
  const e = c.expo - d.expo + debt.decimals - collateral.decimals + 12;
  const price = e >= 0 ? (c.price * 10n ** BigInt(e)) / d.price : c.price / (d.price * 10n ** BigInt(-e));
  return { price, priceTime: Math.min(c.publishTime, d.publishTime) };
}

/** A keeper account-view `message`: `amount u64 LE ‖ blinding [32] ‖ state` (the Borsh `NoteOpening`, docs/lend-api.md). */
export function parseOpening(b64: string): { amount: bigint; blinding: Uint8Array; state: Uint8Array } {
  const b = toBytes(b64);
  if (b.length < 8 + 32 + 8) throw new Error("opening too short");
  return { amount: u64At(b, 0), blinding: b.slice(8, 40), state: b.slice(40) };
}

type KeeperPools = { pools: { mint: string; at: number; size: string; utilisationBps: number; supplyRateBps: number; borrowRateBps: number }[] };
type KeeperAccount = {
  receipts: { pool: string; shares: string; message: string }[];
  positions: { vault: number; position: string; collateral: string; debtShares: string; message: string }[];
};
type WireBuild = { transaction: string; outputs: string[]; now: number; bound?: string; index?: string };

const NEVER_SENT = "This request was never sent (the wallet did not sign it); nothing moved.";
const NEVER_LANDED = "The transaction expired before it reached the chain; nothing moved.";
const FAILED = "The transaction failed on chain; nothing moved.";

export function createLend(d: LendDeps): KeelerLend {
  const cfg = d.config;
  const asset = (mint: string): Asset => d.assets.find((a) => a.mint === mint) ?? { mint, symbol: `${mint.slice(0, 4)}…`, decimals: 0 };
  const poolOf = (mint: string) => {
    const i = cfg.pools.findIndex((p) => p.mint === mint);
    if (i < 0) throw err("storage", "This service has no such pool.");
    // Pool ids (`ReceiptState::pool`, a supply intent's target) are the position in LEND_POOLS, from 1.
    return { ...cfg.pools[i]!, id: BigInt(i + 1) };
  };
  const vaultOf = (id: number) => {
    const v = cfg.vaults.find((x) => x.id === id);
    if (!v) throw err("storage", "This service has no such vault.");
    return v;
  };

  let session: Session | undefined;
  let lastFigures: KeeperPools["pools"] = [];
  let scanned = false;
  const running = new Set<string>();

  const exclusive = async <T>(key: string, run: () => Promise<T>): Promise<T> => {
    if (running.has(key)) throw err("busy", key === "lend" ? "Another lending operation is in progress. Wait for it to finish." : "This refund is already in progress. Wait for it to finish.");
    running.add(key);
    try {
      return await run();
    } finally {
      running.delete(key);
    }
  };

  /** The session, asking the wallet once to authorise it (or again within 5 minutes of its expiry). */
  const authorise = async (now: number): Promise<Session> => {
    if (session && session.expiry - now > 300) return session;
    const keys = await sessionKey(d.sessionSeed());
    const expiry = sessionExpiryFor(now);
    const auth = await d.signMessage(authorisationMessage({ network: d.network, programId: d.programId, wallet: d.wallet(), sessionKey: keys.key, expiry }));
    session = { ...keys, expiry, auth: hex(auth) };
    return session;
  };

  const call = async <T>(path: string, init?: RequestInit): Promise<T> => {
    let res: Response;
    try {
      res = await d.http(path, init);
    } catch {
      throw err("network", `Could not reach ${path}. Check your connection and try again.`);
    }
    if (res.ok) return (await res.json()) as T;
    const said = await res
      .json()
      .then((b: { error?: unknown }) => (typeof b?.error === "string" ? b.error.slice(0, 200) : ""))
      .catch(() => "");
    throw Object.assign(keeperError(res.status, said), { status: res.status });
  };

  /** A signed keeper request; one 401 drops the session and asks again (an expired or replaced session). */
  const keeper = async <T>(method: "GET" | "POST", path: string, body: Fields): Promise<T> => {
    for (let attempt = 0; ; attempt++) {
      const now = await d.chainNow();
      const fields = await signRequest(await authorise(now), d.wallet(), method, path, body, now);
      try {
        if (method === "GET") return await call<T>(`/api${path}?${new URLSearchParams(Object.entries(fields).map(([k, v]) => [k, String(v)]))}`);
        return await call<T>(`/api${path}`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(fields) });
      } catch (e) {
        if (attempt === 0 && (e as { status?: number }).status === 401) {
          session = undefined;
          continue;
        }
        throw e;
      }
    }
  };

  const prices = async (): Promise<Prices> => {
    try {
      return await call<Prices>("/api/prices");
    } catch (e) {
      if (e instanceof KeelerError && e.code === "rate-limited") throw e;
      throw err("price-stale", "Reference prices are unavailable right now. Try again in a minute.");
    }
  };

  const toOp = (r: OpRecord): LendOperation => {
    const { tx: _t, onChain: _o, intent: _i, spends: _s, creates: _c, ...op } = r;
    return { ...op, amounts: op.amounts.map((a) => ({ ...a })) };
  };
  const save = (r: OpRecord) => {
    try {
      d.store().put("op", r.id, r);
    } catch (e) {
      throw err("storage", `Could not save the request in this browser (${message(e)}), so it was not sent. Free some site storage and try again.`);
    }
  };

  /** Applies what a landed keeper-built operation or close leaves: the new note replaces the spent one. */
  const settle = (r: OpRecord) => {
    const s = d.store();
    if (r.spends) {
      s.remove("receipt", hex(r.spends));
      s.remove("position", hex(r.spends));
    }
    if (r.creates?.receipt) s.put("receipt", hex(r.creates.receipt.note.hash), r.creates.receipt);
    if (r.creates?.position) s.put("position", hex(r.creates.position.note.hash), r.creates.position);
  };

  /** One record's next state from the chain; a failed read throws and changes nothing. */
  const refresh = async (r: OpRecord, now: number): Promise<OpRecord> => {
    if (r.status === "landed" || r.status === "refunded" || r.status === "failed") return r;
    let next = r;
    if (!r.onChain) {
      if (!r.tx) return now > (r.expiry ?? r.createdAt + 600) ? { ...r, status: "failed", error: NEVER_SENT } : r;
      const height = await d.chain.blockHeight();
      const s = await d.chain.signatureStatus(r.tx.signature);
      if (s === null) return height > r.tx.lastValidBlockHeight ? { ...r, status: "failed", error: NEVER_LANDED } : r;
      if (s.confirmationStatus !== "confirmed" && s.confirmationStatus !== "finalized") return r;
      if (s.err !== null && s.err !== undefined) return { ...r, status: "failed", error: FAILED };
      next = { ...r, onChain: true };
    }
    if (!next.intent) {
      settle(next);
      const { error: _e, ...rest } = next;
      return { ...rest, status: "landed", step: "landed" };
    }
    const note = await d.chain.note(next.intent.hash, next.intent.output.blinding);
    if (note.spentBy) {
      const wire = await d.chain.transaction(note.spentBy);
      if (!wire) return next;
      const mine = lendInstructions(wire, d.programId).some((ix) => ix.tag === TAG.cancelIntent && ix.signers.includes(d.wallet()));
      return mine ? { ...next, status: "refunded" } : { ...next, status: "landed", step: "landed" };
    }
    return { ...next, status: next.expiry !== null && now > next.expiry ? "refundable" : "pending", step: "keeper" };
  };

  const refreshOps = async (now: number) => {
    const s = d.store();
    return Promise.all(
      s.all("op").map(async (r) => {
        const next = await refresh(r, now).catch(() => r);
        if (next !== r) s.put("op", next.id, next);
        return next;
      }),
    );
  };

  /** Intents this wallet wrote, found on chain: brings back records a cleared browser lost. */
  const rescan = async () => {
    const s = d.store();
    const found = await d.chain.scanIntents();
    for (const f of found) {
      const id = hex(f.hash);
      if (s.get("op", id)) continue;
      const kind: LendKind = f.purpose === PURPOSE.supply ? "supply" : f.purpose === PURPOSE.deposit ? "deposit" : "repay";
      const vault = kind === "supply" ? undefined : cfg.vaults.find((v) => v.collateralMint === f.mint || v.debtMint === f.mint);
      s.put("op", id, {
        id,
        kind,
        pool: kind === "supply" ? f.mint : (vault?.debtMint ?? f.mint),
        vault: vault?.id ?? null,
        positionId: kind === "supply" ? null : f.target.toString(),
        amounts: [{ asset: asset(f.mint), amount: f.amount }],
        step: "keeper",
        status: "pending",
        expiry: f.expiry,
        // shortcut: the intent's own time is not on chain here; its expiry orders records the same way.
        createdAt: f.expiry,
        onChain: true,
        intent: { hash: f.hash, output: f.output, purpose: f.purpose, target: f.target },
      });
    }
    return found.length;
  };

  /** Checks a keeper-view opening against the chain (appended, unspent) and the WASM decoder before keeping it. */
  const adopt = async (kind: "receipt" | "position", mint: string, b64: string): Promise<{ note: NoteOpening; decoded: Awaited<ReturnType<LendWasm["decode"]>> } | undefined> => {
    try {
      const o = parseOpening(b64);
      const { pda } = await d.owners();
      const noteKind = kind === "receipt" ? NOTE_KIND.receipt : NOTE_KIND.position;
      const blinding = o.blinding;
      const hash = await d.wasm.hash({ slot: 0, owner: pda, mint: mintBytes(mint), amount: o.amount, kind: noteKind, state: o.state, blinding });
      const where = await d.chain.note(hash, blinding);
      if (where.leafIndex === undefined || where.spentBy) return undefined;
      const note: NoteOpening = { hash, mint, amount: o.amount, blinding, state: o.state, kind: noteKind };
      const decoded = await d.wasm.decode(note);
      return decoded.ownerIsMe ? { note, decoded } : undefined;
    } catch {
      return undefined;
    }
  };

  /** Positions a liquidation spent: what they held, until the successor shows what was sold. */
  const liquidated = new Map<string, { at: number; price: bigint; collateral: bigint; debtShares: bigint }>();
  const noteLiquidation = async (p: PositionRecord, signature: string) => {
    const wire = await d.chain.transaction(signature);
    const ix = wire ? lendInstructions(wire, d.programId).find((x) => x.tag === TAG.liquidate) : undefined;
    if (ix) liquidated.set(p.id, { at: await d.chainNow(), price: u64At(ix.data.subarray(1), ARGS.liquidate.bound), collateral: p.collateral, debtShares: p.debtShares });
  };

  /**
   * Receipts and positions: local openings still unspent, plus verified keeper-view openings. The keeper view is read
   * when `useKeeper`, or when a local note was spent by something other than this page (a fill, a liquidation): only
   * the keeper's opening describes its successor.
   */
  const holdings = async (useKeeper: boolean, now: number) => {
    const s = d.store();
    for (const kind of ["receipt", "position"] as const) {
      for (const r of s.all(kind)) {
        const where = await d.chain.note(r.note.hash, r.note.blinding).catch(() => undefined);
        if (!where?.spentBy) continue;
        s.remove(kind, hex(r.note.hash));
        useKeeper = true;
        if (kind === "position") await noteLiquidation(r as PositionRecord, where.spentBy).catch(() => undefined);
      }
    }
    if (useKeeper) {
      const view = await keeper<KeeperAccount>("GET", "/lend/account", {}).catch(() => undefined);
      for (const r of view?.receipts ?? []) {
        if (s.all("receipt").some((x) => x.pool === r.pool)) continue;
        const got = await adopt("receipt", r.pool, r.message);
        if (got) s.put("receipt", hex(got.note.hash), { pool: r.pool, shares: got.decoded.shares, note: got.note });
      }
      for (const p of view?.positions ?? []) {
        if (s.all("position").some((x) => x.id === p.position)) continue;
        const v = cfg.vaults.find((x) => x.id === p.vault);
        if (!v) continue;
        const got = await adopt("position", v.collateralMint, p.message);
        if (!got || got.decoded.positionId?.toString() !== p.position) continue;
        const before = liquidated.get(p.position);
        liquidated.delete(p.position);
        const at = index(v.debtMint, "borrow", now);
        const lastLiquidation = before
          ? { at: before.at, price: before.price, collateralSold: before.collateral - got.note.amount, debtRepaid: debtOf(before.debtShares, at) - debtOf(got.decoded.shares, at) }
          : null;
        s.put("position", hex(got.note.hash), { vault: v.id, id: p.position, collateral: got.note.amount, debtShares: got.decoded.shares, note: got.note, lastLiquidation });
      }
    }
    return { receipts: s.all("receipt"), positions: s.all("position") };
  };

  const index = (pool: string, kind: "supply" | "borrow", now: number): bigint => {
    const seen = d.store().get("index", pool)?.[kind];
    if (!seen) return PRICE_SCALE;
    if (kind === "supply") return seen.index;
    // Debt grows from the last index seen at the published borrow rate (the curve's top if none), rounded up.
    const rate = BigInt(lastFigures.find((f) => f.mint === pool)?.borrowRateBps ?? Math.max(...poolOf(pool).curve.map((c) => c.rateBps)));
    const dt = BigInt(Math.max(0, now - seen.at));
    return seen.index + (seen.index * rate * dt + 10_000n * 31_536_000n - 1n) / (10_000n * 31_536_000n);
  };
  const remember = (pool: string, kind: "supply" | "borrow", value: bigint, at: number) => {
    const s = d.store();
    s.put("index", pool, { ...(s.get("index", pool) ?? { pool }), [kind]: { index: value, at } });
  };

  const verifyTicketReply = async (t: { id?: unknown; expiry?: unknown; signature?: unknown }, purpose: number, target: bigint, amount: bigint, now: number) => {
    if (typeof t.id !== "string" || !/^[0-9a-f]{32}$/.test(t.id) || !Number.isSafeInteger(t.expiry)) throw err("network", "The keeper sent a malformed ticket. Nothing was sent.");
    const expiry = t.expiry as number;
    if (expiry < now + CHECK.intentMinSecs || expiry > now + CHECK.intentMaxSecs) throw err("network", "The keeper's ticket has an expiry outside the allowed window. Nothing was sent; try again.");
    const ok = await verifyTicket(d.keeper, t.signature, { programId: d.programId, wallet: d.wallet(), id: t.id, purpose, target, amount, expiry });
    if (!ok) throw err("network", "The keeper's ticket is not validly signed by the keeper this site was built with. Nothing was sent.");
    return expiry;
  };

  /** supply, deposit, repay: ticket, intent proof, sign, send; resolves at step "keeper". */
  const intent = (kind: LendKind, purpose: Purpose, mint: string, amount: bigint, target: bigint, place: { pool: string; vault: number | null; positionId: string | null }, ticket: Fields, on: (s: LendStep) => void) =>
    exclusive("lend", async () => {
      if (amount <= 0n) throw err("below-minimum", "Enter an amount above zero.");
      await d.sync();
      const source = d.notes(mint).filter((n) => n.amount >= amount).sort((a, b) => (a.amount < b.amount ? -1 : 1))[0];
      if (!source) throw err("insufficient-note", `No single private note of ${asset(mint).symbol} covers that amount. Enter at most your largest note, or deposit more.`);
      on("proving");
      const now = await d.chainNow();
      const t = await keeper<{ id?: unknown; expiry?: unknown; signature?: unknown }>("POST", "/lend/ticket", { purpose, amount: amount.toString(), ...ticket });
      const expiry = await verifyTicketReply(t, PURPOSE[purpose], target, amount, now);
      const built = await d.build.intent({ source: source.utxo, mint, amount, purpose: PURPOSE[purpose], target, expiry }, () => {}).catch((e) => {
        throw e instanceof KeelerError ? e : err("proof", `Proving the request failed: ${message(e)}`);
      });
      const record: OpRecord = {
        id: hex(built.hash),
        kind,
        ...place,
        amounts: [{ asset: asset(mint), amount }],
        step: "signing",
        status: "pending",
        expiry,
        createdAt: now,
        intent: { hash: built.hash, output: built.output, purpose: PURPOSE[purpose], target },
      };
      // Without this record the browser could never refund the intent: no record, no send.
      save(record);
      on("signing");
      return sendSaved(record, built, () => {
        on("keeper");
        return { ...record, step: "keeper" as const };
      });
    });

  /** Signs, saves the signature, sends; a failed signature fails the record, a failed send leaves it to the chain. */
  const sendSaved = async (record: OpRecord, built: Built, after: () => OpRecord): Promise<LendOperation> => {
    let signed: Transaction;
    try {
      signed = await d.sign(built.transaction);
    } catch (e) {
      d.store().put("op", record.id, { ...record, status: "failed", error: message(e) });
      throw e;
    }
    const sent: OpRecord = { ...record, tx: { signature: getSignatureFromTransaction(signed), lastValidBlockHeight: built.lastValidBlockHeight } };
    save(sent);
    try {
      await d.send(signed);
    } catch (e) {
      const said = `${message(e)} It may still reach the chain; its status will update here.`;
      d.store().put("op", record.id, { ...sent, error: said });
      throw err("network", said);
    }
    const done = { ...after(), tx: sent.tx, onChain: true };
    d.store().put("op", done.id, done);
    return toOp(done);
  };

  const parseBuild = (w: WireBuild): BuildReply & { index?: bigint } => {
    const bad = () => err("network", "The keeper sent a malformed transaction. Nothing was sent.");
    if (typeof w?.transaction !== "string" || !Array.isArray(w.outputs) || !Number.isSafeInteger(w.now)) throw bad();
    const big = (v: unknown) => (typeof v === "string" && /^\d+$/.test(v) ? BigInt(v) : v === undefined ? undefined : (() => { throw bad(); })());
    return {
      transaction: toBytes(w.transaction),
      outputs: w.outputs.map((o) => (typeof o === "string" && /^[0-9a-f]{64}$/.test(o) ? fromHex(o) : (() => { throw bad(); })())),
      now: w.now,
      ...(w.bound === undefined ? {} : { bound: big(w.bound)! }),
      ...(w.index === undefined ? {} : { index: big(w.index)! }),
    };
  };

  const mustHave = <T>(v: T | undefined, what: string): T => {
    if (v === undefined) throw err("storage", `This browser has no record of ${what}. Reload the page to read your account again.`);
    return v;
  };

  const keeperBuilt = async (
    kind: "redeem" | "draw",
    request: Fields,
    spent: NoteOpening,
    expect: (reply: BuildReply & { index?: bigint }, owners: Owners) => { notes: ExpectedNote[]; reference?: bigint; creates: (blindings: Map<number, Uint8Array>, outputs: Uint8Array[]) => OpRecord["creates"] },
    place: Pick<LendOperation, "pool" | "vault" | "positionId" | "amounts">,
    on: (s: LendStep) => void,
  ) => {
    on("keeper");
    const reply = parseBuild(await keeper<WireBuild>("POST", "/lend/build", { op: kind, ...request }));
    const owners = await d.owners();
    const want = expect(reply, owners);
    const seed = d.wasm.seed(SEED_DOMAIN[kind], spent.blinding);
    const checked = await checkKeeperTransaction(
      reply,
      { op: kind, programId: d.programId, keeper: d.keeper, owner: d.wallet(), chainNow: await d.chainNow(), spends: d.wasm.nullifier(spent.hash, spent.blinding), ownerViewing: owners.viewing, seed, notes: want.notes, ...(want.reference === undefined ? {} : { reference: want.reference }) },
      d.wasm,
    );
    const blindings = new Map<number, Uint8Array>();
    for (const n of want.notes) blindings.set(n.slot, await d.wasm.blinding(checked.firstNullifier, seed, n.slot));
    const creates = want.creates(blindings, checked.outputs);
    const record: OpRecord = { id: hex(checked.outputs[kind === "redeem" ? SLOT.redeem.receipt : SLOT.draw.position]!), kind, ...place, step: "signing", status: "pending", expiry: null, createdAt: Number(checked.now), spends: spent.hash, ...(creates ? { creates } : {}) };
    save(record);
    on("signing");
    // The keeper's blockhash is seconds old and lives 150 blocks; this bound only decides when an unseen send gave up.
    const lastValidBlockHeight = (await d.chain.blockHeight().catch(() => 0n)) + 300n;
    const op = await sendSaved(record, { transaction: checked.transaction, lastValidBlockHeight }, () => {
      settle(record);
      on("landed");
      return { ...record, status: "landed", step: "landed" };
    });
    d.sync().catch(() => undefined);
    return op;
  };

  const positionRecord = (id: string) => mustHave(d.store().all("position").find((p) => p.id === id), "that position");

  const lend: KeelerLend = {
    async pools() {
      const figures = (await call<KeeperPools>("/api/lend/pools")).pools ?? [];
      lastFigures = figures;
      const now = await d.chainNow().catch(() => Math.floor(Date.now() / 1000));
      const pools: Pool[] = cfg.pools.map((p) => {
        const f = figures.find((x) => x.mint === p.mint);
        return {
          id: p.mint,
          asset: asset(p.mint),
          supplyAprBps: f?.supplyRateBps ?? 0,
          borrowAprBps: f?.borrowRateBps ?? 0,
          utilisationBps: f?.utilisationBps ?? 0,
          size: BigInt(f?.size ?? "0"),
          asOf: f?.at ?? now,
          minSupply: BigInt(p.minSupply),
          feeBps: p.feeBps,
          maxUtilisationBps: p.maxUtilisationBps,
          curve: p.curve.map((c) => ({ ...c })),
        };
      });
      const p = await prices();
      const vaults: Vault[] = cfg.vaults.map((v) => {
        const collateral = asset(v.collateralMint);
        const debt = asset(v.debtMint);
        return {
          id: v.id,
          pool: v.debtMint,
          collateral,
          debt,
          collateralFactorBps: v.cfBps,
          liquidationThresholdBps: v.ltBps,
          liquidationMaxLimitBps: v.lmlBps,
          penaltyBps: v.penaltyBps,
          minCollateral: BigInt(v.minCollateral),
          minDebt: BigInt(v.minDebt),
          ...vaultPrice(p, collateral, debt),
        };
      });
      return { pools, vaults };
    },

    async lendAccount(): Promise<LendAccount> {
      d.sessionSeed(); // throws while locked
      const now = await d.chainNow();
      let found = 0;
      if (!scanned) {
        found = await rescan().catch(() => 0);
        scanned = true;
      }
      const s = d.store();
      const before = new Map(s.all("op").map((r) => [r.id, r.status]));
      const ops = await refreshOps(now);
      // The keeper view is read once a session exists; to recover a wallet that has lent before but whose browser lost
      // its records; and after a fill, whose note only the keeper's opening describes.
      const lost = found > 0 && s.all("receipt").length === 0 && s.all("position").length === 0;
      const filled = ops.some((r) => r.intent && r.status === "landed" && before.get(r.id) !== "landed");
      const held = await holdings(session !== undefined || lost || filled, now);
      const receipts: Receipt[] = held.receipts.map((r) => ({ pool: r.pool, shares: r.shares, value: assetsOf(r.shares, index(r.pool, "supply", now)) }));
      const positions: Position[] = held.positions.map((p) => {
        const v = vaultOf(p.vault);
        return { vault: p.vault, id: p.id, collateral: p.collateral, debt: debtOf(p.debtShares, index(v.debtMint, "borrow", now)), lastLiquidation: p.lastLiquidation };
      });
      return { receipts, positions, operations: ops.sort((a, b) => b.createdAt - a.createdAt).map(toOp), asOf: now };
    },

    supply(pool, amount, on) {
      const p = poolOf(pool);
      if (amount < BigInt(p.minSupply)) return Promise.reject(err("below-minimum", `The pool takes at least ${p.minSupply} base units of ${asset(pool).symbol} at a time. Enter a larger amount.`));
      return intent("supply", "supply", pool, amount, p.id, { pool, vault: null, positionId: null }, { pool }, on);
    },

    deposit(vaultId, positionId, amount, on) {
      const v = vaultOf(vaultId);
      if (positionId === null && amount < BigInt(v.minCollateral)) return Promise.reject(err("below-minimum", `A new position needs at least ${v.minCollateral} base units of ${asset(v.collateralMint).symbol}. Enter a larger amount.`));
      if (positionId !== null) positionRecord(positionId);
      // A new position takes a fresh random id, named by the intent and the ticket.
      const id = positionId ?? (new DataView(crypto.getRandomValues(new Uint8Array(8)).buffer).getBigUint64(0) >> 1n).toString();
      return intent("deposit", "deposit", v.collateralMint, amount, BigInt(id), { pool: v.debtMint, vault: v.id, positionId: id }, { vault: v.id, position: id }, on);
    },

    repay(positionId, amount, on) {
      const p = positionRecord(positionId);
      const v = vaultOf(p.vault);
      return intent("repay", "repay", v.debtMint, amount, BigInt(positionId), { pool: v.debtMint, vault: v.id, positionId }, { vault: v.id, position: positionId }, on);
    },

    redeem: (pool, amount, on) =>
      exclusive("lend", async () => {
        const receipt = mustHave(d.store().all("receipt").find((r) => r.pool === pool), "a deposit in this pool");
        const mint = mintBytes(pool);
        let left = 0n;
        return keeperBuilt(
          "redeem",
          { pool, amount: amount.toString() },
          receipt.note,
          (reply, owners) => {
            if (reply.index === undefined || reply.index < PRICE_SCALE) throw err("network", "The keeper did not state a usable share price. Nothing was sent.");
            const burned = sharesToBurn(amount, reply.index);
            if (burned > receipt.shares) throw err("insufficient-note", "That is more than your deposit is worth. Enter a smaller amount.");
            left = receipt.shares - burned;
            remember(pool, "supply", reply.index, reply.now);
            const state = withLastU64(receipt.note.state, left);
            return {
              notes: [
                { slot: SLOT.redeem.payout, owner: owners.user, mint, amount, kind: NOTE_KIND.token, state: new Uint8Array() },
                { slot: SLOT.redeem.receipt, owner: owners.pda, mint, amount: 0n, kind: NOTE_KIND.receipt, state },
              ],
              creates: (b, outputs) => ({ receipt: { pool, shares: left, note: { hash: outputs[SLOT.redeem.receipt]!, mint: pool, amount: 0n, blinding: b.get(SLOT.redeem.receipt)!, state, kind: NOTE_KIND.receipt } } }),
            };
          },
          { pool, vault: null, positionId: null, amounts: [{ asset: asset(pool), amount }] },
          on,
        );
      }),

    draw: (positionId, borrow, withdraw, on) =>
      exclusive("lend", async () => {
        if (borrow < 0n || withdraw < 0n || (borrow === 0n && withdraw === 0n)) throw err("below-minimum", "Enter an amount to borrow or to withdraw.");
        const p = positionRecord(positionId);
        const v = vaultOf(p.vault);
        if (withdraw > p.collateral) throw err("unhealthy", "That is more collateral than the position holds.");
        const reference = vaultPrice(await prices(), asset(v.collateralMint), asset(v.debtMint)).price;
        const [collateralMint, debtMint] = [mintBytes(v.collateralMint), mintBytes(v.debtMint)];
        return keeperBuilt(
          "draw",
          { vault: v.id, position: positionId, borrow: borrow.toString(), withdraw: withdraw.toString() },
          p.note,
          (reply, owners) => {
            if (reply.index === undefined || reply.index < PRICE_SCALE) throw err("network", "The keeper did not state a usable debt price. Nothing was sent.");
            const debtShares = p.debtShares + (borrow > 0n ? borrowShares(borrow, reply.index) : 0n);
            remember(v.debtMint, "borrow", reply.index, reply.now);
            const state = withLastU64(p.note.state, debtShares);
            const collateral = p.collateral - withdraw;
            return {
              reference,
              notes: [
                { slot: SLOT.draw.position, owner: owners.pda, mint: collateralMint, amount: collateral, kind: NOTE_KIND.position, state },
                { slot: SLOT.draw.debtPayout, owner: owners.user, mint: debtMint, amount: borrow, kind: NOTE_KIND.token, state: new Uint8Array() },
                { slot: SLOT.draw.collateralPayout, owner: owners.user, mint: collateralMint, amount: withdraw, kind: NOTE_KIND.token, state: new Uint8Array() },
              ],
              creates: (b, outputs) => ({
                position: { ...p, collateral, debtShares, note: { hash: outputs[SLOT.draw.position]!, mint: v.collateralMint, amount: collateral, blinding: b.get(SLOT.draw.position)!, state, kind: NOTE_KIND.position } },
              }),
            };
          },
          {
            pool: v.debtMint,
            vault: v.id,
            positionId,
            amounts: [...(borrow > 0n ? [{ asset: asset(v.debtMint), amount: borrow }] : []), ...(withdraw > 0n ? [{ asset: asset(v.collateralMint), amount: withdraw }] : [])],
          },
          on,
        );
      }),

    closePosition: (positionId, on) =>
      exclusive("lend", async () => {
        const p = positionRecord(positionId);
        if (p.debtShares > 0n) throw err("unhealthy", "This position still has debt. Repay it all, then close.");
        const v = vaultOf(p.vault);
        const where = await d.chain.note(p.note.hash, p.note.blinding);
        if (where.leafIndex === undefined || where.spentBy) throw err("storage", "This position is not on chain as this browser knows it. Reload the page to read your account again.");
        on("proving");
        const built = await d.build.close({ ...p.note, leafIndex: where.leafIndex }, () => {}).catch((e) => {
          throw e instanceof KeelerError ? e : err("proof", `Proving the close failed: ${message(e)}`);
        });
        const now = await d.chainNow();
        const record: OpRecord = { id: `close:${hex(p.note.hash)}`, kind: "close", pool: v.debtMint, vault: v.id, positionId, amounts: [{ asset: asset(v.collateralMint), amount: p.collateral }], step: "signing", status: "pending", expiry: null, createdAt: now, spends: p.note.hash };
        save(record);
        on("signing");
        const op = await sendSaved(record, built, () => {
          settle(record);
          on("landed");
          return { ...record, status: "landed", step: "landed" };
        });
        await d.sync().catch(() => undefined);
        return op;
      }),

    refundIntent: (operationId) =>
      exclusive(`refund:${operationId}`, async () => {
        const s = d.store();
        let r = s.get("op", operationId);
        if (!r) {
          await rescan();
          r = s.get("op", operationId);
        }
        if (!r?.intent) throw err("storage", "No unfilled request with that id was found in this browser or on chain.");
        const now = await d.chainNow();
        const fresh = await refresh(r, now).catch((e) => {
          throw err("network", `Reading the request's status failed: ${message(e)}`);
        });
        s.put("op", fresh.id, fresh);
        if (fresh.status !== "refundable") {
          throw err("quote-expired", fresh.status === "pending" ? `You can take the funds back after the request expires, ${new Date((fresh.expiry ?? 0) * 1000).toLocaleTimeString()} (chain time).` : `The request is ${fresh.status}; there is nothing to refund.`);
        }
        const where = await d.chain.note(fresh.intent!.hash, fresh.intent!.output.blinding);
        if (where.leafIndex === undefined) throw err("network", "The request is not indexed yet. Try again in a minute.");
        const built = await d.build.cancel({ hash: fresh.intent!.hash, output: fresh.intent!.output, leafIndex: where.leafIndex }, () => {}).catch((e) => {
          throw e instanceof KeelerError ? e : err("proof", `Proving the refund failed: ${message(e)}`);
        });
        try {
          await d.send(await d.sign(built.transaction));
        } finally {
          // The intent's nullifier, not the send result, says what happened: the keeper may have filled it first.
          const after = await refresh(fresh, await d.chainNow()).catch(() => fresh);
          s.put("op", after.id, after);
        }
        await d.sync().catch(() => undefined);
      }),
  };
  return lend;
}
