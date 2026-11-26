// The Earn and Borrow desks and the lending activity list (docs/plan-lend.md); a sentence that says what a
// money-moving button does stays next to that button.
import { useRef, useState } from "react";
import type { Asset, Balance, KeelerLend, LendAccount, LendKind, LendOperation, LendStep, Pool, Position, Vault } from "../keeler/index";
import type { MockLendControls } from "../keeler/mock-lend";
import { useReport } from "./app/ring";
import type { Report } from "./app/ring";
import { countdown, formatUnits, parseUnits } from "./format";
import { borrowLimits, drawPrice, earnLimits, grow, healthBps, liquidatable, liquidationPrice, maxBorrow, pct, priceText } from "./lend-view";
import { borrowModel, earnModel } from "./orbit/model";
import type { DeskModel } from "./orbit/model";
import { Amount, amountText, ErrorNote, errorText, Funds, OnChainNotes, ON_CHAIN, OrbLinks, RefundNotice, useChainNow } from "./shared";
import type { OrbLink } from "./shared";

export type Market = { pools: Pool[]; vaults: Vault[] };

export type LendProps = {
  lend: KeelerLend;
  balances: Balance[] | null;
  market: Market | null;
  account: LendAccount | null;
  chainOffset: number | null;
  /** `prompt`: the wallet may ask to allow the lending session (only on the reader's click). */
  refreshLend: (prompt?: boolean) => Promise<unknown>;
  refreshBalances: () => Promise<unknown>;
};

type DeskProps = LendProps & { lend: KeelerLend; refundable: number; report: Report; toBalances: () => void };

const FLOW: Record<LendKind, LendStep[]> = {
  supply: ["proving", "signing", "keeper", "landed"],
  deposit: ["proving", "signing", "keeper", "landed"],
  repay: ["proving", "signing", "keeper", "landed"],
  redeem: ["keeper", "signing", "landed"],
  draw: ["keeper", "signing", "landed"],
  close: ["proving", "signing", "landed"],
};
const STEP_LABEL: Record<LendStep, string> = {
  proving: "Proving in your browser",
  signing: "Sign in your wallet",
  keeper: "Waiting for the keeper",
  landed: "Landed",
};
const LANDED: Record<LendKind, string> = {
  supply: "Done. Your deposit is in the pool and earning.",
  redeem: "Done. The withdrawal is in your private balance.",
  deposit: "Done. The collateral is in your position.",
  draw: "Done. It's in your private balance.",
  repay: "Done. The repayment is applied.",
  close: "Done. The position is closed and the collateral is in your private balance.",
};
const STEP_SHORT: Record<LendStep, string> = { proving: "Proving", signing: "Signing", keeper: "Keeper", landed: "Landed" };

const isIntent = (op: LendOperation) => op.kind === "supply" || op.kind === "deposit" || op.kind === "repay";

/** The transaction this page sent and, for an intent, the keeper's fill or the owner's refund. */
export function opLinks(op: LendOperation): OrbLink[] {
  return [
    { signature: op.signature, label: isIntent(op) ? "View request on Orb" : "View transaction on Orb" },
    { signature: op.spentBy, label: op.status === "refunded" ? "View refund on Orb" : "View keeper's fill on Orb" },
  ];
}

/** What is public for the links `opLinks` shows: a request alone or with its refund names no pool or vault. */
export const opSays = (op: LendOperation) => (isIntent(op) && (!op.spentBy || op.status === "refunded") ? ON_CHAIN.request : ON_CHAIN.lend);

const liquidationLinks = (signature?: string): OrbLink[] => [{ signature, label: "View liquidation on Orb" }];

function NoPrivate({ asset, then, toBalances }: { asset: Asset; then: string; toBalances: () => void }) {
  return (
    <div className="empty">
      <p>
        Your wallet's private balance has no {asset.symbol}. Move some in from your public balance, then {then}.
      </p>
      <button type="button" className="button button-primary main-action" onClick={toBalances}>
        Go to balances
      </button>
    </div>
  );
}

export function kindLabel(o: Pick<LendOperation, "kind" | "amounts">, collateral?: Asset): string {
  switch (o.kind) {
    case "supply":
      return "Deposit to Earn";
    case "redeem":
      return "Withdraw from Earn";
    case "deposit":
      return "Add collateral";
    case "repay":
      return "Repay";
    case "close":
      return "Close position";
    case "draw": {
      const out = o.amounts.some((a) => a.asset.mint === collateral?.mint);
      const borrow = o.amounts.some((a) => a.asset.mint !== collateral?.mint);
      return borrow && out ? "Borrow and withdraw" : out ? "Withdraw collateral" : "Borrow";
    }
  }
}

/** A value growing at `rateBps` a year since `asOf`, recomputed each second from chain time. */
const grown = (value: bigint, rateBps: number, asOf: number, now: number | null) => (now === null ? value : grow(value, rateBps, now - asOf));

const plain = (v: bigint, a: Asset) => formatUnits(v, a.decimals).replace(/,/g, "");

/** The step list for one operation; `failed` marks the step it stopped at. */
export function OpSteps({ kind, step, status, failed }: { kind: LendKind; step: LendStep | null; status?: LendOperation["status"]; failed?: boolean }) {
  const flow = FLOW[kind];
  const at = status === "landed" ? flow.length - 1 : step ? flow.indexOf(step) : -1;
  return (
    <ol className="steps" aria-label="Progress">
      {flow.map((s, i) => {
        const refundable = s === "keeper" && (status === "refundable" || status === "refunded");
        const state = failed && i === at ? "fail" : i < at || (i === at && s === "landed") ? "done" : i === at ? "now" : "todo";
        return (
          <li key={s} className={`step step-${refundable ? "warn" : state}`} aria-current={state === "now" ? "step" : undefined}>
            <span className="step-dot" aria-hidden="true" />
            <span>
              {refundable ? "The keeper did not take it in" : STEP_LABEL[s]}
              {s === "proving" && state === "now" && <span className="muted step-sub">Keep this tab open.</span>}
              {s === "keeper" && state === "now" && kind !== "redeem" && kind !== "draw" && (
                <span className="muted step-sub">Usually under a minute.</span>
              )}
            </span>
          </li>
        );
      })}
    </ol>
  );
}

