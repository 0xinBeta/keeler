// The landing sections below the hero. Every claim here must hold for the code (docs/plan-lend.md, README.md);
// wording changes must not add or strengthen one.
import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { BUILD_NETWORK } from "../keeler/network";
import { RingCanvas } from "./RingCanvas";

const reduced = () => matchMedia("(prefers-reduced-motion: reduce)").matches;

/** The example pair's symbols: the lending market's vault when the service has named it, else the build network's. */
export type Pair = { collateral: string; debt: string };
export function pairOf(market?: { vaults: { collateral: { symbol: string }; debt: { symbol: string } }[] } | null, network = BUILD_NETWORK): Pair {
  const v = market?.vaults[0];
  if (v) return { collateral: v.collateral.symbol, debt: v.debt.symbol };
  return network === "mainnet" ? { collateral: "SOL", debt: "USDC" } : { collateral: "tSOL", debt: "tUSD" };
}

const SWAP_FACTS = [
  ["~1.5 s", "to prove your order, right in your browser"],
  ["0", "slippage. The quote you accept is the amount you get"],
  ["On-chain", "custody. Your funds sit in Solana programs, never in a Keeler account"],
  ["1 click", "to take your funds back if a dealer doesn't deliver"],
];

export function Facts({ facts = SWAP_FACTS, label = "Keeler in numbers" }: { facts?: string[][]; label?: string }) {
  return (
    <section className="facts" aria-label={label}>
      <ul>
        {facts.map(([fig, line]) => (
          <li key={fig}>
            <span className="fact-fig">{fig}</span>
            <span className="fact-line">{line}</span>
          </li>
        ))}
      </ul>
    </section>
  );
}

/* Small visuals drawn from the ring and its gap. Decorative; motion only without reduced-motion. */
const VIZ: Record<string, ReactNode> = {
  private: (
    <svg viewBox="0 0 160 80" aria-hidden="true">
      <ellipse cx="80" cy="40" rx="66" ry="24" className="v-ring" />
      <ellipse cx="80" cy="40" rx="52" ry="18" className="v-ring v-faint" />
      <g className="v-mask">
        <rect x="52" y="33" width="56" height="14" rx="7" className="v-chip" />
        <circle cx="64" cy="40" r="2.2" />
        <circle cx="74" cy="40" r="2.2" />
        <circle cx="84" cy="40" r="2.2" />
        <circle cx="94" cy="40" r="2.2" />
      </g>
      <circle cx="140" cy="31" r="3" className="v-moon v-orbit" />
    </svg>
  ),
  compete: (
    <svg viewBox="0 0 160 80" aria-hidden="true">
      <path d="M20 66 A70 46 0 0 1 140 66" className="v-ring v-faint" />
      <path d="M32 38 L80 64" className="v-line v-faint" />
      <path d="M128 38 L80 64" className="v-line v-faint" />
      <path d="M80 20 L80 64" className="v-line v-flow" />
      <circle cx="32" cy="38" r="4" className="v-dot-dim" />
      <circle cx="128" cy="38" r="4" className="v-dot-dim" />
      <circle cx="80" cy="20" r="5" className="v-dot v-pulse" />
      <circle cx="80" cy="64" r="3.5" className="v-moon" />
    </svg>
  ),
  band: (
    <svg viewBox="0 0 160 80" aria-hidden="true">
      <rect x="48" y="24" width="64" height="32" rx="6" className="v-band" />
      <line x1="80" y1="14" x2="80" y2="66" className="v-ref" />
      <circle cx="68" cy="40" r="4.5" className="v-dot" />
      <circle cx="92" cy="40" r="4.5" className="v-dot v-pulse" />
      <circle cx="22" cy="40" r="4.5" className="v-off" />
      <path d="M17 35l10 10M27 35l-10 10" className="v-x" />
    </svg>
  ),
  fixed: (
    <svg viewBox="0 0 160 80" aria-hidden="true">
      <path d="M14 52 C34 30 46 66 66 44 S96 28 112 50 S136 60 146 40" className="v-line v-faint" />
      <line x1="14" y1="34" x2="146" y2="34" className="v-line v-gold" />
      <circle cx="146" cy="34" r="4.5" className="v-dot v-pulse" />
    </svg>
  ),
  keys: (
    <svg viewBox="0 0 160 80" aria-hidden="true">
      <rect x="40" y="12" width="80" height="56" rx="8" className="v-frame" />
      <line x1="40" y1="24" x2="120" y2="24" className="v-frame" />
      <circle cx="49" cy="18" r="1.6" className="v-dot-dim" />
      <circle cx="55" cy="18" r="1.6" className="v-dot-dim" />
      <ellipse cx="80" cy="46" rx="26" ry="10" className="v-ring v-spin-slow" />
      <circle cx="80" cy="46" r="5" className="v-dot" />
      <path d="M85 46h12v5M93 46v4" className="v-line v-gold" />
    </svg>
  ),
  out: (
    <svg viewBox="0 0 160 80" aria-hidden="true">
      <path d="M52 22 A30 22 0 1 0 108 30" className="v-line v-gold v-flow" />
      <path d="M100 20 l9 10 -12 4" className="v-line v-gold" />
      <circle cx="80" cy="44" r="5" className="v-moon" />
    </svg>
  ),
};

