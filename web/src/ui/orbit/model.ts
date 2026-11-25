// What the Orbit scenes read, as pure functions of the product's state: no DOM, safe in node tests and the first bundle.
import type { Asset, LendKind, LendStatus, OrderStatus } from "../../keeler/index";

export type DeskTab = "earn" | "borrow" | "swap";
export type DeskMode = "supply" | "redeem" | "add" | "draw" | "repay" | "remove" | "close";
export type DeskPhase = "edit" | "quotes" | "review" | "settled";

/**
 * What a desk's ring shows. `fraction`: the validated amount over the spendable limit, 0..1 in steps of 1/1000.
 * `ltv`: loan-to-value as a fraction, 0..1.25 (Borrow only). `sell` and `lane`: the swap's direction (the asset pair's
 * first asset is "base") and the selected dealer's index in `config.dealers`. `receiptId`: set on "settled" only.
 * `mode` is unused for `tab: "swap"`. `review` means in flight.
 */
export type DeskModel = {
  tab: DeskTab;
  mode: DeskMode;
  phase: DeskPhase;
  fraction: number;
  valid: boolean;
  ltv?: number;
  sell?: "base" | "quote";
  lane?: 0 | 1;
  receiptId?: string;
};

/** A desk form as it checks itself: `check()`'s `parsed` and `ok` (or the swap form's), and the maximum it checks against. */
export type FormInput = { parsed: bigint | null; ok: boolean; limit: bigint | null };

/**
 * A lending operation as `useOperation` holds it (`run`, `live`), and `submitted`: the desk's model at the moment of
 * submit, kept by the caller, so the ring keeps that amount while the balances it was measured against change.
 */
export type LendOpInput = {
  run: { kind: LendKind; amounts: readonly { asset: Asset; amount: bigint }[]; id: string | null; failed: string | null } | null;
  live: { id: string; status: LendStatus } | null;
  submitted: Pick<DeskModel, "fraction" | "ltv"> | null;
};

/** Earn: `mode` is the form's "in" (deposit) or "out" (withdraw); `limit` the form's `max`. */
export type EarnInput = FormInput & LendOpInput & { mode: "in" | "out" };

/**
 * Borrow: `action` and `limit` as the form has them (`limits[act].max`), `collateral` the vault's collateral asset,
 * `health` and `projected` the position's loan-to-value now and after the typed amount, in basis points (`healthBps`).
 */
export type BorrowInput = FormInput &
  LendOpInput & { action: "add" | "borrow" | "repay" | "withdraw"; collateral: Asset; health: number; projected: number };

/**
 * Swap: mints of the pair's first asset (`base`) and of the asset sold, `config.dealers`, the swap view (for quotes,
 * `selected` is the usable quote's dealer, as the form shows it chosen), the refreshed order list, and `submitted`
 * as for lending.
 */
export type SwapInput = FormInput & {
  base: string;
  sell: string;
  dealers: readonly { pubkey: string }[];
  view:
    | { kind: "form" }
    | { kind: "quotes"; selected: string | null }
    | { kind: "progress"; quote: { dealer: string }; order: { id: string; status: OrderStatus; error?: string } | null; failed: string | null };
  orders: readonly { id: string; status: OrderStatus; error?: string }[];
  submitted: Pick<DeskModel, "fraction"> | null;
};

/** `amount / limit` clamped to 0..1 and rounded to 1/1000; 0 without an amount or a limit above zero. */
export function amountFraction(amount: bigint | null, limit: bigint | null): number {
  if (amount === null || limit === null || limit <= 0n || amount <= 0n) return 0;
  if (amount >= limit) return 1;
  return Number((amount * 1000n + limit / 2n) / limit) / 1000;
}

/** Basis points as a fraction, rounded to whole basis points, clamped to 0..1.25 (Infinity, debt against nothing, is 1.25). */
export function ltvFraction(bps: number): number {
  if (Number.isNaN(bps)) return 0;
  return Math.min(1.25, Math.max(0, Math.round(bps) / 10_000));
}

const IDLE = { phase: "edit", valid: false, fraction: 0 } as const;

function formPart(f: FormInput) {
  const valid = f.ok && f.parsed !== null;
  return { phase: "edit" as DeskPhase, valid, fraction: valid ? amountFraction(f.parsed, f.limit) : 0 };
}

function lendPart(op: LendOpInput, close: boolean): Part | null {
  if (!op.run) return null;
  if (op.run.failed) return IDLE;
  const fraction = close ? 1 : (op.submitted?.fraction ?? 0);
  const status = op.live?.status;
  if (status === "landed") return { phase: "settled", valid: true, fraction, receiptId: op.live!.id };
  if (status === "refundable" || status === "refunded" || status === "failed") return IDLE;
  return { phase: "review", valid: true, fraction };
}

type Part = { phase: DeskPhase; valid: boolean; fraction: number; receiptId?: string };