type Running = { kind: LendKind; amounts: { asset: Asset; amount: bigint }[]; id: string | null; step: LendStep | null; failed: string | null };

/** Runs one operation at a time; its live state comes from the account once the client returns its id. */
function useOperation(p: LendProps) {
  const [run, setRun] = useState<Running | null>(null);
  const [formError, setFormError] = useState<string | null>(null);
  const start = async (kind: LendKind, amounts: Running["amounts"], fn: (on: (s: LendStep) => void) => Promise<LendOperation>) => {
    setFormError(null);
    setRun({ kind, amounts, id: null, step: null, failed: null });
    let started = false;
    try {
      const op = await fn((step) => {
        started = true;
        setRun((r) => r && { ...r, step });
      });
      setRun((r) => r && { ...r, id: op.id, step: op.step });
    } catch (e) {
      // Refused before anything was sent: back to the form with the reason. Otherwise the progress view shows it.
      if (!started) {
        setRun(null);
        setFormError(errorText(e));
      } else setRun((r) => r && { ...r, failed: errorText(e) });
    }
    await Promise.all([p.refreshLend(), p.refreshBalances()]).catch(() => {});
  };
  const live = run?.id ? (p.account?.operations.find((o) => o.id === run.id) ?? null) : null;
  return { run, live, start, formError, setFormError, clear: () => setRun(null) };
}

