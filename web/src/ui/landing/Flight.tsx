// The landing flight: one sticky full-viewport scene whose camera follows normal, reversible document scrolling, with
// a chapter at each of `flightStops`. Ported from the design handoff's index.html and landing.js.
import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import type { Config } from "../../keeler/index";
import type { Route } from "../App";
import type { Pair } from "../Landing";
import type { Market } from "../Lend";
import daphnisUrl from "../orbit/media/daphnis-relief.webp";
import { flightStops } from "../orbit/model";
import type { Flight as Scene, RendererKind } from "../orbit/scene";
import { NetworkPill } from "../shared";
import { CollateralSlider, PriceStress, PrivacySwitch, SupplyExample } from "./Instruments";

type Props = { config: Config | null; market: Market | null; lend: boolean; open: (route: Route) => void; network?: string; pair: Pair };
type Chapter = { key: string; stop: number; title: ReactNode; body: ReactNode };

const Arrow = ({ down = false }: { down?: boolean }) => <span className={`kl-arrow${down ? " kl-down" : ""}`} aria-hidden="true" />;
const clamp = (v: number) => Math.max(0, Math.min(1, v));

function chapters({ lend, open, network, pair, market }: Props, cue: ReactNode): Chapter[] {
  const { collateral: c, debt: d } = pair;
  const test = network !== "mainnet";
  const go = (route: Route, label: string, primary = false) => (
    <button type="button" className={primary ? "kl-primary" : "kl-text"} onClick={() => open(route)}>
      {label} <Arrow />
    </button>
  );
  if (!lend) {
    return [
      {
        key: "home",
        stop: flightStops[0],
        title: (
          <>
            Swap in <br />
            <em>private.</em>
          </>
        ),
        body: (
          <>
            <p className="kl-lead">
              Get signed quotes from the listed dealers and settle on Solana. The chain sees that you traded, not how much or at
              what price.
            </p>
            <div className="kl-actions">{go("swap", "Start a swap", true)}</div>
            {cue}
          </>
        ),
      },
      {
        key: "markets",
        stop: flightStops[5],
        title: (
          <>
            Try private <br />
            <em>swaps.</em>
          </>
        ),
        body: (
          <>
            <p className="kl-lead">{test ? "Try it with free test tokens." : "Your next swap doesn't have to be public."}</p>
            <div className="kl-actions">{go("swap", "Start a swap", true)}</div>
          </>
        ),
      },
    ];
  }
  return [
    {
      key: "home",
      stop: flightStops[0],
      title: (
        <>
          Lend in <br />
          <em>private.</em>
        </>
      ),
      body: (
        <>
          <p className="kl-lead">
            Supply {d} or borrow against {c} on Solana. Your lending amounts stay hidden from the public chain.
          </p>
          <div className="kl-actions">
            {go("earn", `Supply ${d}`, true)}
            {go("borrow", `Borrow ${d}`)}
          </div>
          {cue}
        </>
      ),
    },
    {
      key: "supply",
      stop: flightStops[1],
      title: (
        <>
          Put {d} <br />
          to work.
        </>
      ),
      body: (
        <>
          <p className="kl-lead">Supply from your private balance and earn variable interest as borrowers pay into the pool.</p>
          <div className="kl-slot" role="group" aria-label="Supply pool example">
            <SupplyExample pair={pair} market={market} />
          </div>
          {go("earn", "Open the earn desk")}
        </>
      ),
    },
    {
      key: "borrow",
      stop: flightStops[2],
      title: (
        <>
          Borrow against <br />
          your {c}.
        </>
      ),
      body: (
        <>
          <p className="kl-lead">
            Post {c} as collateral to draw {d} into your private balance.
          </p>
          <div className="kl-slot" role="group" aria-label="Borrowing position example">
            <CollateralSlider pair={pair} market={market} />
          </div>
          {go("borrow", "Open the borrow desk")}
        </>
      ),
    },
    {
      key: "risk",
      stop: flightStops[3],
      title: (
        <>
          Know your <br />
          <em>liquidation price.</em>
        </>
      ),
      body: (
        <>
          <p className="kl-lead">
            A falling {c} price can put your collateral at risk. Review the threshold before you draw {d}.
          </p>
          <div className="kl-slot" role="group" aria-label="Liquidation risk example">
            <PriceStress pair={pair} market={market} />
          </div>
          {go("borrow", "Review a position")}
        </>
      ),
    },
    {
      key: "privacy",
      stop: flightStops[4],
      title: (
        <>
          Private amounts. <br />
          <em>Public activity.</em>
        </>
      ),
      body: (
        <>
          <p className="kl-lead">
            Wallet addresses, the vault and transaction timing are public. Lending amounts are visible to you and to Keeler's
            keeper service.
          </p>
          <div className="kl-slot" role="group" aria-label="Position visibility example">
            <PrivacySwitch pair={pair} />
          </div>
        </>
      ),
    },
    {
      key: "markets",
      stop: flightStops[5],
      title: (
        <>
          Try private <br />
          <em>lending.</em>
        </>
      ),
      body: (
        <>
          <p className="kl-lead">{test ? "Try it with free test tokens." : "Supply or borrow from your private balance on Solana."}</p>
          <div className="kl-actions">
            {go("earn", `Supply ${d}`, true)}
            {go("borrow", `Borrow ${d}`)}
          </div>
          <button type="button" className="kl-swap-link" onClick={() => open("swap")}>
            Explore private swaps <Arrow />
          </button>
        </>
      ),
    },
  ];
}