export function Features() {
  const items: [string, string, string][] = [
    ["private", "Private where it counts", "Amounts and prices are encrypted on-chain. People can see that you traded, never how much or at what price."],
    ["compete", "Best price, by competition", "One request reaches every listed dealer. They quote without seeing each other, and the best signed quote wins."],
    ["band", "Checked before you commit", "Every quote is signed and checked against a reference price. Anything outside the band can't be accepted."],
    ["fixed", "No slippage, no surprises", "A quote is a fixed amount, signed by the dealer. You receive exactly that, or the trade doesn't happen."],
    ["keys", "Your keys, your browser", "Your private keys are created in your browser from your wallet. Keeler's servers never see them, and your order is proved on your device."],
    ["out", "Always a way out", "If a dealer doesn't settle, only you can take the funds back, as soon as the quote expires."],
  ];
  return (
    <section className="section" id="why" aria-labelledby="why-title">
      <h2 id="why-title" className="big">
        Everything a trade should be. <span className="gold">Nothing it should leak.</span>
      </h2>
      <ul className="features">
        {items.map(([k, title, text]) => (
          <li key={k} className="feature">
            <div className="viz">{VIZ[k]}</div>
            <h3>{title}</h3>
            <p>{text}</p>
          </li>
        ))}
      </ul>
    </section>
  );
}

const rows = ({ collateral: c, debt: d }: Pair) => [
  { label: "Wallet addresses", value: "7fUq…1rMk and DeaL…RtYk", secret: false },
  { label: "Assets", value: `${d} → ${c}`, secret: false },
  { label: "Amount sold", value: `1,250 ${d}`, secret: true },
  { label: "Amount received", value: `8.78 ${c}`, secret: true },
  { label: "Price", value: `142.37 ${d} per ${c}`, secret: true },
];

type CompareProps = {
  id: string;
  title: ReactNode;
  rows: { label: string; value: string; secret: boolean }[];
  labels: [string, string];
  caption: string;
};

export function ChainSees({ pair = pairOf() }: { pair?: Pair }) {
  return (
    <Compare
      id="privacy"
      title="What the chain sees"
      rows={rows(pair)}
      labels={["A normal swap", "On Keeler"]}
      caption="Who traded and which assets are public. How much and at what price are not."
    />
  );
}