function Progress({ p, op, onDone }: { p: LendProps; op: ReturnType<typeof useOperation>; onDone: () => void }) {
  const { run, live } = op;
  const now = useChainNow(p.chainOffset);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (!run) return null;
  const status = live?.status;
  const step = live?.step ?? run.step;
  const left = live?.expiry != null && now !== null ? live.expiry - now : null;
  const what = run.amounts.map((a, i) => (
    <span key={i}>
      {i > 0 && " and "}
      <Amount value={a.amount} asset={a.asset} />
    </span>
  ));

  let line: string;
  if (run.failed) line = "It stopped.";
  else if (status === "landed") line = LANDED[run.kind];
  else if (status === "refunded") line = "Done. The funds are back in your private balance.";
  else if (status === "refundable") line = "The keeper did not take it in before the request expired. The funds are still yours: take them back.";
  else if (status === "failed") line = live?.error ?? "It failed.";
  else line = step ? `${STEP_LABEL[step]}…` : "Starting… Your wallet may ask once to allow this lending session.";

  const refund = async () => {
    setBusy(true);
    setError(null);
    try {
      await p.lend.refundIntent(live!.id);
      await Promise.all([p.refreshLend(), p.refreshBalances()]);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="ticket-flow">
      {status === "landed" && <span className="result-mark" aria-hidden="true" />}
      <p className="review-lede">
        {kindLabel(run)}: {what}
      </p>
      <OpSteps kind={run.kind} step={step} status={status} failed={!!run.failed} />
      {status === "pending" && step === "keeper" && left !== null && (
        <p className="ticket-foot num">{left > 0 ? `If it isn't taken in, you can take it back in ${countdown(left)}.` : "Request expired, checking…"}</p>
      )}
      <p className={`status${run.failed || status === "failed" ? " bad" : ""}`} aria-live="polite">
        {line}
      </p>
      {live && <OrbLinks links={opLinks(live)} says={opSays(live)} />}
      {run.failed && (
        <p className="note note-error" role="alert">
          {run.failed}
        </p>
      )}
      {status === "refundable" && (
        <button className="button button-primary main-action" onClick={refund} disabled={busy} aria-busy={busy}>
          {busy ? "Taking funds back…" : "Take funds back"}
        </button>
      )}
      {(run.failed || status === "landed" || status === "refunded" || status === "failed") && (
        <button className="button button-primary main-action" onClick={onDone}>
          Done
        </button>
      )}
      <ErrorNote text={error} />
    </div>
  );
}

function Field(props: {
  id: string;
  label: string;
  asset: Asset;
  text: string;
  setText: (t: string) => void;
  invalid: string | null;
  hint: React.ReactNode;
  max: bigint | null;
  maxLabel?: string;
}) {
  const { id, asset } = props;
  return (
    <div className="money-field">
      <div className="field-frame">
        <label className="instrument-label" htmlFor={id}>
          {props.label}
        </label>
        <div className="amount-row">
          <input
            id={id}
            inputMode="decimal"
            autoComplete="off"
            placeholder="0"
            value={props.text}
            onChange={(e) => props.setText(e.target.value)}
            aria-invalid={!!props.invalid}
            aria-describedby={`${id}-hint`}
          />
          <span className="asset-static">{asset.symbol}</span>
        </div>
      </div>
      <div className="field-hint" id={`${id}-hint`}>
        {props.invalid ? <span className="bad">{props.invalid}</span> : <span>{props.hint}</span>}
        {props.max !== null && props.max > 0n && (
          <button type="button" className="inline-button" onClick={() => props.setText(plain(props.max!, asset))}>
            {props.maxLabel ?? "Use maximum"}
          </button>
        )}
      </div>
    </div>
  );
}

/** Parses the field and checks it against a maximum; the error shows once the person has tried. */
function check(text: string, asset: Asset, shown: boolean, max: bigint | null, tooMuch: string, min?: { value: bigint; text: string }) {
  const parsed = parseUnits(text, asset.decimals, asset.symbol);
  let invalid = typeof parsed === "string" ? parsed : null;
  if (!invalid && typeof parsed === "bigint") {
    if (max !== null && parsed > max) invalid = tooMuch;
    else if (min && parsed < min.value) invalid = min.text;
  }
  return { parsed: typeof parsed === "bigint" ? parsed : null, invalid: shown ? invalid : null, ok: !invalid };
}

function DeskLayout({ p, title, rail, railLabel, children }: { p: DeskProps; title: string; rail: React.ReactNode; railLabel: string; children: React.ReactNode }) {
  return (
    <div className="workspace-grid lending-workspace">
      <section className="ticket" aria-label={title}>
        <div className="ticket-header">
          <h2>{title}</h2>
        </div>
        <div className="ticket-body">
          <RefundNotice count={p.refundable} />
          <KeeperCheck p={p} />
          {children}
        </div>
      </section>
      <aside className="context-rail" aria-label={railLabel}>
        {rail}
        <Funds balances={p.balances} />
      </aside>
    </div>
  );
}

function KeeperCheck({ p }: { p: LendProps }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (!p.account?.needsSession) return null;
  return (
    <div className="note keeper-check">
      The keeper may have news for your lending (a fill, or a change made in another browser).{" "}
      <button
        className="button button-secondary button-small"
        disabled={busy}
        onClick={() => {
          setBusy(true);
          setError(null);
          p.refreshLend(true)
            .catch((e) => setError(errorText(e)))
            .finally(() => setBusy(false));
        }}
      >
        Check with the keeper
      </button>{" "}
      Your wallet asks once to allow this lending session.
      <ErrorNote text={error} />
    </div>
  );
}

const amountsText = (amounts: Running["amounts"]) => amounts.map((x) => amountText(x.amount, x.asset)).join(" and ");

function earnForm(p: LendProps, pool: Pool, chosen: "in" | "out", text: string, shown: boolean, now: number | null) {
  const a = pool.asset;
  // Every receipt in this pool counts towards the deposit; one withdrawal spends one receipt, so the largest caps it.
  const { mine, largestReceipt, note } = earnLimits(pool, p.account?.receipts ?? null, p.balances);
  const receipt = mine.length ? { value: mine.reduce((t, r) => t + r.value, 0n) } : undefined;
  // With nothing left to withdraw the form goes back to depositing.
  const mode = receipt ? chosen : "in";
  const value = receipt ? grown(receipt.value, pool.supplyAprBps, p.account!.asOf, now) : 0n;
  const max = mode === "in" ? note : largestReceipt;
  const f = check(
    text,
    a,
    shown,
    max,
    mode === "in"
      ? `One deposit spends one private note, and your largest is ${formatUnits(note ?? 0n, a.decimals)} ${a.symbol}.`
      : mine.length > 1
        ? `One withdrawal spends one deposit, and your largest is worth ${formatUnits(max ?? 0n, a.decimals)} ${a.symbol}. Enter that or less.`
        : `Your deposit is worth ${formatUnits(max ?? 0n, a.decimals)} ${a.symbol}. Enter that or less.`,
    mode === "in" ? { value: pool.minSupply, text: `The pool takes at least ${formatUnits(pool.minSupply, a.decimals)} ${a.symbol}.` } : undefined,
  );
  return { a, receipt, mode, value, note, max, f };
}

export function EarnTab(p: DeskProps) {
  const pool = p.market?.pools[0];
  const [chosen, setMode] = useState<"in" | "out">("in");
  const [text, setText] = useState("");
  const [shown, setShown] = useState(false);
  const op = useOperation(p);
  const now = useChainNow(p.chainOffset);
  const submitted = useRef<Pick<DeskModel, "fraction" | "ltv"> | null>(null);

  const form = pool ? earnForm(p, pool, chosen, text, shown, now) : null;
  const input = form && { parsed: form.f.parsed, ok: form.f.ok, limit: form.max, mode: form.mode };
  const edit = earnModel(input && { ...input, run: null, live: null, submitted: null });
  if (!op.run) submitted.current = { fraction: edit.fraction };
  const model = earnModel(input && { ...input, run: op.run, live: op.live, submitted: submitted.current });
  let label = "Your deposit";
  const unread = !p.account || p.account.needsSession;
  let value = form && !(unread && form.value === 0n) ? amountText(form.value, form.a) : "—";
  if (form && op.run && model.phase === "review") [label, value] = [kindLabel(op.run), amountsText(op.run.amounts)];
  else if (form && !op.run && model.valid && form.f.parsed !== null) {
    const after = form.mode === "in" ? form.value + form.f.parsed : form.value > form.f.parsed ? form.value - form.f.parsed : 0n;
    [label, value] = [form.mode === "in" ? "Your deposit after this" : "Your deposit after this withdrawal", amountText(after, form.a)];
  }
  useReport("earn", { model, label, value }, p.report);

  if (!pool || !form)
    return (
      <DeskLayout p={p} title="Earn" rail={null} railLabel="Balances">
        <p className="muted">Reading the pool…</p>
      </DeskLayout>
    );

  const { a, receipt, mode, note, max, f } = form;
  const submit = () => {
    setShown(true);
    if (!f.ok || f.parsed === null) return;
    const amount = f.parsed;
    op.start(mode === "in" ? "supply" : "redeem", [{ asset: a, amount }], (on) =>
      // The whole deposit's worth withdraws every share, so no dust stays in the receipt.
      mode === "in" ? p.lend.supply(pool.id, amount, on) : p.lend.redeem(pool.id, amount === max ? "all" : amount, on),
    );
  };

  const rail = (
    <>
      <PoolCard pool={pool} />
      <section className="rail-panel mine" aria-label="Your deposit">
        <div className="rail-head">
          <h2>Your deposit</h2>
        </div>
        {receipt ? (
          <div className="stat">
            <span className="mine-big num" aria-live="off">
              <Amount value={form.value} asset={a} />
            </span>
            <span className="rail-foot">
              Growing at <span className="good">{pct(pool.supplyAprBps, 2)}</span> a year. Lending amounts are visible to you and to Keeler's
              keeper service.
            </span>
          </div>
        ) : (
          <p className="rail-foot">Nothing deposited yet. Your first deposit starts earning as soon as the keeper takes it in.</p>
        )}
      </section>
    </>
  );

  return (
    <DeskLayout p={p} title={op.run ? kindLabel(op.run) : mode === "in" ? `Deposit ${a.symbol}` : `Withdraw ${a.symbol}`} rail={rail} railLabel="Pool and balances">
      {op.run ? (
        <Progress
          p={p}
          op={op}
          onDone={() => {
            op.clear();
            setText("");
            setShown(false);
          }}
        />
      ) : (
        <form
          className="ticket-flow"
          noValidate
          onSubmit={(e) => {
            e.preventDefault();
            submit();
          }}
        >
          <div className="mode-picker" role="group" aria-label="Earn action">
            <button type="button" aria-pressed={mode === "in"} onClick={() => { setMode("in"); setShown(false); }}>
              Deposit
            </button>
            <button type="button" aria-pressed={mode === "out"} onClick={() => { setMode("out"); setShown(false); }} disabled={!receipt}>
              Withdraw
            </button>
          </div>
          <Field
            id="earn-amt"
            label={mode === "in" ? "Deposit to the pool" : "Withdraw from the pool"}
            asset={a}
            text={text}
            setText={setText}
            invalid={f.invalid}
            max={max}
            maxLabel={mode === "out" ? "Withdraw all" : undefined}
            hint={
              max === null ? (
                "Reading your private balance…"
              ) : mode === "in" ? (
                <>
                  Largest private note <Amount value={max} asset={a} />
                </>
              ) : (
                <>
                  Deposit worth <Amount value={max} asset={a} />
                </>
              )
            }
          />
          <dl className="form-summary">
            <div>
              <dt>From</dt>
              <dd>{mode === "in" ? "Private balance" : `${a.symbol} pool`}</dd>
            </div>
            <div>
              <dt>To</dt>
              <dd>{mode === "in" ? `${a.symbol} pool` : "Private balance"}</dd>
            </div>
            <div>
              <dt>Supply rate</dt>
              <dd>{pct(pool.supplyAprBps, 2)} a year, floating</dd>
            </div>
          </dl>
          {mode === "in" && note === 0n ? (
            <NoPrivate asset={a} then="deposit it here to earn" toBalances={p.toBalances} />
          ) : (
            <button className="button button-primary main-action" type="submit">
              {mode === "in" ? "Deposit and earn" : "Withdraw"}
            </button>
          )}
          <p className="ticket-foot">
            {mode === "in"
              ? "Your browser proves the deposit and your wallet signs it; the keeper then adds it to the pool. If it isn't added before the request expires, you take it back in one click."
              : "The keeper builds the withdrawal and your wallet signs it. It is paid into your private balance in that one transaction."}
          </p>
        </form>
      )}
      <ErrorNote text={op.formError} />
    </DeskLayout>
  );
}

export function PoolCard({ pool }: { pool: Pool }) {
  return (
    <section className="rail-panel pool" aria-label={`${pool.asset.symbol} pool`}>
      <div className="rail-head">
        <h2>{pool.asset.symbol} pool</h2>
        <span className="rail-tag">Floating rate</span>
      </div>
      <div className="apr">
        <span className="apr-big num">{pct(pool.supplyAprBps, 2)}</span>
        <span className="instrument-label">supply APR</span>
      </div>
      <dl className="readings">
        <div>
          <dt>Borrow APR</dt>
          <dd className="num">{pct(pool.borrowAprBps, 2)}</dd>
        </div>
        <div>
          <dt>Utilisation</dt>
          <dd className="num">{pct(pool.utilisationBps, 0)}</dd>
        </div>
        <div>
          <dt>Pool size</dt>
          <dd>
            <Amount value={pool.size} asset={pool.asset} />
          </dd>
        </div>
        <div>
          <dt>Smallest deposit</dt>
          <dd>
            <Amount value={pool.minSupply} asset={pool.asset} />
          </dd>
        </div>
      </dl>
      <div className="util" role="meter" aria-label="Utilisation" aria-valuemin={0} aria-valuemax={100} aria-valuenow={pool.utilisationBps / 100}>
        <span style={{ width: `${Math.min(pool.utilisationBps, 10_000) / 100}%` }} />
      </div>
      <p className="rail-foot">The lending keeper can see your deposit and position amounts.</p>
    </section>
  );
}

/* ---------------- Borrow ---------------- */

type Action = "add" | "borrow" | "repay" | "withdraw";
const ACTIONS: [Action, string][] = [
  ["add", "Add collateral"],
  ["borrow", "Borrow"],
  ["repay", "Repay"],
  ["withdraw", "Withdraw"],
];

type HealthState = "healthy" | "close" | "liquidatable";
export function healthState(collateral: bigint, debt: bigint, v: Vault): HealthState {
  return liquidatable(collateral, debt, v) ? "liquidatable" : healthBps(collateral, debt, v.price) > v.collateralFactorBps ? "close" : "healthy";
}
const STATE_LABEL = { healthy: "Healthy", close: "Close to liquidation", liquidatable: "Liquidatable" } as const;

type Reading = { health: number; state: HealthState };
export function HealthBar({ health, state: st, vault, after }: Reading & { vault: Vault; after?: Reading }) {
  const w = (h: number) => `${Math.min(Number.isFinite(h) ? h : 10_000, 10_000) / 100}%`;
  return (
    <div className="health">
      <div className="health-head">
        <span className="instrument-label">Loan-to-value</span>
        <span className="num">
          {pct(health)}
          {after && after.health !== health && (
            <>
              <span className="muted" aria-hidden="true"> → </span>
              <span className="sr-only"> after: </span>
              <span className={after.state === "healthy" ? "" : "bad"}>{pct(after.health)}</span>
            </>
          )}
        </span>
      </div>
      <div
        className={`health-bar health-${st}`}
        role="meter"
        aria-label="Loan-to-value"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.min(Number.isFinite(health) ? health : 10_000, 10_000) / 100}
        aria-valuetext={`${pct(health)}, ${STATE_LABEL[st].toLowerCase()}`}
      >
        <span className="health-fill" style={{ width: w(health) }} />
        {after && after.health !== health && <span className="health-after" style={{ left: w(after.health) }} />}
        <span className="health-mark" style={{ left: w(vault.collateralFactorBps) }} />
        <span className="health-mark health-mark-lt" style={{ left: w(vault.liquidationThresholdBps) }} />
      </div>
      <div className="health-scale muted" aria-hidden="true">
        <span>{pct(vault.collateralFactorBps, 0)} borrow limit</span>
        <span className="lt">{pct(vault.liquidationThresholdBps, 0)} liquidation</span>
      </div>
    </div>
  );
}

