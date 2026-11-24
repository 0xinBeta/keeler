import { useEffect, useRef, useState } from "react";
import type { Dispatch, SetStateAction } from "react";
import type { Wallet } from "@wallet-standard/base";
import { KeelerError } from "../keeler/index";
import type { Asset, Balance, Config, Keeler, LendAccount, Order, Quote, SwapStep } from "../keeler/index";
import { bps, countdown, formatUnits, parseUnits, short } from "./format";
import { BorrowTab, EarnTab } from "./Lend";
import type { Market } from "./Lend";
import type { DealerLight } from "./ring";
import { Amount, ErrorNote, errorText, useChainNow } from "./shared";

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
  setLights: Dispatch<SetStateAction<DealerLight[] | null>>;
  setProving: (p: boolean) => void;
  tab: Tab;
  setTab: (t: Tab) => void;
  market: Market | null;
  account: LendAccount | null;
  refreshLend: (prompt?: boolean) => Promise<unknown>;
};

export type Tab = "swap" | "earn" | "borrow" | "balances";
const TITLE: Record<Tab, string> = { swap: "Swap privately", earn: "Earn privately", borrow: "Borrow privately", balances: "Balances" };

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

export function Panel(p: Props) {
  const { keeler, config } = p;
  const [phase, setPhase] = useState<Phase>("wallet");
  const [address, setAddress] = useState<string | null>(null);
  const { tab, setTab } = p;
  const act = useAction();
  const heading = useRef<HTMLHeadingElement>(null);
  const first = useRef(true);

  // Move focus to the new heading when the panel changes screen, so keyboard and screen reader users follow.
  useEffect(() => {
    if (first.current) {
      first.current = false;
      return;
    }
    heading.current?.focus();
  }, [phase, tab]);

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
  const register = () =>
    act.run("register", async () => {
      await keeler.register();
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

  if (phase === "wallet") {
    const wallets = keeler.wallets();
    return (
      <div className="panel-body">
        <h2 className="panel-title" ref={heading} tabIndex={-1}>
          Choose a wallet
        </h2>
        <p className="muted">Your wallet signs; your private keys are created in this browser and Keeler's servers never see them.</p>
        {wallets.length === 0 ? (
          <p className="note">No Solana wallet was found in this browser. Install one, such as Phantom, then reload the page.</p>
        ) : (
          <ul className="wallets">
            {wallets.map((w) => (
              <li key={w.name}>
                <button className="wallet" onClick={() => connect(w)} disabled={!!act.busy} aria-busy={act.busy === w.name}>
                  <img src={w.icon} alt="" width="28" height="28" />
                  <span>{w.name}</span>
                  <span className="muted">{act.busy === w.name ? "Connecting…" : "Connect"}</span>
                </button>
                {/* shortcut: recognised by name; importing the client's constant would pull the client into the first bundle. */}
                {/browser test wallet/i.test(w.name) && (
                  <p className="muted small wallet-note">A test key kept in this browser. Good for trying Keeler; clearing site data removes it.</p>
                )}
              </li>
            ))}
          </ul>
        )}
        <ErrorNote text={act.error} />
      </div>
    );
  }

  if (phase === "unlock" || phase === "register") {
    const unlocking = phase === "unlock";
    return (
      <div className="panel-body">
        <h2 className="panel-title" ref={heading} tabIndex={-1}>
          {unlocking ? "Unlock your private balance" : "Register this wallet"}
        </h2>
        <p className="addr">
          Connected <span className="mono">{short(address ?? "")}</span>
        </p>
        <p>
          {unlocking
            ? "This signature creates your private keys. It does not move funds."
            : "Registering adds your private-note keys to the public Rings registry, so dealers can pay you privately. It is one transaction and does not move funds."}
        </p>
        <button
          className="btn btn-primary"
          onClick={unlocking ? unlock : register}
          disabled={!!act.busy}
          aria-busy={!!act.busy}
        >
          {act.busy === "unlock" ? "Waiting for your wallet…" : act.busy === "register" ? "Registering…" : unlocking ? "Sign to unlock" : "Register"}
        </button>
        {!unlocking && config.faucet && (
          <div className="faucet">
            <p className="muted small">Registering costs a small network fee in SOL. On this test network you can get some here.</p>
            <button className="btn btn-ghost" onClick={faucet} disabled={!!act.busy} aria-busy={act.busy === "faucet"}>
              {act.busy === "faucet" ? "Sending test tokens…" : "Get test tokens"}
            </button>
          </div>
        )}
        <p className="status" aria-live="polite">
          {funded ? "Test tokens sent to your public balance." : ""}
        </p>
        <ErrorNote text={act.error} />
      </div>
    );
  }

  const lend = keeler.lend;
  const tabs: Tab[] = lend ? ["swap", "earn", "borrow", "balances"] : ["swap", "balances"];
  const shown: Tab = tabs.includes(tab) ? tab : "swap";
  const lendProps = {
    balances: p.balances,
    market: p.market,
    account: p.account,
    chainOffset: p.chainOffset,
    refreshLend: p.refreshLend,
    refreshBalances: p.refreshBalances,
  };

  return (
    <div className="panel-body">
      <div className="panel-head">
        <h2 className="panel-title" ref={heading} tabIndex={-1}>
          {TITLE[shown]}
        </h2>
        <span className="addr mono" title={address ?? ""}>
          {short(address ?? "")}
        </span>
      </div>
      <div className={`seg${lend ? " seg-4" : ""}`} role="group" aria-label="Panel">
        {tabs.map((t) => (
          <button key={t} aria-pressed={shown === t} onClick={() => setTab(t)}>
            {t === "swap" ? "Swap" : t === "earn" ? "Earn" : t === "borrow" ? "Borrow" : "Balances"}
          </button>
        ))}
      </div>
      {/* All stay mounted so an operation in progress survives a look at another tab. */}
      <div hidden={shown !== "swap"}>
        <SwapTab {...p} toBalances={() => setTab("balances")} />
      </div>
      {lend && (
        <>
          <div hidden={shown !== "earn"}>
            <EarnTab {...lendProps} lend={lend} />
          </div>
          <div hidden={shown !== "borrow"}>
            <BorrowTab {...lendProps} lend={lend} />
          </div>
        </>
      )}
      <div hidden={shown !== "balances"}>
        <BalancesTab {...p} />
      </div>
    </div>
  );
}

function SwapTab(p: Props & { toBalances: () => void }) {
  const { keeler, config, balances } = p;
  const [sell, setSell] = useState(config.assets[0]!.mint);
  const [buy, setBuy] = useState(config.assets[1]?.mint ?? config.assets[0]!.mint);
  const [text, setText] = useState("");
  const [shown, setShown] = useState(false); // show validation only after a first attempt
  const [view, setView] = useState<View>({ kind: "form" });
  const act = useAction();
  const now = useChainNow(p.chainOffset);

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
    p.setLights(config.dealers.map(() => "waiting"));
    return act.run("quotes", async () => {
      try {
        const res = await keeler.quotes(sellAsset, buyAsset, parsed);
        const answered = new Set(res.quotes.map((q) => q.dealer));
        // Light each dealer in turn so the ring shows the answers arriving.
        config.dealers.forEach((d, i) =>
          setTimeout(
            () => p.setLights((l) => l && l.map((s, j) => (j === i ? (answered.has(d.pubkey) ? "answered" : "silent") : s))),
            120 + i * 260,
          ),
        );
        const usable = res.quotes.find((q) => q.inBand);
        setView({ kind: "quotes", fair: res.fair, priceTime: res.priceTime, quotes: res.quotes, selected: usable?.dealer ?? null });
      } catch (e) {
        p.setLights(null);
        if (e instanceof KeelerError && e.code === "paused") p.reloadConfig();
        throw e;
      }
    });
  };

  const back = () => {
    p.setLights(null);
    act.setError(null);
    setView({ kind: "form" });
  };

  const swap = async (quote: Quote) => {
    p.setLights(null);
    setView({ kind: "progress", quote, step: null, order: null, failed: null });
    try {
      const order = await keeler.swap(quote, (step) => {
        p.setProving(step === "proving");
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
    } finally {
      p.setProving(false);
    }
  };

  if (view.kind === "progress") return <Progress {...p} view={view} now={now} onDone={() => { setText(""); setShown(false); back(); }} onRetry={back} />;

  if (view.kind === "quotes") {
    const usable = (q: Quote) => q.inBand && (now === null || q.expiry - now >= config.minTtlSecs);
    const chosen = view.quotes.find((q) => q.dealer === view.selected && usable(q)) ?? null;
    const band = (config.bandBps / 100).toFixed(2);
    const age = now === null ? null : Math.max(0, Math.round(now - view.priceTime));
    return (
      <div className="stack">
        {pauseNote}
        <p className="muted">
          For <Amount value={view.quotes[0]?.sellAmount ?? 0n} asset={sellAsset} />, the reference price gives{" "}
          <Amount value={view.fair} asset={buyAsset} />.
          {/* Fixed reference values carry a fresh time; an age under 2 s says nothing useful. */}
          <span className="num">
            {age !== null && age >= 2
              ? ` Reference price from ${age < 60 ? `${age} s` : `${Math.floor(age / 60)} min ${age % 60} s`} ago.`
              : " Reference price."}
          </span>
        </p>
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
                  <span className="quote-amt">
                    <Amount value={q.buyAmount} asset={q.buy} />
                  </span>
                  <span className="muted">
                    Dealer <span className="mono">{short(q.dealer)}</span>
                  </span>
                  <span className="muted verified">Signature verified</span>
                </span>
                <span className="quote-side">
                  <span className={q.vsFairBps >= 0 ? "good" : "muted"}>{bps(q.vsFairBps)}</span>
                  <span className="muted num">{left === null ? "" : left > 0 ? `Expires in ${countdown(left)}` : "Expired"}</span>
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
        <button className="btn btn-primary" disabled={!chosen || paused} onClick={() => chosen && swap(chosen)}>
          Swap privately
        </button>
        <p className="muted small">If the dealer doesn't settle before the quote expires, you take your funds back in one click.</p>
        <div className="row">
          <button className="btn btn-ghost" onClick={getQuotes} disabled={!!act.busy || paused}>
            {act.busy ? "Getting quotes…" : "Get new quotes"}
          </button>
          <button className="btn btn-ghost" onClick={back}>
            Change amount
          </button>
        </div>
        <ErrorNote text={act.error} />
      </div>
    );
  }

  const offline = act.code === "dealer-offline";
  return (
    <form
      className="stack"
      noValidate
      onSubmit={(e) => {
        e.preventDefault();
        getQuotes();
      }}
    >
      {pauseNote}
      <div className="field">
        <label htmlFor="sell-amt">You sell</label>
        <div className="combo">
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
          <select aria-label="Asset to sell" value={sell} onChange={(e) => pick("sell", e.target.value)}>
            {config.assets.map((a) => (
              <option key={a.mint} value={a.mint}>
                {a.symbol}
              </option>
            ))}
          </select>
        </div>
        <div className="hint" id="sell-hint">
          {shown && invalid ? (
            <span className="bad">{invalid}</span>
          ) : bal ? (
            <span>
              Maximum <Amount value={max} asset={sellAsset} />{" "}
              <span className="muted">{capped ? "(per-swap limit)" : "(largest private note)"}</span>
            </span>
          ) : (
            <span>Reading your private balance…</span>
          )}
          {max > 0n && (
            <button type="button" className="link" onClick={() => setText(formatUnits(max, sellAsset.decimals).replace(/,/g, ""))}>
              Use maximum
            </button>
          )}
        </div>
      </div>
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
      <div className="field">
        <label htmlFor="buy-asset">You buy</label>
        <select id="buy-asset" className="wide" value={buy} onChange={(e) => pick("buy", e.target.value)}>
          {config.assets.map((a) => (
            <option key={a.mint} value={a.mint}>
              {a.symbol}
            </option>
          ))}
        </select>
      </div>
      {bal && bal.private === 0n ? (
        <div className="empty">
          <p>You have no private {sellAsset.symbol} yet. Move some in from your wallet to swap.</p>
          <button type="button" className="btn btn-primary" onClick={p.toBalances}>
            Go to balances
          </button>
        </div>
      ) : (
        <button className="btn btn-primary" type="submit" disabled={!!act.busy || paused} aria-busy={!!act.busy}>
          {act.busy ? "Asking dealers…" : "Get quotes"}
        </button>
      )}
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
    <div className="stack">
      <p className="muted">
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
                {s.key === "proving" && state === "now" && <span className="muted step-sub">Keep this tab open.</span>}
                {s.key === "locked" && state === "now" && status === "locked" && left !== null && (
                  <span className="muted step-sub num">
                    {left > 0 ? `Dealer has ${countdown(left)} to settle` : "Quote expired, checking…"}
                  </span>
                )}
              </span>
            </li>
          );
        })}
      </ol>
      <p className={`status${view.failed || status === "failed" ? " bad" : ""}`} aria-live="polite">
        {line}
      </p>
      {view.failed && <p className="note note-error" role="alert">{view.failed}</p>}
      {status === "refundable" && (
        <button
          className="btn btn-primary"
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
        <button className="btn btn-primary" onClick={p.onRetry}>
          Back to the swap form
        </button>
      ) : (
        (status === "settled" || status === "refunded" || status === "failed") && (
          <button className="btn btn-primary" onClick={p.onDone}>
            New swap
          </button>
        )
      )}
      <ErrorNote text={act.error} />
    </div>
  );
}

