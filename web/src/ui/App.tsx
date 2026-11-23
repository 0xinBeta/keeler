import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Balance, Config, Keeler, Order } from "../keeler/index";
import { Audience, ChainSees, Closing, Dealers, Facts, Faq, Features, Steps } from "./Landing";
import { Orders } from "./Orders";
import { Panel } from "./Panel";
import type { DealerLight } from "./ring";
import { RingCanvas } from "./RingCanvas";
import { errorText } from "./shared";

const PILL = { local: "Local", devnet: "Devnet beta", mainnet: "Mainnet" } as const;
const OPEN: Order["status"][] = ["pending", "locked"];

export function NetworkPill({ config }: { config: Config | null }) {
  if (!config) return null;
  return (
    <span className="net-pill">
      <span className="net-dot" aria-hidden="true" />
      {PILL[config.network]}
    </span>
  );
}

export function Logo() {
  return (
    <svg viewBox="0 0 40 24" width="34" height="20" aria-hidden="true" className="logo">
      <ellipse cx="20" cy="12" rx="17" ry="7" fill="none" stroke="currentColor" strokeWidth="1.6" />
      <ellipse cx="20" cy="12" rx="12" ry="4.6" fill="none" stroke="currentColor" strokeWidth="1.6" opacity=".55" />
      <circle cx="31.5" cy="8.6" r="1.9" fill="#F4EBD6" />
    </svg>
  );
}

