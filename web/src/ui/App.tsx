import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Balance, Config, Keeler, LendAccount, Order } from "../keeler/index";
import { Audience, ChainSees, Closing, Dealers, Facts, Faq, Features, LendFeatures, LendLiquidation, LendNumbers, LendPrivacy, Steps, pairOf } from "./Landing";
import { LendActivity } from "./Lend";
import type { Market } from "./Lend";
import { Orders } from "./Orders";
import { Panel } from "./Panel";
import type { Tab } from "./Panel";
import type { DealerLight } from "./ring";
import { RingCanvas } from "./RingCanvas";
import { errorText, Logo, NetworkPill } from "./shared";

const OPEN: Order["status"][] = ["pending", "locked"];

/** The app's views, each with its own hash; any other hash shows the landing. */
export const ROUTES = ["earn", "borrow", "swap", "balances", "activity"] as const;
export type Route = (typeof ROUTES)[number];
const routeOf = (hash: string): Route | null => ROUTES.find((r) => `#${r}` === hash) ?? null;
const isTab = (r: Route | null): r is Tab => r !== null && r !== "activity";

/** The route the location's hash names, or null for the landing. */
function useRoute(): Route | null {
  const [route, setRoute] = useState(() => routeOf(location.hash));
  useEffect(() => {
    const read = () => setRoute(routeOf(location.hash));
    window.addEventListener("hashchange", read);
    return () => window.removeEventListener("hashchange", read);
  }, []);
  return route;
}

/** Shows a route by setting the hash, so the back button walks the views. */
export const go = (route: Route) => {
  location.hash = route;
};

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
  const heroSlot = useRef<HTMLDivElement>(null);
  const appHero = useRef<HTMLDivElement>(null);
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

  // Lending: the market is public and read from the start; the account once the wallet is unlocked.
  const lend = keeler.lend;
  const [market, setMarket] = useState<Market | null>(null);
  const [account, setAccount] = useState<LendAccount | null>(null);
  const lendSeen = useRef(new Map<string, string>());
  const refreshMarket = useCallback(() => (lend ? lend.pools().then(setMarket) : Promise.resolve()), [lend]);
  // Reads the page starts on its own never ask the wallet; the reader asks for that (`prompt`).
  const refreshAccount = useCallback(async (prompt = false) => {
    if (!lend) return;
    const a = await lend.lendAccount({ prompt });
    setAccount(a);
    // A keeper fill or a liquidation changes private balances too.
    let changed = false;
    for (const o of a.operations) {
      if (lendSeen.current.get(o.id) !== o.status) changed = true;
      lendSeen.current.set(o.id, o.status);
    }
    if (changed) refreshBalances().catch(() => {});
  }, [lend, refreshBalances]);
  // After an action: market and account each on their own, so a keeper or price outage still shows the account.
  const refreshLend = useCallback(async (prompt?: boolean) => {
    const [, a] = await Promise.allSettled([refreshMarket(), refreshAccount(prompt)]);
    if (a.status === "rejected") throw a.reason;
  }, [refreshMarket, refreshAccount]);

  useEffect(() => {
    refreshMarket().catch(() => {});
    const id = setInterval(() => refreshMarket().catch(() => {}), 60_000);
    return () => clearInterval(id);
  }, [refreshMarket]);
  useEffect(() => {
    if (ready) refreshLend().catch(() => {});
  }, [ready, refreshLend]);

  // Poll quickly while an operation is in flight, slower while a loan is open (the keeper may liquidate it).
  // A fill that just landed counts as pending for two minutes: the keeper and the indexer may not show its new note yet.
  const lendPending = account?.operations.some((o) => o.status === "pending" || (o.status === "landed" && account.asOf - o.createdAt < 120)) ?? false;
  const owing = account?.positions.some((x) => x.debt > 0n) ?? false;
  // Never overlapping, backing off on errors (the swap poller's loop); the account only, the market has its own timer.
  useEffect(() => {
    if (!lendPending && !owing) return;
    const base = lendPending ? 3000 : 10_000;
    let stopped = false;
    let delay = base;
    let timer = 0;
    const tick = async () => {
      try {
        await refreshAccount();
        delay = base;
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
  }, [lendPending, owing, refreshAccount]);

  const route = useRoute();
  const view = route ? "app" : "landing";
  const [tab, setTab] = useState<Tab>(isTab(route) ? route : "swap");
  if (isTab(route) && route !== tab) setTab(route);
  const ringInput = useMemo(() => ({ dealers: lights, proving, settled }), [lights, proving, settled]);
  const startSwap = useCallback(() => go("swap"), []);

  const shownView = useRef(view);
  useEffect(() => {
    if (shownView.current === view) return;
    shownView.current = view;
    const anchor = isTab(route) ? null : document.getElementById(location.hash.slice(1));
    if (anchor) anchor.scrollIntoView();
    else window.scrollTo(0, 0);
    if (view === "app" && !panel.current?.contains(document.activeElement)) panel.current?.focus({ preventScroll: true });
  }, [view, route]);

  return (
    <>
      <a className="skip" href="#swap">
        Skip to the swap
      </a>
      <div className="stage">
        {view === "landing" ? (
          <RingCanvas key="landing" mode="hero" input={ringInput} anchor={heroSlot} hero={hero} />
        ) : (
          <RingCanvas key="app" mode="hero" input={ringInput} anchor={panel} hero={appHero} />
        )}
        <header className="top">
          <a className="wordmark" href="#top" aria-label="Keeler, top of page">
            <Logo />
            Keeler
          </a>
          <nav aria-label="On this page">
            <a href="#how">How it works</a>
            <a href="#privacy">Privacy</a>
            {lend && <a href="#lend">Earn & Borrow</a>}
            <a href="#dealers">Dealers</a>
            <a href="#faq">FAQ</a>
          </nav>
          <NetworkPill config={config} />
        </header>
        <main id="top">
          {view === "landing" && (
            <>
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
                <div ref={heroSlot} aria-hidden="true" />
              </section>
              <Facts />
              <Features />
              <ChainSees pair={pairOf(market)} />
              <Steps />
              {lend && (
                <>
                  <LendFeatures market={market} onOpen={go} pair={pairOf(market)} />
                  <LendPrivacy pair={pairOf(market)} />
                  <LendLiquidation pair={pairOf(market)} />
                  <LendNumbers />
                </>
              )}
              <Audience />
              <Dealers />
              <Faq lend={!!lend} pair={pairOf(market)} />
              <Closing onStart={startSwap} />
            </>
          )}
          <div className="app-view" hidden={view !== "app"}>
            <div className="hero app-hero" ref={appHero}>
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
                    tab={tab}
                    setTab={go}
                    market={market}
                    account={account}
                    refreshLend={refreshLend}
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
            </div>
            <div id="activity">
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
              {ready && lend && (
                <LendActivity
                  lend={lend}
                  balances={balances}
                  market={market}
                  account={account}
                  chainOffset={chainOffset}
                  refreshLend={refreshLend}
                  refreshBalances={refreshBalances}
                />
              )}
            </div>
          </div>
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