function borrowForm(p: LendProps, vault: Vault, pool: Pool, action: Action, text: string, shown: boolean, now: number | null) {
  const position = p.account?.positions.find((x) => x.vault === vault.id) ?? null;
  // The latest full liquidation in this vault, shown while there is no position.
  const emptied = (p.account?.emptied ?? []).filter((x) => x.vault === vault.id && x.lastLiquidation).sort((a, b) => b.lastLiquidation!.at - a.lastLiquidation!.at)[0];
  const c = position?.collateral ?? 0n;
  const d = position ? grown(position.debt, pool.borrowAprBps, p.account!.asOf, now) : 0n;
  const act: Action = position ? action : "add";
  const asset = act === "add" || act === "withdraw" ? vault.collateral : vault.debt;
  const cf = vault.collateralFactorBps;
  const sym = (a: Asset, v: bigint) => `${formatUnits(v, a.decimals)} ${a.symbol}`;
  const max = borrowLimits(vault, c, d, p.balances);
  const limits: Record<Action, { max: bigint | null; tooMuch: string; min?: { value: bigint; text: string } }> = {
    add: {
      max: max.add,
      tooMuch: `One deposit spends one private note, and your largest is ${sym(vault.collateral, max.add ?? 0n)}.`,
      min: position ? undefined : { value: vault.minCollateral, text: `A new position needs at least ${sym(vault.collateral, vault.minCollateral)}.` },
    },
    borrow: {
      max: max.borrow,
      tooMuch: `That would take you past ${pct(cf, 0)}. You can borrow up to ${sym(vault.debt, max.borrow)} now.`,
      min: d === 0n ? { value: vault.minDebt, text: `A loan starts at ${sym(vault.debt, vault.minDebt)}.` } : undefined,
    },
    repay: {
      max: max.repay,
      tooMuch: `One repayment spends one private note, and your largest is ${sym(vault.debt, max.repay ?? 0n)}.`,
    },
    withdraw: {
      max: max.withdraw,
      tooMuch: `That would take you past ${pct(cf, 0)}. You can withdraw up to ${sym(vault.collateral, max.withdraw)} now.`,
    },
  };
  const lim = limits[act];
  const f = check(text, asset, shown, lim.max, lim.tooMuch, lim.min);
  // The preview uses the typed amount even before it is valid, so the bar moves as the person types.
  const amt = f.parsed ?? 0n;
  const c2 = act === "add" ? c + amt : act === "withdraw" ? (amt > c ? 0n : c - amt) : c;
  const d2 = act === "borrow" ? d + amt : act === "repay" ? (amt > d ? 0n : d - amt) : d;
  const h = healthBps(c, d, vault.price);
  const h2 = healthBps(c2, d2, vault.price);
  return { position, emptied, c, d, act, asset, cf, lim, f, amt, c2, d2, h, h2, st: healthState(c, d, vault), st2: healthState(c2, d2, vault) };
}