function model(base: Pick<DeskModel, "tab" | "mode">, part: Part, extra: Pick<DeskModel, "ltv" | "sell" | "lane"> = {}): DeskModel {
  const m: DeskModel = { ...base, phase: part.phase, fraction: part.fraction, valid: part.valid };
  if (extra.ltv !== undefined) m.ltv = extra.ltv;
  if (extra.sell !== undefined) m.sell = extra.sell;
  if (extra.lane !== undefined) m.lane = extra.lane;
  if (part.receiptId !== undefined) m.receiptId = part.receiptId;
  return m;
}

const LEND_MODE: Record<Exclude<LendKind, "draw">, DeskMode> = { supply: "supply", redeem: "redeem", deposit: "add", repay: "repay", close: "close" };

function runMode(run: NonNullable<LendOpInput["run"]>, collateral?: Asset): DeskMode {
  if (run.kind !== "draw") return LEND_MODE[run.kind];
  return run.amounts.some((a) => a.asset.mint !== collateral?.mint && a.amount > 0n) ? "draw" : "remove";
}

/** The Earn desk's model; `null` while there is nothing to read (no pool yet, or the wallet is not ready). */
export function earnModel(input: EarnInput | null): DeskModel {
  if (!input) return model({ tab: "earn", mode: "supply" }, IDLE);
  const lend = lendPart(input, false);
  const mode = input.run ? runMode(input.run) : input.mode === "in" ? "supply" : "redeem";
  return model({ tab: "earn", mode }, lend ?? formPart(input));
}

const BORROW_MODE = { add: "add", borrow: "draw", repay: "repay", withdraw: "remove" } as const;

/** The Borrow desk's model; `null` while there is nothing to read (no vault yet, or the wallet is not ready). */
export function borrowModel(input: BorrowInput | null): DeskModel {
  if (!input) return model({ tab: "borrow", mode: "add" }, IDLE, { ltv: 0 });
  const now = ltvFraction(input.health);
  if (input.run) {
    const part = lendPart(input, input.run.kind === "close")!;
    const ltv = part.phase === "review" ? (input.submitted?.ltv ?? now) : now;
    return model({ tab: "borrow", mode: runMode(input.run, input.collateral) }, part, { ltv });
  }
  const part = formPart(input);
  return model({ tab: "borrow", mode: BORROW_MODE[input.action] }, part, { ltv: part.valid ? ltvFraction(input.projected) : now });
}

function laneOf(dealers: readonly { pubkey: string }[], dealer: string | null): 0 | 1 | undefined {
  const i = dealer === null ? -1 : dealers.findIndex((d) => d.pubkey === dealer);
  return i === 0 || i === 1 ? i : undefined;
}

/** The Swap desk's model; `null` while there is nothing to read (no config yet, or the wallet is not ready). */
export function swapModel(input: SwapInput | null): DeskModel {
  const tab = { tab: "swap", mode: "supply" } as const;
  if (!input) return model(tab, IDLE);
  const sell = input.sell === input.base ? "base" : "quote";
  const { view } = input;
  if (view.kind === "form") return model(tab, formPart(input), { sell });
  if (view.kind === "quotes") return model(tab, { ...formPart(input), phase: "quotes" }, { sell, lane: laneOf(input.dealers, view.selected) });
  if (view.failed) return model(tab, IDLE, { sell });
  const order = view.order ? (input.orders.find((o) => o.id === view.order!.id) ?? view.order) : null;
  const lane = laneOf(input.dealers, view.quote.dealer);
  const fraction = input.submitted?.fraction ?? 0;
  if (order?.status === "settled" && !order.error) return model(tab, { phase: "settled", valid: true, fraction, receiptId: order.id }, { sell, lane });
  if (order && order.status !== "pending" && order.status !== "locked") return model(tab, IDLE, { sell });
  return model(tab, { phase: "review", valid: true, fraction }, { sell, lane });
}

/** A set of receipts that have played. `claim(id)` is true the first time it sees an id and false after. */
export type ReceiptGate = { claim(receiptId: string): boolean };

/** A new receipt gate. Keep one for the page's life (the desk scene keeps one at module level), so a remount never replays. */
export function receiptGate(): ReceiptGate {
  const played = new Set<string>();
  return {
    claim(receiptId) {
      if (played.has(receiptId)) return false;
      played.add(receiptId);
      return true;
    },
  };
}

/** The landing flight's camera stops: overview, supply, borrow, risk, privacy, closing. */
export const flightStops = [0, 0.22, 0.45, 0.66, 0.84, 1] as const;

/** The two still frames to crossfade at `progress` ("wide-2", "tall-3"), `to` shown at opacity `mix` over `from`. */
export function fallbackFrames(progress: number, tall: boolean): { from: string; to: string; mix: number } {
  const p = Number.isFinite(progress) ? Math.min(1, Math.max(0, progress)) : 0;
  let lower = 0;
  flightStops.forEach((stop, i) => {
    if (stop <= p) lower = i;
  });
  const upper = Math.min(flightStops.length - 1, lower + 1);
  const mix = upper === lower ? 0 : (p - flightStops[lower]!) / (flightStops[upper]! - flightStops[lower]!);
  const name = (i: number) => `${tall ? "tall" : "wide"}-${i}`;
  return { from: name(lower), to: name(upper), mix };
}
