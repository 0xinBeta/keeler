// Earn and Borrow in the panel, and the lending activity list below the hero. Copy rules: docs/plan-landing.md and
// the brief in docs/plan-lend.md; a sentence that says what a money-moving button does stays next to that button.
import { useState } from "react";
import type { Asset, Balance, KeelerLend, LendAccount, LendKind, LendOperation, LendStep, Pool, Position, Vault } from "../keeler/index";
import type { MockLendControls } from "../keeler/mock-lend";
import { countdown, formatUnits, parseUnits } from "./format";
import { drawPrice, grow, healthBps, liquidatable, liquidationPrice, maxBorrow, maxWithdraw, pct, priceText } from "./lend-view";
import { Amount, ErrorNote, errorText, useChainNow } from "./shared";

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
const STEP_SHORT:Record<LendStep, string> = { proving: "Proving", signing: "Signing", keeper: "Keeper", landed: "Landed" };

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

const largest = (balances: Balance[] | null, asset: Asset) => balances?.find((b) => b.asset.mint === asset.mint)?.largestNote ?? null;
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
    <div className="stack">
      <p className="muted">
        {kindLabel(run)}: {what}
      </p>
      <OpSteps kind={run.kind} step={step} status={status} failed={!!run.failed} />
      {status === "pending" && step === "keeper" && left !== null && (
        <p className="muted small num">{left > 0 ? `If it isn't taken in, you can take it back in ${countdown(left)}.` : "Request expired, checking…"}</p>
      )}
      <p className={`status${run.failed || status === "failed" ? " bad" : ""}`} aria-live="polite">
        {line}
      </p>
      {run.failed && (
        <p className="note note-error" role="alert">
          {run.failed}
        </p>
      )}
      {status === "refundable" && (
        <button className="btn btn-primary" onClick={refund} disabled={busy} aria-busy={busy}>
          {busy ? "Taking funds back…" : "Take funds back"}
        </button>
      )}
      {(run.failed || status === "landed" || status === "refunded" || status === "failed") && (
        <button className="btn btn-primary" onClick={onDone}>
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
    <div className="field">
      <label htmlFor={id}>{props.label}</label>
      <div className="combo">
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
        <span className="combo-sym">{asset.symbol}</span>
      </div>
      <div className="hint" id={`${id}-hint`}>
        {props.invalid ? <span className="bad">{props.invalid}</span> : <span>{props.hint}</span>}
        {props.max !== null && props.max > 0n && (
          <button type="button" className="link" onClick={() => props.setText(plain(props.max!, asset))}>
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

/* ---------------- Earn ---------------- */

export function EarnTab(p: LendProps) {
  const pool = p.market?.pools[0];
  const [chosen, setMode] = useState<"in" | "out">("in");
  const [text, setText] = useState("");
  const [shown, setShown] = useState(false);
  const op = useOperation(p);
  const now = useChainNow(p.chainOffset);

  if (!pool) return <p className="muted">Reading the pool…</p>;
  if (op.run)
    return (
      <Progress
        p={p}
        op={op}
        onDone={() => {
          op.clear();
          setText("");
          setShown(false);
        }}
      />
    );

  const a = pool.asset;
  // Every receipt in this pool counts towards the deposit; one withdrawal spends one receipt, so the largest caps it.
  const mine = p.account?.receipts.filter((r) => r.pool === pool.id && r.shares > 0n) ?? [];
  const receipt = mine.length ? { value: mine.reduce((t, r) => t + r.value, 0n) } : undefined;
  const largestReceipt = mine.reduce((m, r) => (r.value > m ? r.value : m), 0n);
  // With nothing left to withdraw the form goes back to depositing.
  const mode = receipt ? chosen : "in";
  const value = receipt ? grown(receipt.value, pool.supplyAprBps, p.account!.asOf, now) : 0n;
  const note = largest(p.balances, a);
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

  const submit = () => {
    setShown(true);
    if (!f.ok || f.parsed === null) return;
    const amount = f.parsed;
    op.start(mode === "in" ? "supply" : "redeem", [{ asset: a, amount }], (on) =>
      // The whole deposit's worth withdraws every share, so no dust stays in the receipt.
      mode === "in" ? p.lend.supply(pool.id, amount, on) : p.lend.redeem(pool.id, amount === max ? "all" : amount, on),
    );
  };

  return (
    <div className="stack">
      <PoolCard pool={pool} />
      <div className="lend-card mine">
        <span className="muted small">Your deposit</span>
        {receipt ? (
          <>
            <span className="mine-big num" aria-live="off">
              <Amount value={value} asset={a} />
            </span>
            <span className="muted small">
              Growing at <span className="good">{pct(pool.supplyAprBps, 2)}</span> a year. Only you and the keeper can see it.
            </span>
          </>
        ) : (
          <span className="muted">Nothing deposited yet. Your first deposit starts earning as soon as the keeper takes it in.</span>
        )}
      </div>
      <form
        className="stack"
        noValidate
        onSubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        <div className="seg seg-small" role="group" aria-label="Earn action">
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
        {mode === "in" && note === 0n ? (
          <p className="empty">You have no private {a.symbol} yet. Move some in from Balances to start earning.</p>
        ) : (
          <button className="btn btn-primary" type="submit">
            {mode === "in" ? "Deposit and earn" : "Withdraw"}
          </button>
        )}
        <p className="muted small">
          {mode === "in"
            ? "Your browser proves the deposit and your wallet signs it; the keeper then adds it to the pool. If it isn't added before the request expires, you take it back in one click."
            : "The keeper builds the withdrawal and your wallet signs it. It is paid into your private balance in that one transaction."}
        </p>
      </form>
      <ErrorNote text={op.formError} />
    </div>
  );
}

export function PoolCard({ pool }: { pool: Pool }) {
  return (
    <div className="lend-card pool">
      <div className="lend-card-head">
        <span>
          <span className="pool-name">{pool.asset.symbol} pool</span>
          <span className="muted small"> · floating rate</span>
        </span>
        <span className="tag tag-private">Private amounts</span>
      </div>
      <div className="apr">
        <span className="apr-big num">{pct(pool.supplyAprBps, 2)}</span>
        <span className="muted">supply APR</span>
      </div>
      <dl className="kv">
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
      </dl>
      <div className="util" role="meter" aria-label="Utilisation" aria-valuemin={0} aria-valuemax={100} aria-valuenow={pool.utilisationBps / 100}>
        <span style={{ width: `${Math.min(pool.utilisationBps, 10_000) / 100}%` }} />
      </div>
    </div>
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

export function healthState(h: number, v: Vault): "healthy" | "close" | "liquidatable" {
  return liquidatable(h, v) ? "liquidatable" : h > v.collateralFactorBps ? "close" : "healthy";
}
const STATE_LABEL = { healthy: "Healthy", close: "Close to liquidation", liquidatable: "Liquidatable" } as const;

export function HealthBar({ health, vault, after }: { health: number; vault: Vault; after?: number }) {
  const w = (h: number) => `${Math.min(Number.isFinite(h) ? h : 10_000, 10_000) / 100}%`;
  const st = healthState(health, vault);
  return (
    <div className="health">
      <div className="health-head">
        <span className="muted small">Loan-to-value</span>
        <span className="num">
          {pct(health)}
          {after !== undefined && after !== health && (
            <>
              <span className="muted" aria-hidden="true"> → </span>
              <span className="sr-only"> after: </span>
              <span className={healthState(after, vault) === "healthy" ? "" : "bad"}>{pct(after)}</span>
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
        {after !== undefined && after !== health && <span className="health-after" style={{ left: w(after) }} />}
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

export function BorrowTab(p: LendProps) {
  const vault = p.market?.vaults[0];
  const pool = p.market?.pools.find((x) => x.id === vault?.pool);
  const [action, setAction] = useState<Action>("add");
  const [text, setText] = useState("");
  const [shown, setShown] = useState(false);
  const op = useOperation(p);
  const now = useChainNow(p.chainOffset);

  if (!vault || !pool) return <p className="muted">Reading the vault…</p>;
  const position = p.account?.positions.find((x) => x.vault === vault.id) ?? null;
  // The latest full liquidation in this vault, shown while there is no position.
  const emptied = (p.account?.emptied ?? []).filter((x) => x.vault === vault.id && x.lastLiquidation).sort((a, b) => b.lastLiquidation!.at - a.lastLiquidation!.at)[0];
  if (op.run)
    return (
      <Progress
        p={p}
        op={op}
        onDone={() => {
          op.clear();
          setText("");
          setShown(false);
        }}
      />
    );

  const c = position?.collateral ?? 0n;
  const d = position ? grown(position.debt, pool.borrowAprBps, p.account!.asOf, now) : 0n;
  const act: Action = position ? action : "add";
  const asset = act === "add" || act === "withdraw" ? vault.collateral : vault.debt;
  const cf = vault.collateralFactorBps;
  const sym = (a: Asset, v: bigint) => `${formatUnits(v, a.decimals)} ${a.symbol}`;
  const limits: Record<Action, { max: bigint | null; tooMuch: string; min?: { value: bigint; text: string } }> = {
    add: {
      max: largest(p.balances, vault.collateral),
      tooMuch: `One deposit spends one private note, and your largest is ${sym(vault.collateral, largest(p.balances, vault.collateral) ?? 0n)}.`,
      min: position ? undefined : { value: vault.minCollateral, text: `A new position needs at least ${sym(vault.collateral, vault.minCollateral)}.` },
    },
    borrow: {
      max: maxBorrow(c, d, drawPrice(vault), cf),
      tooMuch: `That would take you past ${pct(cf, 0)}. You can borrow up to ${sym(vault.debt, maxBorrow(c, d, drawPrice(vault), cf))} now.`,
      min: d === 0n ? { value: vault.minDebt, text: `A loan starts at ${sym(vault.debt, vault.minDebt)}.` } : undefined,
    },
    repay: {
      max: largest(p.balances, vault.debt),
      tooMuch: `One repayment spends one private note, and your largest is ${sym(vault.debt, largest(p.balances, vault.debt) ?? 0n)}.`,
    },
    withdraw: {
      max: maxWithdraw(c, d, drawPrice(vault), cf),
      tooMuch: `That would take you past ${pct(cf, 0)}. You can withdraw up to ${sym(vault.collateral, maxWithdraw(c, d, drawPrice(vault), cf))} now.`,
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
  const lp = liquidationPrice(c, d, vault.liquidationThresholdBps);
  const lp2 = liquidationPrice(c2, d2, vault.liquidationThresholdBps);
  const st = healthState(h, vault);
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

  return (
    <div className="stack">
      <div className="lend-card">
        <div className="lend-card-head">
          <span className="pool-name">
            {vault.collateral.symbol} → {vault.debt.symbol} vault
          </span>
          <span className="muted small num">
            1 {vault.collateral.symbol} = {priceText(vault.price, vault)} {vault.debt.symbol}
          </span>
        </div>
        <dl className="kv kv-4">
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
      </div>

      {position ? (
        <div className={`lend-card position position-${st}`}>
          <div className="lend-card-head">
            <span className="pool-name">Your position</span>
            <span className={`badge badge-${st}`}>{STATE_LABEL[st]}</span>
          </div>
          <dl className="kv kv-2">
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
          <HealthBar health={h} vault={vault} after={shown || amt > 0n ? h2 : undefined} />
          {st === "close" && !position.lastLiquidation && (
            <p className="note note-warn" role="status">
              Above {pct(cf, 0)} you can't borrow more. Liquidation starts above {pct(vault.liquidationThresholdBps, 0)}, at a{" "}
              {vault.collateral.symbol} price of {lp === null ? "" : priceText(lp, vault)}. Repaying or adding collateral moves you back.
            </p>
          )}
          {st === "liquidatable" && (
            <p className="note note-error" role="status">
              <strong>Past {pct(vault.liquidationThresholdBps, 0)}.</strong> The keeper sells only enough collateral to bring the position back
              to {pct(vault.liquidationThresholdBps, 0)} (past {pct(vault.liquidationMaxLimitBps, 0)}, to repay the whole loan), plus the{" "}
              {pct(vault.penaltyBps, 0)} penalty. Repay or add collateral to keep more of it.
            </p>
          )}
          {position.lastLiquidation && <LiquidatedNote position={position} vault={vault} now={now} />}
        </div>
      ) : (
        <div className="lend-card mine">
          <span className="muted small">Your position</span>
          {emptied && <EmptiedNote position={emptied} vault={vault} now={now} />}
          <span className="muted">
            No position yet. Add {vault.collateral.symbol} as collateral, then borrow up to {pct(cf, 0)} of its value, with no term.
          </span>
        </div>
      )}

      <form
        className="stack"
        noValidate
        onSubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        <div className="seg seg-small seg-4" role="group" aria-label="Position action">
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
          <dl className="kv kv-preview" aria-live="polite">
            <div>
              <dt>Loan-to-value after</dt>
              <dd className={`num${healthState(h2, vault) === "healthy" ? "" : " bad"}`}>{d2 === 0n ? "No debt" : pct(h2)}</dd>
            </div>
            <div>
              <dt>Liquidation price after</dt>
              <dd className="num">{lp2 === null ? "None" : `${priceText(lp2, vault)} ${vault.debt.symbol}`}</dd>
            </div>
          </dl>
        )}
        <button className="btn btn-primary" type="submit">
          {BUTTON[act]}
        </button>
        <p className="muted small">{SAYS[act]}</p>
      </form>
      {position && d === 0n && c > 0n && (
        <div className="faucet">
          <button className="btn btn-ghost" onClick={close}>
            Close position and take back {formatUnits(c, vault.collateral.decimals)} {vault.collateral.symbol}
          </button>
          <p className="muted small close-note">With no debt, your browser proves the close and your wallet signs it. No keeper needed.</p>
        </div>
      )}
      <ErrorNote text={op.formError} />
      {import.meta.env.VITE_KEELER_MOCK === "1" && "mock" in p.lend && (
        <MockControls controls={(p.lend as KeelerLend & { mock: MockLendControls }).mock} refresh={p.refreshLend} />
      )}
    </div>
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
      <button type="button" className="btn btn-ghost btn-small" onClick={() => go(() => controls.movePrice(-800))}>
        −8%
      </button>
      <button type="button" className="btn btn-ghost btn-small" onClick={() => go(() => controls.crash())}>
        Crash to liquidation
      </button>
      <button type="button" className="btn btn-ghost btn-small" onClick={() => go(() => controls.resetPrice())}>
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
    <section className="section section-tight" aria-labelledby="lending-title">
      <div className="section-head">
        <h2 id="lending-title">Your lending</h2>
      </div>
      {p.account?.needsSession && (
        <p className="note">
          The keeper may have news for your lending (a fill, or a change made in another browser).{" "}
          <button
            className="btn btn-ghost"
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
        <p className="muted">Nothing yet. Deposit to Earn or open a position from Earn and Borrow above.</p>
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
                  <button className="btn btn-primary btn-small" onClick={() => refund(o.id)} disabled={!!busy} aria-busy={busy === o.id}>
                    {busy === o.id ? "Taking funds back…" : "Take funds back"}
                  </button>
                )}
              </li>
            );
          })}
        </ul>
      )}
      <ErrorNote text={error} />
    </section>
  );
}