export function App({ keeler }: { keeler: Keeler }) {
  const [config, setConfig] = useState<Config | null>(null);
  const [configError, setConfigError] = useState<string | null>(null);
  const [ready, setReady] = useState(false); // wallet connected, unlocked and registered
  const [balances, setBalances] = useState<Balance[] | null>(null);
  const [orders, setOrders] = useState<Order[]>([]);
  const [chainOffset, setChainOffset] = useState<number | null>(null);
  const [lights, setLights] = useState<DealerLight[] | null>(null);
  const [proving, setProving] = useState(false);
  const [settled, setSettled] = useState(0);
  const panel = useRef<HTMLElement>(null);
  const hero = useRef<HTMLElement>(null);
  const seen = useRef(new Map<string, Order["status"]>());
  const inflight = useRef<Promise<Order[]> | null>(null);

  const reloadConfig = useCallback(() => {
    keeler.config().then(
      (c) => {
        setConfig(c);
        setConfigError(null);
      },
      (e) => setConfigError((prev) => prev ?? errorText(e)),
    );
  }, [keeler]);

  useEffect(() => {
    reloadConfig();
    // A pause or unpause shows without a reload.
    const cfg = setInterval(reloadConfig, 60_000);
    const read = () =>
      keeler.chainNow().then(
        (t) => setChainOffset(t - Date.now() / 1000),
        () => {},
      );
    read();
    const clock = setInterval(read, 20_000);
    return () => {
      clearInterval(cfg);
      clearInterval(clock);
    };
  }, [keeler, reloadConfig]);

  const refreshBalances = useCallback(() => keeler.balances().then(setBalances), [keeler]);
  // Single flight: a call while one is running shares its result instead of starting another.
  const refreshOrders = useCallback(() => {
    if (inflight.current) return inflight.current;
    const run = keeler
      .orders()
      .then((list) => {
        // The ring's settle front runs once for each order that settles while the page is open.
        for (const o of list) {
          const before = seen.current.get(o.id);
          if (before && before !== "settled" && o.status === "settled") setSettled((n) => n + 1);
          seen.current.set(o.id, o.status);
        }
        setOrders(list);
        return list;
      })
      .finally(() => {
        inflight.current = null;
      });
    inflight.current = run;
    return run;
  }, [keeler]);

  useEffect(() => {
    if (!ready) return;
    refreshBalances().catch(() => {});
    refreshOrders().catch(() => {});
  }, [ready, refreshBalances, refreshOrders]);

  // Poll every 5 s while an order is open, never overlapping, backing off on errors; stop when none is open.
  const open = orders.some((o) => OPEN.includes(o.status));
  useEffect(() => {
    if (!open) return;
    let stopped = false;
    let delay = 5000;
    let timer = 0;
    const tick = async () => {
      try {
        const before = new Map(seen.current);
        const list = await refreshOrders();
        if (list.some((o) => before.get(o.id) !== o.status)) refreshBalances().catch(() => {});
        delay = 5000;
      } catch {
        delay = Math.min(delay * 2, 60_000);
      }
      if (!stopped) timer = window.setTimeout(tick, delay);
    };
    timer = window.setTimeout(tick, delay);
    return () => {
      stopped = true;
      clearTimeout(timer);
    };
  }, [open, refreshOrders, refreshBalances]);

  const ringInput = useMemo(() => ({ dealers: lights, proving, settled }), [lights, proving, settled]);
  const startSwap = useCallback(() => {
    panel.current?.focus();
    panel.current?.scrollIntoView({ block: "center" });
  }, []);

  return (
    <>
      <a className="skip" href="#swap">
        Skip to the swap
      </a>
      <div className="stage">
        <RingCanvas mode="hero" input={ringInput} anchor={panel} hero={hero} />
        <header className="top">
          <a className="wordmark" href="#top" aria-label="Keeler, top of page">
            <Logo />
            Keeler
          </a>
          <nav aria-label="On this page">
            <a href="#how">How it works</a>
            <a href="#privacy">Privacy</a>
            <a href="#dealers">Dealers</a>
            <a href="#faq">FAQ</a>
          </nav>
          <NetworkPill config={config} />
        </header>
        <main id="top">
          <section className="hero" ref={hero} aria-labelledby="hero-title">
            <div className="intro">
              <h1 id="hero-title">Swap without showing your hand.</h1>
              <p className="lede">
                Keeler gets you signed quotes from dealers, checks them against a reference price, and settles the best
                one privately on Solana. The chain sees that you traded. It never sees how much, or at what price.
              </p>
              <div className="pills">
                <button className="pill pill-gold" onClick={startSwap}>
                  Start a swap
                </button>
                <a className="pill pill-outline" href="#how">
                  See how it works
                </a>
              </div>
              <ul className="chips" aria-label="Foundations">
                <li>Built on Helius Privacy Rings</li>
                <li>Settles on Solana</li>
                <li>Order proved in your browser</li>
              </ul>
            </div>
            <section className="panel" id="swap" ref={panel} tabIndex={-1} aria-label="Private swap">
              {config ? (
                <Panel
                  keeler={keeler}
                  config={config}
                  balances={balances}
                  orders={orders}
                  chainOffset={chainOffset}
                  onReady={() => setReady(true)}
                  refreshBalances={refreshBalances}
                  refreshOrders={refreshOrders}
                  reloadConfig={reloadConfig}
                  setLights={setLights}
                  setProving={setProving}
                />
              ) : configError ? (
                <div className="panel-body">
                  <h2 className="panel-title">Keeler is not reachable</h2>
                  <p className="note note-error" role="alert">
                    {configError}
                  </p>
                  <button className="btn btn-primary" onClick={() => location.reload()}>
                    Reload the page
                  </button>
                </div>
              ) : (
                <div className="panel-body" aria-busy="true">
                  <p className="muted">Loading…</p>
                </div>
              )}
            </section>
          </section>
          <Facts />
          {ready && (
            <Orders
              keeler={keeler}
              orders={orders}
              chainOffset={chainOffset}
              refresh={refreshOrders}
              refreshBalances={refreshBalances}
              rescan={() =>
                keeler.rescanOrders().then((list) => {
                  for (const o of list) seen.current.set(o.id, o.status);
                  setOrders(list);
                  return list;
                })
              }
            />
          )}
          <Features />
          <ChainSees />
          <Steps />
          <Audience />
          <Dealers />
          <Faq />
          <Closing onStart={startSwap} />
        </main>
      </div>
      <footer className="foot">
        <div className="foot-brand">
          <Logo />
          <span>Keeler</span>
        </div>
        <p className="muted">Built on Helius Privacy Rings</p>
        <NetworkPill config={config} />
      </footer>
    </>
  );
}
