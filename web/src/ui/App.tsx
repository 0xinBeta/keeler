import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Balance, Config, Keeler, Order } from "../keeler/index";
import { Info } from "./Info";
import { Orders } from "./Orders";
import { Panel } from "./Panel";
import type { DealerLight } from "./ring";
import { RingCanvas } from "./RingCanvas";
import { errorText } from "./shared";

const NETWORK = { local: "local network", devnet: "devnet", mainnet: "mainnet" } as const;

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

  useEffect(() => {
    keeler.config().then(setConfig, (e) => setConfigError(errorText(e)));
    const read = () =>
      keeler.chainNow().then(
        (t) => setChainOffset(t - Date.now() / 1000),
        () => {},
      );
    read();
    const id = setInterval(read, 20_000);
    return () => clearInterval(id);
  }, [keeler]);

  const refreshBalances = useCallback(() => keeler.balances().then(setBalances), [keeler]);
  const refreshOrders = useCallback(
    () =>
      keeler.orders().then((list) => {
        // The ring's settle front runs once for each order that settles while the page is open.
        for (const o of list) {
          const before = seen.current.get(o.id);
          if (before && before !== "settled" && o.status === "settled") setSettled((n) => n + 1);
          seen.current.set(o.id, o.status);
        }
        setOrders(list);
        return list;
      }),
    [keeler],
  );

  useEffect(() => {
    if (!ready) return;
    refreshBalances().catch(() => {});
    refreshOrders().catch(() => {});
  }, [ready, refreshBalances, refreshOrders]);

  // Poll while an order waits on the dealer or the clock.
  const waiting = orders.some((o) => o.status === "pending" || o.status === "locked");
  useEffect(() => {
    if (!waiting) return;
    const id = setInterval(() => {
      refreshOrders()
        .then((list) => {
          if (list.some((o) => o.status === "settled" || o.status === "refundable")) refreshBalances();
        })
        .catch(() => {});
    }, 2000);
    return () => clearInterval(id);
  }, [waiting, refreshOrders, refreshBalances]);

  const ringInput = useMemo(() => ({ dealers: lights, proving, settled }), [lights, proving, settled]);

  return (
    <>
      <p className="banner" role="note">
        <span className="banner-dot" aria-hidden="true" />
        Test version{config ? ` on ${NETWORK[config.network]}` : ""}: test tokens only, unaudited. Do not use real funds.
      </p>
      <div className="stage">
      <RingCanvas input={ringInput} anchor={panel} hero={hero} />
      <header className="top">
        <a className="wordmark" href="#top" aria-label="Keeler, top of page">
          <svg viewBox="0 0 40 24" width="34" height="20" aria-hidden="true">
            <ellipse cx="20" cy="12" rx="17" ry="7" fill="none" stroke="currentColor" strokeWidth="1.6" />
            <ellipse cx="20" cy="12" rx="12" ry="4.6" fill="none" stroke="currentColor" strokeWidth="1.6" opacity=".55" />
            <circle cx="31.5" cy="8.6" r="1.9" fill="#F4EBD6" />
          </svg>
          Keeler
        </a>
        <nav aria-label="On this page">
          <a href="#how">How it works</a>
          <a href="#privacy">Privacy</a>
          <a href="#limits">Limits</a>
        </nav>
      </header>
      <main id="top">
        <section className="hero" ref={hero} aria-labelledby="hero-title">
            <div className="intro">
              <h1 id="hero-title">A private lane for your trades.</h1>
              <p className="lede">
                Keeler asks several dealers for a price, drops any quote too far from the reference, and locks a
                private order for the best one. Assets and amounts stay private. If the dealer does not settle, you take
                your funds back.
              </p>
              <div className="pills">
                <a className="pill pill-soft" href="#how">
                  How a swap works
                </a>
                <a className="pill pill-outline" href="#privacy">
                  What stays private
                </a>
              </div>
              <p className="built">Built on Helius Privacy Rings, on Solana.</p>
            </div>
            <section className="panel" ref={panel} aria-label="Private swap">
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
        <Info />
      </main>
      </div>
      <footer className="foot">
        <p>
          Named after the Keeler Gap, a thin dark lane in Saturn's A ring. The small moon Daphnis keeps it open and
          raises waves on its edges as it passes.
        </p>
        <p className="muted">Unaudited test software, running with test tokens.</p>
      </footer>
    </>
  );
}