export function BorrowTab(p: DeskProps) {
  const vault = p.market?.vaults[0];
  const pool = p.market?.pools.find((x) => x.id === vault?.pool);
  const [action, setAction] = useState<Action>("add");
  const [text, setText] = useState("");
  const [shown, setShown] = useState(false);
  const op = useOperation(p);
  const now = useChainNow(p.chainOffset);
  const submitted = useRef<Pick<DeskModel, "fraction" | "ltv"> | null>(null);

  const form = vault && pool ? borrowForm(p, vault, pool, action, text, shown, now) : null;
  const input =
    form && vault
      ? { parsed: form.f.parsed, ok: form.f.ok, limit: form.lim.max, action: form.act, collateral: vault.collateral, health: form.h, projected: form.h2 }
      : null;
  const edit = borrowModel(input && { ...input, run: null, live: null, submitted: null });
  if (!op.run) submitted.current = { fraction: edit.fraction, ltv: edit.ltv };
  const model = borrowModel(input && { ...input, run: op.run, live: op.live, submitted: submitted.current });
  const ltvText = (h: number, debt: bigint) => (debt === 0n ? "No debt" : pct(h));
  let label = "Loan-to-value";
  const unread = !p.account || p.account.needsSession;
  let value = form && (form.position || !unread) ? (form.position ? ltvText(form.h, form.d) : "No position") : "—";
  if (form && op.run && model.phase === "review") [label, value] = [kindLabel(op.run, vault?.collateral), amountsText(op.run.amounts)];
  else if (form && !op.run && model.valid) [label, value] = ["Loan-to-value after", ltvText(form.h2, form.d2)];
  useReport("borrow", { model, label, value }, p.report);

  if (!vault || !pool || !form)
    return (
      <DeskLayout p={p} title="Borrow" rail={null} railLabel="Balances">
        <p className="muted">Reading the vault…</p>
      </DeskLayout>
    );

  const { position, emptied, c, d, act, asset, cf, lim, f, amt, c2, d2, h, h2, st, st2 } = form;
  const lp = liquidationPrice(c, d, vault);
  const lp2 = liquidationPrice(c2, d2, vault);
  // Repaying "all": the debt plus two minutes of interest; anything above the debt comes back as change.
  // Interest keeps accruing until the keeper's transaction lands; the overpayment comes back as change.
  const repayAll = grow(d, pool.borrowAprBps, 120) + d / 100n + 1n;

  const submit = () => {
    setShown(true);
    if (!f.ok || f.parsed === null) return;
    const amount = f.parsed;
    if (act === "add") return op.start("deposit", [{ asset, amount }], (on) => p.lend.deposit(vault.id, position?.id ?? null, amount, on));
    if (act === "repay") return op.start("repay", [{ asset, amount }], (on) => p.lend.repay(position!.id, amount, on));
    if (act === "borrow") return op.start("draw", [{ asset, amount }], (on) => p.lend.draw(position!.id, amount, 0n, on));
    return op.start("draw", [{ asset, amount }], (on) => p.lend.draw(position!.id, 0n, amount, on));
  };
  const close = () =>
    op.start("close", [{ asset: vault.collateral, amount: c }], (on) => p.lend.closePosition(position!.id, on));

  const BUTTON: Record<Action, string> = { add: position ? "Add collateral" : "Open a position", borrow: "Borrow", repay: "Repay", withdraw: "Withdraw collateral" };
  const SAYS: Record<Action, string> = {
    add: "Your browser proves the deposit and your wallet signs it; the keeper then adds it to your position. If it isn't added before the request expires, you take it back in one click.",
    borrow: `The keeper builds the loan and your wallet signs it. The ${vault.debt.symbol} is paid into your private balance in that one transaction.`,
    repay: "Your browser proves the repayment and your wallet signs it; the keeper then applies it. Anything above your debt comes back as change. If it isn't applied before the request expires, you take it back in one click.",
    withdraw: `The keeper builds the withdrawal and your wallet signs it. The ${vault.collateral.symbol} is paid into your private balance in that one transaction.`,
  };

  const rail = (
    <>
      <section className="rail-panel vault" aria-label="Vault">
        <div className="rail-head">
          <h2>
            {vault.collateral.symbol} → {vault.debt.symbol} vault
          </h2>
          <span className="rail-tag num">
            1 {vault.collateral.symbol} = {priceText(vault.price, vault)} {vault.debt.symbol}
          </span>
        </div>
        <dl className="readings">
          <div>
            <dt>Borrow up to</dt>
            <dd className="num">{pct(cf, 0)}</dd>
          </div>
          <div>
            <dt>Liquidation</dt>
            <dd className="num">{pct(vault.liquidationThresholdBps, 0)}</dd>
          </div>
          <div>
            <dt>Penalty</dt>
            <dd className="num">{pct(vault.penaltyBps, 0)}</dd>
          </div>
          <div>
            <dt>Borrow APR</dt>
            <dd className="num">{pct(pool.borrowAprBps, 2)}</dd>
          </div>
        </dl>
        <p className="rail-foot">
          The borrow limit is {pct(cf, 0)} of collateral value at the draw price: the oracle price less its confidence and a 0.5% margin.
        </p>
      </section>
      {position ? (
        <section className={`rail-panel position position-${st}`} aria-label="Your position">
          <div className="rail-head">
            <h2>Your position</h2>
            <span className={`badge badge-${st}`}>{STATE_LABEL[st]}</span>
          </div>
          <dl className="readings">
            <div>
              <dt>Collateral</dt>
              <dd>
                <Amount value={c} asset={vault.collateral} />
              </dd>
            </div>
            <div>
              <dt>Debt</dt>
              <dd>
                <Amount value={d} asset={vault.debt} />
              </dd>
            </div>
            <div>
              <dt>Liquidation price</dt>
              <dd className="num">{lp === null ? "None" : `${priceText(lp, vault)} ${vault.debt.symbol}`}</dd>
            </div>
            <div>
              <dt>You can still borrow</dt>
              <dd>
                <Amount value={maxBorrow(c, d, drawPrice(vault), cf)} asset={vault.debt} />
              </dd>
            </div>
          </dl>
          <HealthBar health={h} state={st} vault={vault} after={shown || amt > 0n ? { health: h2, state: st2 } : undefined} />
        </section>
      ) : (
        <section className="rail-panel mine" aria-label="Your position">
          <div className="rail-head">
            <h2>Your position</h2>
          </div>
          <p className="rail-foot">
            No position yet. Add {vault.collateral.symbol} as collateral, then borrow up to {pct(cf, 0)} of its value, with no term.
          </p>
        </section>
      )}
    </>
  );

  return (
    <DeskLayout p={p} title={op.run ? kindLabel(op.run, vault.collateral) : "Manage position"} rail={rail} railLabel="Vault, position and balances">
      {position && st === "close" && !position.lastLiquidation && (
        <p className="note note-warn" role="status">
          Above {pct(cf, 0)} you can't borrow more. Liquidation starts above {pct(vault.liquidationThresholdBps, 0)}, at a{" "}
          {vault.collateral.symbol} price of {lp === null ? "" : priceText(lp, vault)}. Repaying or adding collateral moves you back.
        </p>
      )}
      {position && st === "liquidatable" && (
        <p className="note note-error" role="status">
          <strong>Past {pct(vault.liquidationThresholdBps, 0)}.</strong> The keeper sells only enough collateral to bring the position back
          to {pct(vault.liquidationThresholdBps, 0)} (past {pct(vault.liquidationMaxLimitBps, 0)}, to repay the whole loan), plus the{" "}
          {pct(vault.penaltyBps, 0)} penalty. Repay or add collateral to keep more of it.
        </p>
      )}
      {position?.lastLiquidation && <LiquidatedNote position={position} vault={vault} now={now} />}
      {!position && emptied && <EmptiedNote position={emptied} vault={vault} now={now} />}
      {op.run ? (
        <Progress
          p={p}
          op={op}
          onDone={() => {
            op.clear();
            setText("");
            setShown(false);
          }}
        />
      ) : (
        <>
          <form
            className="ticket-flow"
            noValidate
            onSubmit={(e) => {
              e.preventDefault();
              submit();
            }}
          >
            <div className="mode-picker" role="group" aria-label="Position action">
              {ACTIONS.map(([k, label]) => (
                <button
                  key={k}
                  type="button"
                  aria-pressed={act === k}
                  disabled={!position && k !== "add"}
                  onClick={() => {
                    setAction(k);
                    setText("");
                    setShown(false);
                  }}
                >
                  {k === "add" ? "Add" : label}
                </button>
              ))}
            </div>
            <Field
              id="borrow-amt"
              label={BUTTON[act]}
              asset={asset}
              text={text}
              setText={setText}
              invalid={f.invalid}
              max={act === "repay" ? (lim.max !== null && lim.max < repayAll ? lim.max : repayAll) : lim.max}
              maxLabel={act === "repay" ? "Repay all" : undefined}
              hint={
                lim.max === null ? (
                  "Reading your private balance…"
                ) : act === "add" ? (
                  <>
                    Largest private note <Amount value={lim.max} asset={asset} />
                  </>
                ) : act === "repay" ? (
                  <>
                    Debt <Amount value={d} asset={asset} />
                  </>
                ) : lim.max === 0n ? (
                  `Nothing more while the position is at or above ${pct(cf, 0)}.`
                ) : (
                  <>
                    Up to <Amount value={lim.max} asset={asset} />
                  </>
                )
              }
            />
            {(amt > 0n || act === "add") && (
              <dl className="form-summary kv-preview" aria-live="polite">
                <div>
                  <dt>Loan-to-value after</dt>
                  <dd className={`num${st2 === "healthy" ? "" : " bad"}`}>{d2 === 0n ? "No debt" : pct(h2)}</dd>
                </div>
                <div>
                  <dt>Liquidation price after</dt>
                  <dd className="num">{lp2 === null ? "None" : `${priceText(lp2, vault)} ${vault.debt.symbol}`}</dd>
                </div>
              </dl>
            )}
            {act === "add" && lim.max === 0n ? (
              <NoPrivate asset={vault.collateral} then="add it here as collateral" toBalances={p.toBalances} />
            ) : act === "repay" && lim.max === 0n ? (
              <NoPrivate asset={vault.debt} then="repay from it here" toBalances={p.toBalances} />
            ) : (
              <button className="button button-primary main-action" type="submit">
                {BUTTON[act]}
              </button>
            )}
            <p className="ticket-foot">{SAYS[act]}</p>
          </form>
          {position && d === 0n && c > 0n && (
            <div className="faucet-box">
              <button className="button button-secondary" onClick={close}>
                Close position and take back {formatUnits(c, vault.collateral.decimals)} {vault.collateral.symbol}
              </button>
              <p className="ticket-foot close-note">With no debt, your browser proves the close and your wallet signs it. No keeper needed.</p>
            </div>
          )}
        </>
      )}
      <ErrorNote text={op.formError} />
      {import.meta.env.VITE_KEELER_MOCK === "1" && "mock" in p.lend && (
        <MockControls controls={(p.lend as KeelerLend & { mock: MockLendControls }).mock} refresh={p.refreshLend} />
      )}
    </DeskLayout>
  );
}

