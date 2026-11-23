// The landing sections below the hero. Copy follows docs/plan-landing.md; every claim there was checked
// against the code, so wording changes here must not add or strengthen one.
import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { RingCanvas } from "./RingCanvas";

const reduced = () => matchMedia("(prefers-reduced-motion: reduce)").matches;

export function Facts() {
  const facts = [
    ["~1.5 s", "to prove your order, right in your browser"],
    ["0", "slippage. The quote you accept is the amount you get"],
    ["On-chain", "custody. Your funds sit in Solana programs, never in a Keeler account"],
    ["1 click", "to take your funds back if a dealer doesn't deliver"],
  ];
  return (
    <section className="facts" aria-label="Keeler in numbers">
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

const ROWS = [
  { label: "Wallet addresses", value: "7fUq…1rMk and DeaL…RtYk", secret: false },
  { label: "Assets", value: "tUSD → tSOL", secret: false },
  { label: "Amount sold", value: "1,250 tUSD", secret: true },
  { label: "Amount received", value: "8.78 tSOL", secret: true },
  { label: "Price", value: "142.37 tUSD per tSOL", secret: true },
];

export function ChainSees() {
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
    <section className="section" id="privacy" aria-labelledby="privacy-title">
      <div className="split-head">
        <h2 id="privacy-title" className="big">
          What the chain sees
        </h2>
        <div className="switch" role="group" aria-label="Compare">
          <button aria-pressed={!keeler} onClick={() => pick(false)}>
            A normal swap
          </button>
          <button aria-pressed={keeler} onClick={() => pick(true)}>
            On Keeler
          </button>
          <span className={`switch-thumb${keeler ? " on" : ""}`} aria-hidden="true" />
        </div>
      </div>
      <div className={`sees${keeler ? " is-keeler" : ""}`} ref={box}>
        <table>
          <caption className="sr-only">{keeler ? "What the chain sees on Keeler" : "What the chain sees in a normal swap"}</caption>
          <tbody>
            {ROWS.map((r, i) => {
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
                      <span className="sr-only">Encrypted</span>
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
        <p className="sees-cap">Who traded and which assets are public. How much and at what price are not.</p>
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

export function Faq() {
  return (
    <section className="section" id="faq" aria-labelledby="faq-title">
      <div className="faq-grid">
        <h2 id="faq-title" className="big">
          Questions, answered
        </h2>
        <div className="faq">
          {FAQ.map(([q, a, id]) => (
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