function BalancesTab(p: Props) {
  const { keeler, config, balances } = p;
  const [mode, setMode] = useState<"deposit" | "withdraw">("deposit");
  const [mint, setMint] = useState(config.assets[0]!.mint);
  const [text, setText] = useState("");
  const [shown, setShown] = useState(false);
  const [done, setDone] = useState<string | null>(null);
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
      await (mode === "deposit" ? keeler.deposit(asset, parsed) : keeler.withdraw(asset, parsed));
      await p.refreshBalances();
      setDone(`${mode === "deposit" ? "Deposited" : "Withdrew"} ${formatUnits(parsed, asset.decimals)} ${asset.symbol}.`);
      setText("");
      setShown(false);
    });
  };

  const faucet = () =>
    act.run("faucet", async () => {
      setDone(null);
      await keeler.faucet();
      await p.refreshBalances();
      setDone("Test tokens sent to your public balance.");
    });

  return (
    <div className="stack">
      <table className="bal">
        <caption className="sr-only">Your balances</caption>
        <thead>
          <tr>
            <th scope="col">Asset</th>
            <th scope="col">Public</th>
            <th scope="col">Private</th>
          </tr>
        </thead>
        <tbody>
          {(balances ?? []).map((b) => (
            <tr key={b.asset.mint}>
              <th scope="row">{b.asset.symbol}</th>
              <td className="num">{formatUnits(b.public, b.asset.decimals)}</td>
              <td className="num">{formatUnits(b.private, b.asset.decimals)}</td>
            </tr>
          ))}
          {!balances && (
            <tr>
              <td colSpan={3} className="muted">
                Reading balances…
              </td>
            </tr>
          )}
        </tbody>
      </table>
      {config.faucet && (
        <div className={empty ? "empty" : "faucet"}>
          {empty && <p>Your wallet has no test tokens yet. Get some to try a swap.</p>}
          <button className={`btn ${empty ? "btn-primary" : "btn-ghost"}`} onClick={faucet} disabled={!!act.busy} aria-busy={act.busy === "faucet"}>
            {act.busy === "faucet" ? "Sending test tokens…" : "Get test tokens"}
          </button>
        </div>
      )}
      <form
        className="stack move"
        noValidate
        onSubmit={(e) => {
          e.preventDefault();
          move();
        }}
      >
        <div className="seg seg-small" role="group" aria-label="Direction">
          <button type="button" aria-pressed={mode === "deposit"} onClick={() => setMode("deposit")}>
            Deposit
          </button>
          <button type="button" aria-pressed={mode === "withdraw"} onClick={() => setMode("withdraw")}>
            Withdraw
          </button>
        </div>
        <p className="muted small">
          {mode === "deposit"
            ? "Moves tokens from your public wallet balance into your private balance."
            : "Moves tokens from your private balance back to your public wallet balance."}
        </p>
        <div className="field">
          <label htmlFor="move-amt">Amount</label>
          <div className="combo">
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
            <select aria-label="Asset" value={mint} onChange={(e) => setMint(e.target.value)}>
              {config.assets.map((a: Asset) => (
                <option key={a.mint} value={a.mint}>
                  {a.symbol}
                </option>
              ))}
            </select>
          </div>
          <div className="hint" id="move-hint">
            {shown && invalid ? (
              <span className="bad">{invalid}</span>
            ) : (
              <span>
                {mode === "deposit" ? "Public balance" : "Largest private note"} <Amount value={max} asset={asset} />
              </span>
            )}
            {max > 0n && (
              <button type="button" className="link" onClick={() => setText(formatUnits(max, asset.decimals).replace(/,/g, ""))}>
                Use maximum
              </button>
            )}
          </div>
        </div>
        <button className="btn btn-primary" type="submit" disabled={!!act.busy} aria-busy={act.busy === mode}>
          {act.busy === mode ? (mode === "deposit" ? "Depositing…" : "Withdrawing…") : mode === "deposit" ? "Deposit" : "Withdraw"}
        </button>
      </form>
      <p className="status" aria-live="polite">
        {done}
      </p>
      <ErrorNote text={act.error} />
    </div>
  );
}