const agoText = (at: number, now: number | null) => {
  const ago = now === null ? null : Math.max(0, Math.round(now - at));
  return ago === null ? "" : ago < 60 ? `${ago} s ago` : ago < 3600 ? `${Math.floor(ago / 60)} min ago` : new Date(at * 1000).toLocaleString();
};

/** What the last liquidation did, from what it left: a repayment made since is not credited to it. */
export function LiquidatedNote({ position, vault, now }: { position: Position; vault: Vault; now: number | null }) {
  const l = position.lastLiquidation!;
  return (
    <div className="note note-error liquidated" role="status">
      <strong>Liquidated {agoText(l.at, now)}.</strong> <Amount value={l.debtRepaid} asset={vault.debt} /> of debt was repaid with{" "}
      <Amount value={l.collateralSold} asset={vault.collateral} />, the {pct(vault.penaltyBps, 0)} penalty included, at{" "}
      {priceText(l.price, vault)} {vault.debt.symbol}.{" "}
      {l.debtLeft === 0n
        ? "It repaid the whole loan; the rest of your collateral stayed yours."
        : l.collateralLeft === 0n
          ? "It left no collateral; the remaining debt stays on the position until you repay it."
          : `It brought the position back to ${pct(vault.liquidationThresholdBps, 0)} or less, and the rest stayed yours.`}
      <OrbLinks links={liquidationLinks(l.signature)} says={ON_CHAIN.liquidation} />
    </div>
  );
}

