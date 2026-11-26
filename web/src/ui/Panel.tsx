import { useCallback, useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import type { Wallet } from "@wallet-standard/base";
import { KeelerError } from "../keeler/index";
import type { Asset, Balance, Config, Keeler, LendAccount, Order, Quote, SwapStep } from "../keeler/index";
import type { Route } from "./App";
import { DeskRing, useReport } from "./app/ring";
import type { Report, Scene } from "./app/ring";
import { bps, countdown, formatUnits, parseUnits, short } from "./format";
import { BorrowTab, EarnTab } from "./Lend";
import { OrderLinks } from "./Orders";
import type { Market } from "./Lend";
import { borrowModel, earnModel, swapModel } from "./orbit/model";
import type { DeskTab } from "./orbit/model";
import { Amount, amountText, ErrorNote, errorText, Funds, ON_CHAIN, OrbLinks, RefundNotice, useChainNow } from "./shared";

type Props = {
  keeler: Keeler;
  config: Config;
  balances: Balance[] | null;
  orders: Order[];
  chainOffset: number | null;
  onReady: () => void;
  refreshBalances: () => Promise<unknown>;
  refreshOrders: () => Promise<unknown>;
  reloadConfig: () => void;
  tab: Route;
  setTab: (t: Route) => void;
  market: Market | null;
  account: LendAccount | null;
  refreshLend: (prompt?: boolean) => Promise<unknown>;
  address: string | null;
  setAddress: (a: string) => void;
  refundable: number;
  activity: ReactNode;
};

const TITLE: Record<Route, string> = {
  swap: "Swap privately",
  earn: "Earn privately",
  borrow: "Borrow privately",
  balances: "Balances",
  activity: "Activity",
};

type Phase = "wallet" | "unlock" | "register" | "ready";
type View =
  | { kind: "form" }
  | { kind: "quotes"; fair: bigint; priceTime: number; quotes: Quote[]; selected: string | null }
  | { kind: "progress"; quote: Quote; step: SwapStep | null; order: Order | null; failed: string | null };

/** Runs one action at a time, keeping its label for the busy button and its error for the panel. */
function useAction() {
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [code, setCode] = useState<KeelerError["code"] | null>(null);
  const run = async (label: string, fn: () => Promise<void>) => {
    setBusy(label);
    setError(null);
    setCode(null);
    try {
      await fn();
    } catch (e) {
      setError(errorText(e));
      setCode(e instanceof KeelerError ? e.code : null);
    } finally {
      setBusy(null);
    }
  };
  return { busy, error, code, setError, run };
}

const IDLE: Record<DeskTab, Scene> = {
  earn: { model: earnModel(null), label: "", value: "" },
  borrow: { model: borrowModel(null), label: "", value: "" },
  swap: { model: swapModel(null), label: "", value: "" },
};

export function Panel(p: Props) {
  const { keeler, config, address, setAddress } = p;
  const [phase, setPhase] = useState<Phase>("wallet");
  const { setTab } = p;
  const act = useAction();
  const heading = useRef<HTMLHeadingElement>(null);
  const first = useRef(true);
  const [scenes, setScenes] = useState<Partial<Record<DeskTab, Scene>>>({});
  const report = useCallback<Report>((t, s) => setScenes((all) => ({ ...all, [t]: s })), []);

  const lend = keeler.lend;
  const tabs: Route[] = lend ? ["earn", "borrow", "swap", "balances", "activity"] : ["swap", "balances", "activity"];
  const shown: Route = tabs.includes(p.tab) ? p.tab : "swap";
  const ringTab: DeskTab | null = shown === "earn" || shown === "borrow" || shown === "swap" ? shown : null;
  const scene = ringTab ? ((phase === "ready" ? scenes[ringTab] : undefined) ?? IDLE[ringTab]) : IDLE.swap;

  // Move focus to the new heading when the panel changes screen, so keyboard and screen reader users follow.
  useEffect(() => {
    if (first.current) {
      first.current = false;
      return;
    }
    heading.current?.focus();
  }, [phase, shown]);

  const connect = (w: Wallet) =>
    act.run(w.name, async () => {
      const { address } = await keeler.connect(w);
      setAddress(address);
      setPhase("unlock");
    });
  const unlock = () =>
    act.run("unlock", async () => {
      await keeler.unlock();
      if (await keeler.isRegistered()) {
        setPhase("ready");
        p.onReady();
      } else setPhase("register");
    });
  const [registration, setRegistration] = useState<string | undefined>();
  const register = () =>
    act.run("register", async () => {
      setRegistration(await keeler.register());
      setPhase("ready");
      p.onReady();
    });
  // A new wallet has no SOL for the registration fee; on a test network the faucet sends some.
  const [funded, setFunded] = useState(false);
  const faucet = () =>
    act.run("faucet", async () => {
      await keeler.faucet();
      setFunded(true);
    });

  const pool = p.market?.pools[0];
  const vault = p.market?.vaults[0];
  const DESC: Record<Route, string> = {
    swap: "Get signed quotes from the listed dealers and settle to your private balance.",
    earn: pool ? `Deposit private ${pool.asset.symbol} into the lending pool and earn interest.` : "Deposit into the lending pool and earn interest.",
    borrow: vault ? `Borrow against your ${vault.collateral.symbol}.` : "Borrow against your collateral.",
    balances: "Move tokens between your public and private balances.",
    activity: lend ? "Your swap orders and lending operations." : "Your swap orders.",
  };

  let gate: ReactNode = null;
  if (phase === "wallet") {
    const wallets = keeler.wallets();
    gate = (
      <>
        <p className="lede-small">Your wallet signs; your private keys are created in this browser and Keeler's servers never see them.</p>
        {wallets.length === 0 ? (
          <p className="note">No Solana wallet was found in this browser. Install one, such as Phantom, then reload the page.</p>
        ) : (
          <ul className="wallets">
            {wallets.map((w) => (
              <li key={w.name}>
                <button className="wallet-option" onClick={() => connect(w)} disabled={!!act.busy} aria-busy={act.busy === w.name}>
                  <img src={w.icon} alt="" width="28" height="28" />
                  <span>{w.name}</span>
                  <span className="wallet-go">{act.busy === w.name ? "Connecting…" : "Connect"}</span>
                </button>
                {/* shortcut: recognised by name; importing the client's constant would pull the client into the first bundle. */}
                {/browser test wallet/i.test(w.name) && (
                  <p className="wallet-note">A test key kept in this browser. Good for trying Keeler; clearing site data removes it.</p>
                )}
              </li>
            ))}
          </ul>
        )}
        <ErrorNote text={act.error} />
      </>
    );
  } else if (phase === "unlock" || phase === "register") {
    const unlocking = phase === "unlock";
    gate = (
      <>
        <p className="connected">
          Connected <span className="mono">{short(address ?? "")}</span>
        </p>
        <p className="lede-small">
          {unlocking
            ? "This signature creates your private keys. It does not move funds."
            : "Registering adds your private-note keys to the public Rings registry, so dealers can pay you privately. It is one transaction and does not move funds."}
        </p>
        <button className="button button-primary main-action" onClick={unlocking ? unlock : register} disabled={!!act.busy} aria-busy={!!act.busy}>
          {act.busy === "unlock" ? "Waiting for your wallet…" : act.busy === "register" ? "Registering…" : unlocking ? "Sign to unlock" : "Register"}
        </button>
        {!unlocking && config.faucet && (
          <div className="faucet-box">
            <p className="ticket-foot">Registering costs a small network fee in SOL. On this test network you can get some here.</p>
            <button className="button button-secondary" onClick={faucet} disabled={!!act.busy} aria-busy={act.busy === "faucet"}>
              {act.busy === "faucet" ? "Sending test tokens…" : "Get test tokens"}
            </button>
          </div>
        )}
        <p className="status" aria-live="polite">
          {funded ? "Test tokens sent to your public balance." : ""}
        </p>
        <ErrorNote text={act.error} />
      </>
    );
  }
  const gateTitle = phase === "wallet" ? "Choose a wallet" : phase === "unlock" ? "Unlock your private balance" : "Register this wallet";

  const lendProps = {
    balances: p.balances,
    market: p.market,
    account: p.account,
    chainOffset: p.chainOffset,
    refreshLend: p.refreshLend,
    refreshBalances: p.refreshBalances,
    refundable: p.refundable,
    report,
    toBalances: () => setTab("balances"),
  };

  return (
    <div className={`desk-page${ringTab ? " has-ring" : ""}`}>
      <div className="page-heading">
        <h1 ref={gate ? undefined : heading} tabIndex={-1}>
          {TITLE[shown]}
        </h1>
        <p>{DESC[shown]}</p>
      </div>
      <DeskRing scene={scene} hidden={!ringTab} />
      {gate ? (
        <div className="workspace-grid">
          <section className="ticket" aria-labelledby="gate-title">
            <div className="ticket-header">
              <h2 id="gate-title" ref={heading} tabIndex={-1}>
                {gateTitle}
              </h2>
            </div>
            <div className="ticket-body">{gate}</div>
          </section>
        </div>
      ) : (
        <>
          <div className="desk" hidden={shown !== "swap"}>
            <SwapTab {...p} toBalances={() => setTab("balances")} report={report} />
          </div>
          {lend && (
            <>
              <div className="desk" hidden={shown !== "earn"}>
                <EarnTab {...lendProps} lend={lend} />
              </div>
              <div className="desk" hidden={shown !== "borrow"}>
                <BorrowTab {...lendProps} lend={lend} />
              </div>
            </>
          )}
          <div className="desk" hidden={shown !== "balances"}>
            <BalancesTab {...p} registration={registration} />
          </div>
          <div className="desk desk-activity" hidden={shown !== "activity"}>
            {p.activity}
          </div>
        </>
      )}
    </div>
  );
}

/** Where each quote sits against the reference price, inside the band the page accepts. */
function PriceBand({ quotes, bandBps, chosen }: { quotes: Quote[]; bandBps: number; chosen: Quote | null }) {
  const range = Math.max(bandBps * 2.5, ...quotes.map((q) => Math.abs(q.vsFairBps) * 1.16));
  const band = (bandBps / 100).toFixed(2);
  return (
    <div className="price-band" style={{ "--band-width": `${(bandBps / range) * 100}%` } as React.CSSProperties}>
      <div className="band-heading">
        <span>Reference price band</span>
        <span>±{band}%</span>
      </div>
      <div className="band-visual" aria-hidden="true">
        <div className="band-track">
          <span className="band-center" />
          {quotes.map((q, i) => {
            const position = 50 + (q.vsFairBps / range) * 50;
            const lane = q.inBand ? (i % 2 === 0 ? "above" : "below") : "outside";
            return (
              <span
                key={q.dealer}
                className={`quote-marker ${lane}${chosen?.dealer === q.dealer ? " is-selected" : ""}`}
                style={{ "--position": `${position}%`, "--travel": `${50 - position}cqw`, "--delay": `${i * 60}ms` } as React.CSSProperties}
              />
            );
          })}
        </div>
        <div className="band-ticks">
          <span>−{band}%</span>
          <span>Reference</span>
          <span>+{band}%</span>
        </div>
      </div>
      <div className="band-caption">
        <span className="band-selected">{chosen ? `${short(chosen.dealer)} · ${bps(chosen.vsFairBps)}` : "No quote chosen"}</span>
        <span className="band-key">Outside band</span>
      </div>
    </div>
  );
}

function Dealers({ config }: { config: Config }) {
  return (
    <section className="rail-panel" aria-label="Dealers">
      <div className="rail-head">
        <h2>Dealers</h2>
        <span className="rail-tag">{config.dealers.length} listed</span>
      </div>
      <ul className="dealer-list">
        {config.dealers.map((d) => (
          <li key={d.pubkey} className="mono" title={d.pubkey}>
            {short(d.pubkey)}
          </li>
        ))}
      </ul>
      <p className="rail-foot">
        Quotes must fall within ±{(config.bandBps / 100).toFixed(2)}% of the reference price. Each dealer signs its quote; quotes outside the
        band can't be chosen.
      </p>
    </section>
  );
}

function SwapTab(p: Props & { toBalances: () => void; report: Report }) {
  const { keeler, config, balances } = p;
  const [sell, setSell] = useState(config.assets[0]!.mint);
  const [buy, setBuy] = useState(config.assets[1]?.mint ?? config.assets[0]!.mint);
  const [text, setText] = useState("");
  const [shown, setShown] = useState(false); // show validation only after a first attempt
  const [view, setView] = useState<View>({ kind: "form" });
  const act = useAction();
  const now = useChainNow(p.chainOffset);
  const submitted = useRef<{ fraction: number } | null>(null);

  const sellAsset = config.assets.find((a) => a.mint === sell)!;
  const buyAsset = config.assets.find((a) => a.mint === buy)!;
  const bal = balances?.find((b) => b.asset.mint === sell);
  const parsed = parseUnits(text, sellAsset.decimals, sellAsset.symbol);
  // The maximum is the smaller of the per-swap cap and the largest note; the message names whichever applies.
  const capped = bal !== undefined && sellAsset.maxSell !== undefined && sellAsset.maxSell < bal.largestNote;
  const max = bal ? (capped ? sellAsset.maxSell! : bal.largestNote) : 0n;
  const maxText = `${formatUnits(max, sellAsset.decimals)} ${sellAsset.symbol}`;
  let invalid = typeof parsed === "string" ? parsed : null;
  if (!invalid && bal && typeof parsed === "bigint" && parsed > max)
    invalid = capped
      ? `One swap can sell at most ${maxText}. Enter that or less.`
      : `One swap spends one private note, and your largest is ${maxText}. Enter that or less.`;
  const paused = config.paused;
  const pauseNote = paused && (
    <p className="note note-warn" role="status">
      <strong>Swaps are paused.</strong> Balances, withdrawals and taking funds back still work.
    </p>
  );
  const usable = (q: Quote) => q.inBand && (now === null || q.expiry - now >= config.minTtlSecs);
  const chosen = view.kind === "quotes" ? (view.quotes.find((q) => q.dealer === view.selected && usable(q)) ?? null) : null;

  const form = { parsed: typeof parsed === "bigint" ? parsed : null, ok: !invalid, limit: bal ? max : null };
  const pair = { base: config.assets[0]!.mint, sell, dealers: config.dealers, orders: p.orders };
  const edit = swapModel({ ...form, ...pair, view: view.kind === "quotes" ? { kind: "quotes", selected: chosen?.dealer ?? null } : { kind: "form" }, submitted: null });
  if (view.kind !== "progress") submitted.current = { fraction: edit.fraction };
  const model = view.kind === "progress" ? swapModel({ ...form, ...pair, view, submitted: submitted.current }) : edit;
  let label = "You sell";
  let value = model.valid && form.parsed !== null ? amountText(form.parsed, sellAsset) : "—";
  if (chosen) {
    label = `Quote from ${short(chosen.dealer)}`;
    value = amountText(chosen.buyAmount, chosen.buy);
  } else if (view.kind === "progress") {
    const order = view.order ? (p.orders.find((o) => o.id === view.order!.id) ?? view.order) : null;
    const q = order ?? view.quote;
    if (model.phase === "settled" && order) [label, value] = ["Received", amountText(order.buyAmount, order.buy)];
    else if (model.phase === "review") [label, value] = ["Quoted", amountText(q.buyAmount, q.buy)];
    else value = amountText(q.sellAmount, q.sell);
  }
  useReport("swap", { model, label, value }, p.report);

  const pick = (which: "sell" | "buy", mint: string) => {
    const other = config.assets.find((a) => a.mint !== mint)?.mint ?? mint;
    if (which === "sell") {
      setSell(mint);
      if (mint === buy) setBuy(other);
    } else {
      setBuy(mint);
      if (mint === sell) setSell(other);
    }
  };

  const getQuotes = () => {
    setShown(true);
    if (paused || invalid || typeof parsed !== "bigint") return;
    return act.run("quotes", async () => {
      try {
        const res = await keeler.quotes(sellAsset, buyAsset, parsed);
        const usable = res.quotes.find((q) => q.inBand);
        setView({ kind: "quotes", fair: res.fair, priceTime: res.priceTime, quotes: res.quotes, selected: usable?.dealer ?? null });
      } catch (e) {
        if (e instanceof KeelerError && e.code === "paused") p.reloadConfig();
        throw e;
      }
    });
  };

  const back = () => {
    act.setError(null);
    setView({ kind: "form" });
  };

  const swap = async (quote: Quote) => {
    setView({ kind: "progress", quote, step: null, order: null, failed: null });
    try {
      const order = await keeler.swap(quote, (step) => {
        setView((v) => (v.kind === "progress" ? { ...v, step } : v));
      });
      // swap() re-quotes at click, so from here on the order, not the clicked quote, is the truth.
      setView((v) => (v.kind === "progress" ? { ...v, order } : v));
      await Promise.all([p.refreshOrders(), p.refreshBalances()]);
    } catch (e) {
      setView((v) => (v.kind === "progress" ? { ...v, failed: errorText(e) } : v));
      if (e instanceof KeelerError && e.code === "paused") p.reloadConfig();
      // A transaction can land after an error: show whatever order it left, with its refund button.
      await Promise.all([p.refreshOrders(), p.refreshBalances()]).catch(() => {});
    }
  };

  let stage: string;
  let body: ReactNode;
  if (view.kind === "progress") {
    stage = "Swap progress";
    body = <Progress {...p} view={view} now={now} onDone={() => { setText(""); setShown(false); back(); }} onRetry={back} />;
  } else if (view.kind === "quotes") {
    stage = "Dealer quotes";
    const band = (config.bandBps / 100).toFixed(2);
    const age = now === null ? null : Math.max(0, Math.round(now - view.priceTime));
    body = (
      <div className="ticket-flow">
        {pauseNote}
        <p className="review-lede">
          For <Amount value={view.quotes[0]?.sellAmount ?? 0n} asset={sellAsset} />, the reference price gives{" "}
          <Amount value={view.fair} asset={buyAsset} />.
          {/* Fixed reference values carry a fresh time; an age under 2 s says nothing useful. */}
          <span className="num">
            {age !== null && age >= 2
              ? ` Reference price from ${age < 60 ? `${age} s` : `${Math.floor(age / 60)} min ${age % 60} s`} ago.`
              : " Reference price."}
          </span>
        </p>
        <PriceBand quotes={view.quotes} bandBps={config.bandBps} chosen={chosen} />
        <fieldset className="quotes">
          <legend className="sr-only">Choose a quote</legend>
          {view.quotes.map((q) => {
            const left = now === null ? null : q.expiry - now;
            const ok = usable(q);
            const reason = !q.inBand
              ? `Outside the ±${band}% band: ${(Math.abs(q.vsFairBps) / 100).toFixed(2)}% ${q.vsFairBps < 0 ? "below" : "above"} the reference price.`
              : !ok
                ? "Expires too soon to lock safely. Get new quotes."
                : null;
            const id = `q-${q.dealer}`;
            return (
              <label key={q.dealer} className={`quote${ok ? "" : " quote-off"}`} htmlFor={id}>
                <input
                  type="radio"
                  name="quote"
                  id={id}
                  disabled={!ok}
                  checked={chosen?.dealer === q.dealer}
                  onChange={() => setView({ ...view, selected: q.dealer })}
                  aria-describedby={reason ? `${id}-why` : undefined}
                />
                <span className="quote-main">
                  <strong>
                    Dealer <span className="mono">{short(q.dealer)}</span>
                  </strong>
                  <small className="verified">Signature verified</small>
                </span>
                <span className="quote-side">
                  <span className="quote-amt">
                    <Amount value={q.buyAmount} asset={q.buy} />
                  </span>
                  <small>
                    <span className={q.vsFairBps >= 0 ? "good" : ""}>{bps(q.vsFairBps)}</span>{" "}
                    <span className="num">{left === null ? "" : left > 0 ? `Expires in ${countdown(left)}` : "Expired"}</span>
                  </small>
                </span>
                {reason && (
                  <span className="quote-why" id={`${id}-why`}>
                    {reason}
                  </span>
                )}
              </label>
            );
          })}
        </fieldset>
        {!chosen && <p className="note">No quote can be used right now. Get new quotes to try again.</p>}
        <button className="button button-primary main-action" disabled={!chosen || paused} onClick={() => chosen && swap(chosen)}>
          Swap privately
        </button>
        <p className="ticket-foot">If the dealer doesn't settle before the quote expires, you take your funds back in one click.</p>
        <div className="review-actions">
          <button className="button button-secondary" onClick={getQuotes} disabled={!!act.busy || paused}>
            {act.busy ? "Getting quotes…" : "Get new quotes"}
          </button>
          <button className="button button-secondary" onClick={back}>
            Change amount
          </button>
        </div>
        <ErrorNote text={act.error} />
      </div>
    );
  } else {
    stage = "Swap";
    const offline = act.code === "dealer-offline";
    body = (
      <form
        className="ticket-flow swap-order"
        noValidate
        onSubmit={(e) => {
          e.preventDefault();
          getQuotes();
        }}
      >
        {pauseNote}
        <div className="money-field">
          <div className="field-frame">
            <label className="instrument-label" htmlFor="sell-amt">
              You sell
            </label>
            <div className="amount-row">
              <input
                id="sell-amt"
                inputMode="decimal"
                autoComplete="off"
                placeholder="0"
                value={text}
                onChange={(e) => setText(e.target.value)}
                aria-invalid={shown && !!invalid}
                aria-describedby="sell-hint"
              />
              <span className="asset-select">
                <select aria-label="Asset to sell" value={sell} onChange={(e) => pick("sell", e.target.value)}>
                  {config.assets.map((a) => (
                    <option key={a.mint} value={a.mint}>
                      {a.symbol}
                    </option>
                  ))}
                </select>
              </span>
            </div>
          </div>
          <div className="field-hint" id="sell-hint">
            {shown && invalid ? (
              <span className="bad">{invalid}</span>
            ) : bal ? (
              <span>
                Maximum <Amount value={max} asset={sellAsset} /> <span className="soft">{capped ? "(per-swap limit)" : "(largest private note)"}</span>
              </span>
            ) : (
              <span>Reading your private balance…</span>
            )}
            {max > 0n && (
              <button type="button" className="inline-button" onClick={() => setText(formatUnits(max, sellAsset.decimals).replace(/,/g, ""))}>
                Use maximum
              </button>
            )}
          </div>
        </div>
        <div className="direction">
          <button
            type="button"
            className="flip"
            aria-label={`Switch: sell ${buyAsset.symbol}, buy ${sellAsset.symbol}`}
            onClick={() => pick("sell", buy)}
          >
            <svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true">
              <path d="M8 4v14m0 0-4-4m4 4 4-4M16 20V6m0 0-4 4m4-4 4 4" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" />
            </svg>
          </button>
        </div>
        <div className="field-frame">
          <label className="instrument-label" htmlFor="buy-asset">
            You buy
          </label>
          <div className="amount-row">
            <span className="receive-note">Set by the quote you choose</span>
            <span className="asset-select">
              <select id="buy-asset" value={buy} onChange={(e) => pick("buy", e.target.value)}>
                {config.assets.map((a) => (
                  <option key={a.mint} value={a.mint}>
                    {a.symbol}
                  </option>
                ))}
              </select>
            </span>
          </div>
        </div>
        <dl className="form-summary">
          <div>
            <dt>Settlement</dt>
            <dd>Private balance</dd>
          </div>
          <div>
            <dt>Price band</dt>
            <dd>±{(config.bandBps / 100).toFixed(2)}% of the reference price</dd>
          </div>
        </dl>
        {bal && bal.private === 0n ? (
          <div className="empty">
            <p>You have no private {sellAsset.symbol} yet. Move some in from your wallet to swap.</p>
            <button type="button" className="button button-primary main-action" onClick={p.toBalances}>
              Go to balances
            </button>
          </div>
        ) : (
          <button className="button button-primary main-action" type="submit" disabled={!!act.busy || paused} aria-busy={!!act.busy}>
            {act.busy ? "Asking dealers…" : "Get quotes"}
          </button>
        )}
        <p className="ticket-foot">You receive at least the quoted amount, or the swap doesn't happen.</p>
        {offline ? (
          <div className="note note-error" role="alert">
            <strong>No dealers are online.</strong> {act.error}
          </div>
        ) : (
          <ErrorNote text={act.error} />
        )}
      </form>
    );
  }

  return (
    <div className="workspace-grid lending-workspace swap-workspace">
      <div className="ticket-stack">
        <section className="ticket" aria-labelledby="swap-stage">
          <div className="ticket-header">
            <h2 id="swap-stage">{stage}</h2>
          </div>
          <div className="ticket-body">
            <RefundNotice count={p.refundable} />
            {body}
          </div>
        </section>
        <details className="privacy-note">
          <summary>What stays private?</summary>
          <p>Who traded and which assets are public. How much and at what price are not. Dealers see the requests sent to them.</p>
        </details>
      </div>
      <aside className="context-rail" aria-label="Dealers and balances">
        <Dealers config={config} />
        <Funds balances={balances} />
      </aside>
    </div>
  );
}

const STEPS: { key: SwapStep | "settled"; label: string }[] = [
  { key: "checking", label: "Checking the quote" },
  { key: "proving", label: "Proving your order" },
  { key: "signing", label: "Sign in your wallet" },
  { key: "locked", label: "Locked, waiting for the dealer" },
  { key: "settled", label: "Settled" },
];

function Progress(
  p: Props & { view: Extract<View, { kind: "progress" }>; now: number | null; onDone: () => void; onRetry: () => void },
) {
  const { view, now } = p;
  // Amounts and expiry come from the order swap() returned (it re-quotes at click), refreshed from the list.
  const order = view.order ? (p.orders.find((o) => o.id === view.order!.id) ?? view.order) : null;
  const q = order ?? view.quote;
  const act = useAction();
  const status = order?.status;
  const at = status === "settled" ? 4 : order ? 3 : STEPS.findIndex((s) => s.key === view.step);
  const left = now === null ? null : q.expiry - now;

  let line: string;
  if (view.failed) line = "The swap stopped.";
  // The client flags a settled order whose payout it could not match to the quote; show that, not an amount.
  else if (status === "settled" && order?.error) line = order.error;
  else if (status === "settled") line = `Settled. You received ${formatUnits(q.buyAmount, q.buy.decimals)} ${q.buy.symbol} privately.`;
  else if (status === "refunded") line = `Done. Your ${formatUnits(q.sellAmount, q.sell.decimals)} ${q.sell.symbol} is back in your private balance.`;
  else if (status === "refundable")
    line = `The dealer did not settle before the quote expired. Your ${formatUnits(q.sellAmount, q.sell.decimals)} ${q.sell.symbol} is still yours: take it back.`;
  else if (status === "failed") line = order?.error ?? "The order failed.";
  else if (order) line = "Locked. Waiting for the dealer to settle.";
  else line = STEPS[Math.max(at, 0)]!.label + "…";

  return (
    <div className="ticket-flow">
      {status === "settled" && !order?.error && <span className="result-mark" aria-hidden="true" />}
      <p className="review-lede">
        {order ? (
          <>
            <Amount value={q.sellAmount} asset={q.sell} /> for <Amount value={q.buyAmount} asset={q.buy} />
          </>
        ) : (
          <>
            Selling <Amount value={q.sellAmount} asset={q.sell} /> for {q.buy.symbol}
          </>
        )}
      </p>
      <ol className="steps" aria-label="Swap progress">
        {STEPS.map((s, i) => {
          const state =
            view.failed && i === at ? "fail" : i < at || (i === at && s.key === "settled") ? "done" : i === at && !view.failed ? "now" : "todo";
          const refundable = s.key === "locked" && (status === "refundable" || status === "refunded");
          return (
            <li key={s.key} className={`step step-${refundable ? "warn" : state}`} aria-current={state === "now" ? "step" : undefined}>
              <span className="step-dot" aria-hidden="true" />
              <span>
                {refundable ? "Dealer did not settle" : s.label}
                {s.key === "proving" && state === "now" && <span className="step-sub">Keep this tab open.</span>}
                {s.key === "locked" && state === "now" && status === "locked" && left !== null && (
                  <span className="step-sub num">{left > 0 ? `Dealer has ${countdown(left)} to settle` : "Quote expired, checking…"}</span>
                )}
              </span>
            </li>
          );
        })}
      </ol>
      <p className={`status${view.failed || status === "failed" ? " bad" : ""}`} aria-live="polite">
        {line}
      </p>
      {order && <OrderLinks order={order} />}
      {view.failed && <p className="note note-error" role="alert">{view.failed}</p>}
      {status === "refundable" && (
        <button
          className="button button-primary main-action"
          disabled={!!act.busy}
          aria-busy={!!act.busy}
          onClick={() =>
            act.run("refund", async () => {
              await p.keeler.refund(order!.id);
              await Promise.all([p.refreshOrders(), p.refreshBalances()]);
            })
          }
        >
          {act.busy ? "Taking funds back…" : "Take funds back"}
        </button>
      )}
      {view.failed ? (
        <button className="button button-primary main-action" onClick={p.onRetry}>
          Back to the swap form
        </button>
      ) : (
        (status === "settled" || status === "refunded" || status === "failed") && (
          <button className="button button-primary main-action" onClick={p.onDone}>
            New swap
          </button>
        )
      )}
      <ErrorNote text={act.error} />
    </div>
  );
}

type Done = { text: string; link?: { signature: string; label: string; says: string } };

function BalancesTab(p: Props & { registration: string | undefined }) {
  const { keeler, config, balances } = p;
  const [mode, setMode] = useState<"deposit" | "withdraw">("deposit");
  const [mint, setMint] = useState(config.assets[0]!.mint);
  const [text, setText] = useState("");
  const [shown, setShown] = useState(false);
  const [done, setDone] = useState<Done | null>(
    p.registration ? { text: "Wallet registered.", link: { signature: p.registration, label: "View registration on Orb", says: ON_CHAIN.register } } : null,
  );
  const act = useAction();

  const asset = config.assets.find((a) => a.mint === mint)!;
  const bal = balances?.find((b) => b.asset.mint === mint);
  const max = bal ? (mode === "deposit" ? bal.public : bal.largestNote) : 0n;
  const parsed = parseUnits(text, asset.decimals, asset.symbol);
  let invalid = typeof parsed === "string" ? parsed : null;
  if (!invalid && bal && typeof parsed === "bigint" && parsed > max)
    invalid =
      mode === "deposit"
        ? `You have ${formatUnits(max, asset.decimals)} public ${asset.symbol}. Enter that or less.`
        : `One withdrawal spends one private note, and your largest is ${formatUnits(max, asset.decimals)} ${asset.symbol}.`;
  const empty = balances !== null && balances.every((b) => b.public === 0n && b.private === 0n);

  const move = () => {
    setShown(true);
    setDone(null);
    if (invalid || typeof parsed !== "bigint") return;
    return act.run(mode, async () => {
      const signature = await (mode === "deposit" ? keeler.deposit(asset, parsed) : keeler.withdraw(asset, parsed));
      await p.refreshBalances();
      setDone({
        text: `${mode === "deposit" ? "Deposited" : "Withdrew"} ${formatUnits(parsed, asset.decimals)} ${asset.symbol}.`,
        link: { signature, label: "View transaction on Orb", says: ON_CHAIN.move },
      });
      setText("");
      setShown(false);
    });
  };

  const faucet = () =>
    act.run("faucet", async () => {
      setDone(null);
      await keeler.faucet();
      await p.refreshBalances();
      setDone({ text: "Test tokens sent to your public balance." });
    });

  return (
    <div className="workspace-grid balances-workspace">
      <section className="ticket" aria-labelledby="move-title">
        <div className="ticket-header">
          <h2 id="move-title">Move funds</h2>
        </div>
        <div className="ticket-body">
          <RefundNotice count={p.refundable} />
          {config.faucet && empty && (
            <div className="empty">
              <p>Your wallet has no test tokens yet. Get some to try a swap.</p>
              <button className="button button-primary main-action" onClick={faucet} disabled={!!act.busy} aria-busy={act.busy === "faucet"}>
                {act.busy === "faucet" ? "Sending test tokens…" : "Get test tokens"}
              </button>
            </div>
          )}
          <form
            className="ticket-flow move"
            noValidate
            onSubmit={(e) => {
              e.preventDefault();
              move();
            }}
          >
            <div className="mode-picker" role="group" aria-label="Direction">
              <button type="button" aria-pressed={mode === "deposit"} onClick={() => setMode("deposit")}>
                Deposit
              </button>
              <button type="button" aria-pressed={mode === "withdraw"} onClick={() => setMode("withdraw")}>
                Withdraw
              </button>
            </div>
            <div className="money-field">
              <div className="field-frame">
                <label className="instrument-label" htmlFor="move-amt">
                  Amount
                </label>
                <div className="amount-row">
                  <input
                    id="move-amt"
                    inputMode="decimal"
                    autoComplete="off"
                    placeholder="0"
                    value={text}
                    onChange={(e) => setText(e.target.value)}
                    aria-invalid={shown && !!invalid}
                    aria-describedby="move-hint"
                  />
                  <span className="asset-select">
                    <select aria-label="Asset" value={mint} onChange={(e) => setMint(e.target.value)}>
                      {config.assets.map((a: Asset) => (
                        <option key={a.mint} value={a.mint}>
                          {a.symbol}
                        </option>
                      ))}
                    </select>
                  </span>
                </div>
              </div>
              <div className="field-hint" id="move-hint">
                {shown && invalid ? (
                  <span className="bad">{invalid}</span>
                ) : (
                  <span>
                    {mode === "deposit" ? "Public balance" : "Largest private note"} <Amount value={max} asset={asset} />
                  </span>
                )}
                {max > 0n && (
                  <button type="button" className="inline-button" onClick={() => setText(formatUnits(max, asset.decimals).replace(/,/g, ""))}>
                    Use maximum
                  </button>
                )}
              </div>
            </div>
            <dl className="form-summary">
              <div>
                <dt>From</dt>
                <dd>{mode === "deposit" ? "Public balance" : "Private balance"}</dd>
              </div>
              <div>
                <dt>To</dt>
                <dd>{mode === "deposit" ? "Private balance" : "Public balance"}</dd>
              </div>
            </dl>
            <button className="button button-primary main-action" type="submit" disabled={!!act.busy} aria-busy={act.busy === mode}>
              {act.busy === mode ? (mode === "deposit" ? "Depositing…" : "Withdrawing…") : mode === "deposit" ? "Deposit" : "Withdraw"}
            </button>
            <p className="ticket-foot">
              {mode === "deposit"
                ? "Moves tokens from your public wallet balance into your private balance. The amount of this move is public."
                : "Moves tokens from your private balance back to your public wallet balance. The amount of this move is public."}
            </p>
          </form>
          <p className="status" aria-live="polite">
            {done?.text}
          </p>
          {done?.link && <OrbLinks links={[done.link]} says={done.link.says} />}
          <ErrorNote text={act.error} />
        </div>
      </section>
      <aside className="context-rail" aria-label="Account balances">
        <section className="rail-panel" aria-label="Your balances">
          <div className="rail-head">
            <h2>Account balances</h2>
          </div>
          <table className="bal data-table balance-table">
            <caption className="sr-only">Your balances</caption>
            <thead>
              <tr>
                <th scope="col">Asset</th>
                <th scope="col">Public</th>
                <th scope="col">Private</th>
                <th scope="col">Largest note</th>
              </tr>
            </thead>
            <tbody>
              {(balances ?? []).map((b) => (
                <tr key={b.asset.mint}>
                  <th scope="row">{b.asset.symbol}</th>
                  <td className="num">{formatUnits(b.public, b.asset.decimals)}</td>
                  <td className="num">{formatUnits(b.private, b.asset.decimals)}</td>
                  <td className="num">{formatUnits(b.largestNote, b.asset.decimals)}</td>
                </tr>
              ))}
              {!balances && (
                <tr>
                  <td colSpan={4} className="muted">
                    Reading balances…
                  </td>
                </tr>
              )}
            </tbody>
          </table>
          <p className="rail-foot">Each private spend uses one note, which can be smaller than your total private balance.</p>
          {config.faucet && !empty && (
            <div className="faucet-box">
              <button className="button button-secondary" onClick={faucet} disabled={!!act.busy} aria-busy={act.busy === "faucet"}>
                {act.busy === "faucet" ? "Sending test tokens…" : "Get test tokens"}
              </button>
            </div>
          )}
        </section>
      </aside>
    </div>
  );
}