function Compare({ id, title, rows, labels, caption }: CompareProps) {
  const [keeler, setKeeler] = useState(false);
  const [touched, setTouched] = useState(false);
  const box = useRef<HTMLDivElement>(null);

  // Flip by itself while on screen, until the reader uses the switch.
  useEffect(() => {
    if (touched || reduced()) return;
    let id = 0;
    const io = new IntersectionObserver(([e]) => {
      clearInterval(id);
      if (e?.isIntersecting) id = window.setInterval(() => setKeeler((k) => !k), 3600);
    });
    io.observe(box.current!);
    return () => {
      io.disconnect();
      clearInterval(id);
    };
  }, [touched]);

  const pick = (k: boolean) => {
    setTouched(true);
    setKeeler(k);
  };

  return (
    <section className="section" id={id} aria-labelledby={`${id}-title`}>
      <div className="split-head">
        <h2 id={`${id}-title`} className="big">
          {title}
        </h2>
        <div className="switch" role="group" aria-label="Compare">
          <button aria-pressed={!keeler} onClick={() => pick(false)}>
            {labels[0]}
          </button>
          <button aria-pressed={keeler} onClick={() => pick(true)}>
            {labels[1]}
          </button>
          <span className={`switch-thumb${keeler ? " on" : ""}`} aria-hidden="true" />
        </div>
      </div>
      <div className={`sees${keeler ? " is-keeler" : ""}`} ref={box}>
        <table>
          <caption className="sr-only">{`What the chain sees: ${keeler ? labels[1] : labels[0]}`}</caption>
          <tbody>
            {rows.map((r, i) => {
              const hidden = keeler && r.secret;
              return (
                <tr key={r.label} className={r.secret ? "secret" : undefined} style={{ ["--i" as string]: i }}>
                  <th scope="row">{r.label}</th>
                  <td className="sees-state">
                    <span className={`tag${hidden ? " tag-private" : ""}`}>{hidden ? "Private" : "Public"}</span>
                  </td>
                  <td className="sees-val">
                    <span className="val" aria-hidden={hidden}>
                      {r.value}
                    </span>
                    <span className="mask" aria-hidden={!hidden}>
                      <span className="mask-word">Encrypted</span>
                      <span aria-hidden="true">
                        <i />
                        <i />
                        <i />
                        <i />
                        <i />
                        <i />
                      </span>
                    </span>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
        <p className="sees-cap">{caption}</p>
      </div>
    </section>
  );
}

export function Steps() {
  const steps = [
    ["Ask", "One request goes to every listed dealer. Each answers with a signed quote."],
    ["Lock", "Keeler picks the best quote inside the band. Your browser proves the order and locks it so only that dealer can fill it."],
    ["Settle", "The dealer delivers and takes the order in one step, or not at all. If they don't, you take your funds back when the quote expires."],
  ];
  return (
    <section className="section" id="how" aria-labelledby="how-title">
      <h2 id="how-title" className="big">
        How a swap works
      </h2>
      <div className="track" aria-hidden="true">
        <span className="track-run" />
        <span className="node" />
        <span className="node" />
        <span className="node" />
      </div>
      <ol className="steps-3">
        {steps.map(([title, text], i) => (
          <li key={title} className="card">
            <span className="step-n">{i + 1}</span>
            <h3>{title}</h3>
            <p>{text}</p>
          </li>
        ))}
      </ol>
    </section>
  );
}

export function Audience() {
  const who = [
    ["Traders", "Your size and your price stay off the public record, so there is nothing for copy-traders to read."],
    ["Treasuries and DAOs", "Rebalance without publishing how much."],
    ["Funds and desks", "Fill at a fixed amount, with settlement that can't half-happen."],
  ];
  return (
    <section className="section" aria-labelledby="who-title">
      <h2 id="who-title" className="big">
        For traders who <span className="gold">don't broadcast.</span>
      </h2>
      <ul className="who">
        {who.map(([title, text], i) => (
          <li key={title} className="who-card">
            <svg viewBox="0 0 48 48" aria-hidden="true" className="who-mark">
              <circle cx="24" cy="24" r="21" className="v-ring v-faint" />
              <path d={["M24 3 A21 21 0 0 1 45 24", "M24 3 A21 21 0 1 1 3 24", "M24 3 A21 21 0 1 1 23.9 3"][i]} className="v-line v-gold" />
              <circle cx="24" cy="24" r="4" className="v-dot" />
            </svg>
            <h3>{title}</h3>
            <p>{text}</p>
          </li>
        ))}
      </ul>
    </section>
  );
}

export function Dealers() {
  const open = () => {
    const d = document.getElementById("faq-dealers") as HTMLDetailsElement | null;
    if (d) d.open = true;
  };
  return (
    <section className="section" id="dealers" aria-labelledby="dealers-title">
      <div className="dealers">
        <div>
          <h2 id="dealers-title" className="big">
            Quote flow, <span className="gold">privately.</span>
          </h2>
          <p className="dealers-text">
            Run a Keeler dealer: quote with your own prices and limits, sign every quote, keep the last look, and settle in
            one step on Solana.
          </p>
          <a className="pill pill-outline" href="#faq-dealers" onClick={open}>
            How dealers work
          </a>
        </div>
        <svg viewBox="0 0 360 220" className="dealers-viz" aria-hidden="true">
          <ellipse cx="180" cy="120" rx="160" ry="64" className="v-ring v-faint" />
          <ellipse cx="180" cy="120" rx="120" ry="46" className="v-ring" />
          {[
            [44, 96],
            [118, 64],
            [242, 64],
            [316, 96],
          ].map(([x, y], i) => (
            <g key={i}>
              <path d={`M${x} ${y} L180 150`} className={`v-line ${i === 2 ? "v-gold v-flow" : "v-faint"}`} />
              <rect x={x! - 16} y={y! - 10} width="32" height="20" rx="10" className={i === 2 ? "v-chip-on" : "v-chip"} />
              <path d={`M${x! - 5} ${y} l3 3 6 -6`} className="v-tick" />
            </g>
          ))}
          <circle cx="180" cy="150" r="7" className="v-moon v-pulse" />
        </svg>
      </div>
    </section>
  );
}

/* ---------------- Keeler Lend. Every claim here must hold for docs/plan-lend.md's design. ---------------- */

const LEND_VIZ: Record<string, ReactNode> = {
  earn: (
    <svg viewBox="0 0 160 80" aria-hidden="true">
      <ellipse cx="80" cy="44" rx="62" ry="22" className="v-ring v-faint" />
      <path d="M22 62 C52 60 74 50 92 36 S124 16 140 14" className="v-line v-gold" />
      <circle cx="140" cy="14" r="4.5" className="v-dot v-pulse" />
      <circle cx="36" cy="60" r="3" className="v-dot-dim" />
      <circle cx="64" cy="54" r="3" className="v-dot-dim" />
      <circle cx="92" cy="36" r="3" className="v-dot-dim" />
    </svg>
  ),
  exit: (
    <svg viewBox="0 0 160 80" aria-hidden="true">
      <rect x="30" y="20" width="100" height="40" rx="20" className="v-band" />
      <rect x="30" y="20" width="72" height="40" rx="20" className="v-chip" />
      <path d="M112 40 h34 M138 32 l8 8 -8 8" className="v-line v-gold v-flow" />
      <circle cx="116" cy="40" r="4" className="v-moon" />
    </svg>
  ),
  borrow: (
    <svg viewBox="0 0 160 80" aria-hidden="true">
      <rect x="20" y="34" width="120" height="12" rx="6" className="v-chip" />
      <rect x="20" y="34" width="96" height="12" rx="6" className="v-band" />
      <line x1="116" y1="24" x2="116" y2="56" className="v-line v-gold" />
      <line x1="122" y1="26" x2="122" y2="54" className="v-ref" />
      <circle cx="116" cy="24" r="3.5" className="v-dot v-pulse" />
    </svg>
  ),
  trim: (
    <svg viewBox="0 0 160 80" aria-hidden="true">
      <path d="M24 56 L60 50 L92 30 L108 46 L140 44" className="v-line v-faint" />
      <line x1="20" y1="40" x2="144" y2="40" className="v-ref" />
      <circle cx="92" cy="30" r="4.5" className="v-off" />
      <path d="M92 36 L106 44" className="v-line v-gold v-flow" />
      <circle cx="108" cy="46" r="4.5" className="v-dot" />
    </svg>
  ),
  hidden: (
    <svg viewBox="0 0 160 80" aria-hidden="true">
      <ellipse cx="80" cy="40" rx="66" ry="24" className="v-ring v-spin-slow" />
      <g className="v-mask">
        <rect x="46" y="33" width="68" height="14" rx="7" className="v-chip" />
        <circle cx="60" cy="40" r="2.2" />
        <circle cx="72" cy="40" r="2.2" />
        <circle cx="84" cy="40" r="2.2" />
        <circle cx="96" cy="40" r="2.2" />
      </g>
    </svg>
  ),
  close: (
    <svg viewBox="0 0 160 80" aria-hidden="true">
      <rect x="44" y="16" width="72" height="48" rx="10" className="v-frame" />
      <path d="M66 40 l10 10 20 -20" className="v-tick" />
      <circle cx="132" cy="22" r="3.5" className="v-moon v-orbit" />
    </svg>
  ),
};

export function LendFeatures({ market, onOpen, pair = pairOf() }: { market: { pools: { supplyAprBps: number; borrowAprBps: number; asset: { symbol: string } }[] } | null; onOpen: (t: "earn" | "borrow") => void; pair?: Pair }) {
  const pool = market?.pools[0];
  const items: [string, string, string][] = [
    ["earn", "Earn a floating rate", "Supply to one shared pool. Borrowers pay interest into it, and the value of your share grows as that interest accrues."],
    ["exit", "Out when you want", "Withdraw any time the pool has idle funds. No lock-up period, no term, no waiting for a maturity date."],
    ["borrow", "Borrow up to 80%", `Lock ${pair.collateral} and borrow ${pair.debt} against up to 80% of its value. Repay whenever you like; a loan has no end date.`],
    ["trim", "A liquidation that leaves you standing", "Past 85%, collateral is sold to bring your loan back to 85% or less, and the rest stays yours. The penalty is a fixed 1%, which pays the keeper for doing the liquidation."],
    ["hidden", "Hidden from the chain", "Your deposit, collateral, debt, health and liquidation price stay off the public chain. Only you and the pool's keeper can read them."],
    ["close", "Yours to close", "Once a position has no debt, your browser proves the close and you take your collateral back yourself. No keeper needed."],
  ];
  return (
    <section className="section" id="lend" aria-labelledby="lend-title">
      <div className="lend-hero">
        <div>
          <p className="kicker">Keeler Lend</p>
          <h2 id="lend-title" className="big">
            Jupiter-style lending. <span className="gold">The chain never sees your position's size.</span>
          </h2>
          <p className="dealers-text">
            A shared pool with a floating rate, and vaults that let you borrow against your collateral with no term. It works like
            the lending markets you know, except the public chain never sees how much you deposit, borrow or hold.
          </p>
          <div className="pills">
            <button className="pill pill-gold" onClick={() => onOpen("earn")}>
              Start earning
            </button>
            <button className="pill pill-outline" onClick={() => onOpen("borrow")}>
              Borrow against {pair.collateral}
            </button>
          </div>
        </div>
        <div className="lend-rate" aria-live="off">
          <svg viewBox="0 0 200 200" aria-hidden="true" className="lend-rate-ring">
            <circle cx="100" cy="100" r="86" className="v-ring v-faint" />
            <circle cx="100" cy="100" r="86" className="v-ring lend-rate-arc" pathLength={100} strokeDasharray="72 100" />
          </svg>
          <div className="lend-rate-body">
            <span className="fact-fig">{pool ? `${(pool.supplyAprBps / 100).toFixed(2)}%` : "—"}</span>
            <span className="muted">{pool ? `${pool.asset.symbol} supply APR now` : "Reading the pool…"}</span>
            {pool && <span className="muted small">Borrow at {(pool.borrowAprBps / 100).toFixed(2)}%</span>}
          </div>
        </div>
      </div>
      <ul className="features">
        {items.map(([k, title, text]) => (
          <li key={k} className="feature">
            <div className="viz">{LEND_VIZ[k]}</div>
            <h3>{title}</h3>
            <p>{text}</p>
          </li>
        ))}
      </ul>
    </section>
  );
}

const lendRows = ({ collateral: c, debt: d }: Pair) => [
  { label: "Wallet address", value: "9xQe…4kLm", secret: false },
  { label: "Vault", value: `${c} → ${d}`, secret: false },
  { label: "Collateral", value: `1,200 ${c}`, secret: true },
  { label: "Debt", value: `118,400 ${d}`, secret: true },
  { label: "Health", value: "69.3% loan-to-value", secret: true },
  { label: "Liquidation price", value: `116.08 ${d} per ${c}`, secret: true },
];

export function LendPrivacy({ pair = pairOf() }: { pair?: Pair }) {
  return (
    <Compare
      id="lend-privacy"
      title={
        <>
          Your liquidation price <span className="gold">is nobody's business.</span>
        </>
      }
      rows={lendRows(pair)}
      labels={["A public market", "On Keeler"]}
      caption="On a public lending market anyone can list every position and its liquidation price. Here they can't."
    />
  );
}

export function LendLiquidation({ pair = pairOf() }: { pair?: Pair }) {
  const steps = [
    ["Watched", "The keeper checks every position against the oracle price, privately. Nobody else can see how close yours is."],
    ["Trimmed", "Past 85%, it repays part of your debt and sells only enough collateral to bring you back to 85%. Past 90%, it repays the whole loan."],
    ["Capped", "The penalty is a fixed 1%, which pays the keeper for doing the liquidation. The rest of your collateral stays yours."],
  ];
  return (
    <section className="section" id="liquidation" aria-labelledby="liq-title">
      <h2 id="liq-title" className="big">
        How liquidation works: <span className="gold">a trim, not a wipe-out.</span>
      </h2>
      <div className="liq-demo" aria-hidden="true">
        <p className="liq-labels">A position's loan-to-value as the {pair.collateral} price falls, crosses 85%, and is trimmed back.</p>
        <div className="health-bar liq-bar">
          <span className="health-fill" />
          <span className="health-mark" style={{ left: "80%" }} />
          <span className="health-mark health-mark-lt" style={{ left: "85%" }} />
        </div>
        <div className="health-scale muted">
          <span>80% borrow limit</span>
          <span className="lt">85% liquidation</span>
        </div>
      </div>
      <ol className="steps-3">
        {steps.map(([title, text], i) => (
          <li key={title} className="card">
            <span className="step-n">{i + 1}</span>
            <h3>{title}</h3>
            <p>{text}</p>
          </li>
        ))}
      </ol>
    </section>
  );
}

export function LendNumbers() {
  return (
    <div className="section section-facts">
      <Facts
        label="Keeler Lend in numbers"
        facts={[
          ["80%", "of your collateral's value you can borrow, with no term"],
          ["85%", "loan-to-value where liquidation starts; the rest of your collateral stays yours"],
          ["1%", "penalty on a liquidation, fixed, and all of it pays for the liquidation"],
          ["0", "amounts on the public chain: deposit, collateral, debt, health and liquidation price stay hidden"],
        ]}
      />
    </div>
  );
}

const lendFaq = ({ collateral: c, debt: d }: Pair): [string, string, string?][] => [
  ["What does “Jupiter-style” mean?", "Keeler Lend follows the same shape as Jupiter Lend: one shared pool per token with a floating rate set by how much of it is borrowed, and vaults where you borrow against collateral with a borrow limit, a liquidation threshold and a penalty. Keeler is not affiliated with Jupiter and uses none of its code."],
  ["What is visible when I lend or borrow?", "Your wallet address, which pool or vault you used, when, and which kind of operation it was; a borrow and a collateral withdrawal look the same. If a position is liquidated, that is public too, with the price used. Your amounts, health and liquidation price are not. People using the same pool can estimate how much of it is borrowed over time, and may notice a very large operation."],
  ["Who runs the pool?", "A keeper service run by Keeler. It holds the pool's private records, so it can see the amounts in every deposit and position. It puts operations in order and performs liquidations. By design it cannot take your principal: your funds only move with your signed request or your wallet's signature, or by a liquidation under the vault's rules. On this devnet beta the proving keys are test keys and the keeper sets the test price, so that guarantee waits for mainnet and its security review."],
  ["What if the keeper is offline?", "Deposits, borrowing, repayments and withdrawals wait until it is back, and your funds stay where they are. A deposit or repayment it hasn't taken in can be taken back once the request expires. A position that could not be topped up while it was offline may be liquidated when it returns. A position with no debt can be closed without the keeper."],
  ["How does liquidation work?", `Each vault has a liquidation threshold: 85% loan-to-value for ${c} → ${d}. Past it, the keeper repays part of your debt and sells only enough collateral, at a price set from the oracle, to bring you back to 85%, plus a 1% penalty that pays the keeper for the liquidation. If a position gets past 90% before that happens, the whole loan is repaid the same way and the rest of the collateral stays yours.`],
  ["When can I withdraw from Earn?", "Any time the pool has idle funds. When most of the pool is borrowed, the rate climbs to bring repayments and new deposits in, and no borrow may take the pool above 95% used."],
  ["How are lending rates set?", "By utilisation, the share of the pool that is borrowed. The borrow rate rises gently up to 88% and steeply after it. Suppliers share the borrowers' interest, less a 10% fee. Collateral is not lent out, so it does not earn."],
];

const FAQ: [string, string, string?][] = [
  ["What stays private?", "The amounts and the price. The two wallet addresses, the assets, the time, and the fact that a swap happened are public. The dealers you ask see your request so they can quote it."],
  ["Do I need a special wallet?", "No. Your Solana wallet signs a message to create your private keys in the browser. It asks twice, to check your wallet signs consistently. Signing moves no funds."],
  ["Who holds my funds?", "On-chain programs on Solana: the Privacy Rings pool for your private balance, and Keeler's swap program while an order is locked. They are never in a Keeler account."],
  ["Where are proofs made?", "Your order's proof is made in your browser. The pool's own proof is made inside Helius' attested secure enclave; what your browser sends it is encrypted so only the enclave can read it."],
  ["What if the dealer doesn't settle?", "Your order stays locked until the quote expires, ten minutes at most. Then you take the funds back yourself, in one click."],
  ["Can a dealer decline?", "Yes. If the price moves before it settles, a dealer can pass. Nothing is lost: you take your funds back at expiry and ask again."],
  ["What does it cost?", "Solana network fees and the dealer's spread, which is already inside the quote you see. There is no protocol fee."],
  ["What if I clear my browser?", "Your orders can be found again from the chain with the same wallet. The browser test wallet keeps its key in this browser, so keep the site data while you use it."],
  ["How do dealers work?", "A dealer runs Keeler's dealer software with its own inventory, prices and limits, signs every quote, and settles through the same on-chain program. During the beta, Keeler lists the dealers.", "faq-dealers"],
  ["Is Keeler on mainnet?", "Not yet. Keeler is in beta on Solana devnet with test tokens, so you can try every step for free. It has not been audited yet and uses test proving keys; mainnet follows the security review."],
];

const MAINNET_LEND: [string, string] = [
  "Is Keeler on mainnet?",
  "Not yet. Swaps and lending are in beta on Solana devnet with test tokens, so you can try every step for free. Keeler has not been audited yet and uses test proving keys; mainnet follows the security review.",
];

/** The answers a mainnet build gives instead: no test tokens, test keys, test price or browser test wallet there. */
const onMainnet = ({ collateral: c, debt: d }: Pair): Record<string, string> => ({
  "Is Keeler on mainnet?": "Yes. This site runs on Solana mainnet, with real funds. Keeler is new, so start with small amounts.",
  "What if I clear my browser?": "Your orders can be found again from the chain with the same wallet.",
  "Who runs the pool?": "A keeper service run by Keeler. It holds the pool's private records, so it can see the amounts in every deposit and position. It puts operations in order and performs liquidations. By design it cannot take your principal: your funds only move with your signed request or your wallet's signature, or by a liquidation under the vault's rules, at the price the Pyth oracle publishes.",
  "How does liquidation work?": `Each vault has a liquidation threshold: 85% loan-to-value for ${c} → ${d}. Past it, the keeper repays part of your debt and sells only enough collateral, at a price set from the oracle, to bring you back to 85%, plus a 1% penalty that pays the keeper for the liquidation. If a position gets past 90% before that happens, the whole loan is repaid the same way and the rest of the collateral stays yours.`,
});

export function Faq({ lend = false, network = BUILD_NETWORK, pair = pairOf(null, network) }: { lend?: boolean; network?: string; pair?: Pair }) {
  const all = lend ? [...FAQ.slice(0, -1), ...lendFaq(pair), MAINNET_LEND] : FAQ;
  const mainnet = onMainnet(pair);
  const list = network === "mainnet" ? all.map(([q, a, id]) => [q, mainnet[q] ?? a, id] as const) : all;
  return (
    <section className="section" id="faq" aria-labelledby="faq-title">
      <div className="faq-grid">
        <h2 id="faq-title" className="big">
          Questions, answered
        </h2>
        <div className="faq">
          {list.map(([q, a, id]) => (
            <details key={q} id={id}>
              <summary>
                <span>{q}</span>
                <span className="faq-icon" aria-hidden="true" />
              </summary>
              <p>{a}</p>
            </details>
          ))}
        </div>
      </div>
    </section>
  );
}

export function Closing({ onStart }: { onStart: () => void }) {
  return (
    <section className="closing" aria-labelledby="closing-title">
      <RingCanvas mode="arc" />
      <div className="closing-body">
        <h2 id="closing-title">Your next swap doesn't have to be public.</h2>
        <button className="pill pill-gold pill-big" onClick={onStart}>
          Start a swap
        </button>
      </div>
    </section>
  );
}