/** A full liquidation that left no collateral and no debt: the position is gone, and this says why. */
export function EmptiedNote({ position, vault, now }: { position: Position; vault: Vault; now: number | null }) {
  const l = position.lastLiquidation!;
  return (
    <div className="note note-error liquidated" role="status">
      <strong>Your position was liquidated in full {agoText(l.at, now)}.</strong> <Amount value={l.debtRepaid} asset={vault.debt} /> of debt
      was repaid with all of its collateral, <Amount value={l.collateralSold} asset={vault.collateral} />, at {priceText(l.price, vault)}{" "}
      {vault.debt.symbol}. Nothing is left in it: no collateral and no debt. You can open a new position.
      <OrbLinks links={liquidationLinks(l.signature)} says={ON_CHAIN.liquidation} />
    </div>
  );
}

/** Test controls of the mock client. Compiled out of a build without VITE_KEELER_MOCK. */
function MockControls({ controls, refresh }: { controls: MockLendControls; refresh: () => Promise<unknown> }) {
  const go = (f: () => void) => {
    f();
    refresh().catch(() => {});
    setTimeout(() => refresh().catch(() => {}), 4600);
  };
  return (
    <div className="mock" role="group" aria-label="Mock controls">
      <span className="muted small">Mock price</span>
      <button type="button" className="button button-secondary button-small" onClick={() => go(() => controls.movePrice(-800))}>
        −8%
      </button>
      <button type="button" className="button button-secondary button-small" onClick={() => go(() => controls.crash())}>
        Crash to liquidation
      </button>
      <button type="button" className="button button-secondary button-small" onClick={() => go(() => controls.resetPrice())}>
        Reset
      </button>
    </div>
  );
}