export function Flight(props: Props) {
  const journey = useRef<HTMLDivElement>(null);
  const stage = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const scene = useRef<Scene | null>(null);
  const progress = useRef(0);
  const pending = useRef<number | null>(null);
  const [active, setActive] = useState(0);
  const [renderer, setRenderer] = useState<RendererKind | null>(null);

  const goTo = (index: number, smooth: boolean, focus: boolean) => {
    const j = journey.current;
    const s = stage.current;
    const stop = list[index]?.stop;
    if (!j || !s || stop === undefined) return;
    pending.current = focus ? index : null;
    const range = Math.max(1, j.offsetHeight - s.offsetHeight);
    const reduce = matchMedia("(prefers-reduced-motion: reduce)").matches;
    window.scrollTo({ top: window.scrollY + j.getBoundingClientRect().top + stop * range, behavior: smooth && !reduce ? "smooth" : "instant" });
  };

  const cue = (
    <button type="button" className="kl-cue" onClick={() => goTo(1, true, true)}>
      Scroll to fly through <Arrow down />
    </button>
  );
  const list = chapters(props, cue);
  const stops = list.map((c) => c.stop).join();

  useEffect(() => {
    let cancelled = false;
    import("../orbit/scene").then(
      ({ mountFlight }) => {
        if (cancelled || !canvas.current) return;
        scene.current = mountFlight(canvas.current, { onRenderer: setRenderer });
        scene.current.setProgress(progress.current);
      },
      (e) => {
        console.error("Orbit scene:", e);
        if (!cancelled) setRenderer("still");
      },
    );
    return () => {
      cancelled = true;
      scene.current?.destroy();
      scene.current = null;
    };
  }, []);

  useEffect(() => {
    const j = journey.current!;
    const s = stage.current!;
    const points = stops.split(",").map(Number);
    const thresholds = points.slice(1).map((next, i) => (next + points[i]!) / 2);
    const motion = matchMedia("(prefers-reduced-motion: reduce)");
    let frame = 0;
    let relief = false;

    const update = () => {
      frame = 0;
      const range = Math.max(1, j.offsetHeight - s.offsetHeight);
      const p = clamp(-j.getBoundingClientRect().top / range);
      const index = thresholds.filter((t) => p >= t).length;
      setActive(index);
      s.style.setProperty("--kl-shift", `${Math.max(-12, Math.min(12, (points[index]! - p) * 120))}px`);
      const passage = (p - 0.69) / 0.137;
      const visible = passage > 0 && passage < 1 && !motion.matches;
      if (visible && !relief) {
        relief = true;
        s.style.setProperty("--kl-relief", `url("${daphnisUrl}")`);
      }
      s.style.setProperty("--kl-relief-opacity", visible ? String(Math.sin(passage * Math.PI) * 0.83) : "0");
      if (visible) {
        s.style.setProperty("--kl-relief-x", `${42 - 61 * passage}%`);
        s.style.setProperty("--kl-relief-y", `${-10 + 20 * passage}%`);
        s.style.setProperty("--kl-relief-roll", `${-22 + 17 * passage}deg`);
      }
      if (p !== progress.current) {
        progress.current = p;
        scene.current?.setProgress(p);
      }
      if (pending.current === index && Math.abs(p - points[index]!) * range <= 1) {
        pending.current = null;
        s.querySelector<HTMLElement>(`[data-index="${index}"] :is(h1, h2)`)?.focus({ preventScroll: true });
      }
    };
    const request = () => {
      if (!frame) frame = requestAnimationFrame(update);
    };
    const cancelFocus = () => {
      pending.current = null;
    };
    window.addEventListener("scroll", request, { passive: true });
    window.addEventListener("resize", request);
    window.addEventListener("wheel", cancelFocus, { passive: true });
    window.addEventListener("touchstart", cancelFocus, { passive: true });
    motion.addEventListener("change", request);
    update();
    return () => {
      cancelAnimationFrame(frame);
      window.removeEventListener("scroll", request);
      window.removeEventListener("resize", request);
      window.removeEventListener("wheel", cancelFocus);
      window.removeEventListener("touchstart", cancelFocus);
      motion.removeEventListener("change", request);
    };
  }, [stops]);

  useEffect(() => {
    const id = location.hash.slice(1);
    if (id) document.getElementById(id)?.scrollIntoView();
  }, []);

  const current = list[active] ?? list[0]!;
  return (
    <div className="kl-journey" ref={journey} data-chapters={list.length}>
      <div className="kl-stage" ref={stage} data-chapter={current.key} data-renderer={renderer ?? undefined}>
        <div className="kl-cosmos" aria-hidden="true">
          <div className="kl-relief" />
          <canvas ref={canvas} />
        </div>
        <div className="kl-exposure" aria-hidden="true" />
        <header className="kl-mast">
          <button type="button" className="kl-brand" aria-label="Keeler, back to the start" onClick={() => goTo(0, true, true)}>
            <span className="kl-brand-mark" aria-hidden="true" />
            Keeler
          </button>
          <nav className="kl-mast-links" aria-label="Keeler">
            <NetworkPill config={props.config} />
            <button type="button" className="kl-mast-action" onClick={() => props.open(props.lend ? "earn" : "swap")}>
              Open app <Arrow />
            </button>
          </nav>
        </header>
        <div className="kl-chapters">
          {list.map((c, i) => {
            const Heading = i === 0 ? "h1" : "h2";
            return (
              <section
                key={c.key}
                className={`kl-chapter kl-${c.key}`}
                data-index={i}
                data-panel={c.key}
                data-active={i === active}
                aria-labelledby={`kl-${c.key}-title`}
                onFocus={(e) => {
                  if (i !== active && pending.current !== i && e.target !== e.currentTarget.querySelector("h1, h2")) goTo(i, false, false);
                }}
              >
                <div className="kl-copy">
                  <Heading id={`kl-${c.key}-title`} tabIndex={-1}>
                    {c.title}
                  </Heading>
                  {c.body}
                </div>
              </section>
            );
          })}
        </div>
      </div>
    </div>
  );
}