/* ---------------- Activity ---------------- */

const STATUS_LABEL: Record<LendOperation["status"], string> = {
  pending: "In progress",
  landed: "Landed",
  refundable: "Not taken in",
  refunded: "Funds taken back",
  failed: "Failed",
};

export function LendActivity(p: LendProps) {
  const now = useChainNow(p.chainOffset);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const vault = p.market?.vaults[0];
  const ops = p.account?.operations ?? [];
  const liquidations = [...(p.account?.positions ?? []), ...(p.account?.emptied ?? [])].filter((x) => x.lastLiquidation);

  const refund = async (id: string) => {
    setBusy(id);
    setError(null);
    try {
      await p.lend.refundIntent(id);
      await Promise.all([p.refreshLend(), p.refreshBalances()]);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <section className="activity" aria-labelledby="lending-title">
      <div className="panel-header">
        <h2 id="lending-title">Your lending</h2>
      </div>
      {p.account?.needsSession && (
        <p className="note">
          The keeper may have news for your lending (a fill, or a change made in another browser).{" "}
          <button
            className="button button-secondary button-small"
            disabled={busy === "session"}
            onClick={() => {
              setBusy("session");
              setError(null);
              p.refreshLend(true)
                .catch((e) => setError(errorText(e)))
                .finally(() => setBusy(null));
            }}
          >
            Check with the keeper
          </button>{" "}
          Your wallet asks once to allow this lending session.
        </p>
      )}
      {ops.length === 0 && liquidations.length === 0 ? (
        <p className="activity-empty">
          Nothing yet. Deposit in <a href="#earn">Earn</a> or open a position in <a href="#borrow">Borrow</a>.
        </p>
      ) : (
        <ul className="orders lend-ops">
          {vault &&
            liquidations.map((x) => (
              <li key={`liq-${x.id}`} className="order order-failed">
                <span className="order-pair">
                  Liquidated: <Amount value={x.lastLiquidation!.debtRepaid} asset={vault.debt} /> repaid with{" "}
                  <Amount value={x.lastLiquidation!.collateralSold} asset={vault.collateral} />
                </span>
                <span className="order-status">
                  <span className="badge">Liquidation</span>
                </span>
                <OrbLinks links={liquidationLinks(x.lastLiquidation!.signature)} />
              </li>
            ))}
          {ops.map((o) => {
            const left = o.expiry === null || now === null ? null : o.expiry - now;
            const flow = FLOW[o.kind];
            const at = o.status === "landed" ? flow.length : flow.indexOf(o.step);
            return (
              <li key={o.id} className={`order order-${o.status === "landed" ? "settled" : o.status}`}>
                <span className="order-pair">
                  {kindLabel(o, vault?.collateral)}{" "}
                  {o.amounts.map((a, i) => (
                    <span key={i}>
                      {i > 0 && " and "}
                      <Amount value={a.amount} asset={a.asset} />
                    </span>
                  ))}
                </span>
                <span className="order-status">
                  {o.status === "pending" && (
                    <span className="chain-steps">
                      <span className="sr-only">{STEP_LABEL[o.step]}</span>
                      {flow.map((s, i) => (
                        <span key={s} className={i < at ? "cs-done" : i === at ? "cs-now" : ""} aria-hidden="true">
                          {STEP_SHORT[s]}
                        </span>
                      ))}
                    </span>
                  )}
                  <span className="badge">{STATUS_LABEL[o.status]}</span>
                  {o.status === "pending" && o.step === "keeper" && left !== null && (
                    <span className="muted num">{left > 0 ? `refund possible in ${countdown(left)}` : "checking…"}</span>
                  )}
                  {o.error && <span className="bad">{o.error}</span>}
                </span>
                {o.status === "refundable" && (
                  <button className="button button-primary button-small" onClick={() => refund(o.id)} disabled={!!busy} aria-busy={busy === o.id}>
                    {busy === o.id ? "Taking funds back…" : "Take funds back"}
                  </button>
                )}
                <OrbLinks links={opLinks(o)} />
              </li>
            );
          })}
        </ul>
      )}
      <OnChainNotes
        rows={[
          ...liquidations.map((x) => ({ links: liquidationLinks(x.lastLiquidation!.signature), says: ON_CHAIN.liquidation })),
          ...ops.map((o) => ({ links: opLinks(o), says: opSays(o) })),
        ]}
      />
      <ErrorNote text={error} />
    </section>
  );
}
