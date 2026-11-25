// The landing: the flight through the ring (landing/Flight.tsx), then the FAQ and a footer. Every sentence must be
// true of the code; docs/plan-lend.md ("What is public, private, and seen by whom") says what may be claimed.
import "../styles/landing.css";
import type { Config } from "../keeler/index";
import { BUILD_NETWORK } from "../keeler/network";
import type { Route } from "./App";
import type { Market } from "./Lend";
import { Flight } from "./landing/Flight";
import { Logo, NetworkPill } from "./shared";

/** The example pair's symbols: the lending market's vault when the service has named it, else the build network's. */
export type Pair = { collateral: string; debt: string };
export function pairOf(market?: { vaults: { collateral: { symbol: string }; debt: { symbol: string } }[] } | null, network = BUILD_NETWORK): Pair {
  const v = market?.vaults[0];
  if (v) return { collateral: v.collateral.symbol, debt: v.debt.symbol };
  return network === "mainnet" ? { collateral: "SOL", debt: "USDC" } : { collateral: "tSOL", debt: "tUSD" };
}

export type LandingProps = {
  config: Config | null;
  market: Market | null;
  /** False on a deployment without lending: the swap-led landing. */
  lend: boolean;
  open: (route: Route) => void;
  network?: string;
};

export function Landing({ config, market, lend, open, network = BUILD_NETWORK }: LandingProps) {
  const pair = pairOf(market, network);
  return (
    <div className="kl-landing">
      <Flight config={config} market={market} lend={lend} open={open} network={network} pair={pair} />
      <Faq lend={lend} network={network} pair={pair} />
      <footer className="kl-foot">
        <span className="kl-foot-brand">
          <Logo />
          Keeler
        </span>
        <span>Built on Helius Privacy Rings</span>
        <NetworkPill config={config} />
      </footer>
    </div>
  );
}

const lendFaq = ({ collateral: c, debt: d }: Pair): [string, string, string?][] => [
  ["What does “Jupiter-style” mean?", "Keeler Lend follows the same shape as Jupiter Lend: one shared pool per token with a floating rate set by how much of it is borrowed, and vaults where you borrow against collateral with a borrow limit, a liquidation threshold and a penalty. Keeler is not affiliated with Jupiter and uses none of its code."],
  ["What is visible when I lend or borrow?", "Your wallet address, which pool or vault you used, when, and which kind of operation it was; a borrow and a collateral withdrawal look the same. If a position is liquidated, that is public too, with the price used. Your amounts, health and liquidation price are not. People using the same pool can estimate how much of it is borrowed over time, and may notice a very large operation."],
  ["Who runs the pool?", "A keeper service run by Keeler. It holds the pool's private records, so it can see the amounts in every deposit and position. It puts operations in order and performs liquidations. By design it cannot take your principal: your funds only move with your signed request or your wallet's signature, or by a liquidation under the vault's rules. On this devnet beta the proving keys are test keys and the keeper sets the test price, so that guarantee waits for mainnet and its security review."],
  ["Will the keeper always see lending amounts?", "Today it does: lending amounts are visible to you and to Keeler's keeper service. Running the keeper in an attested enclave, so that nobody can read amounts, is planned."],
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
    <section className="kl-faq" id="faq" aria-labelledby="faq-title">
      <h2 id="faq-title">Questions, answered</h2>
      <div className="kl-faq-list">
        {list.map(([q, a, id]) => (
          <details key={q} id={id}>
            <summary>
              <span>{q}</span>
              <span className="kl-faq-icon" aria-hidden="true" />
            </summary>
            <p>{a}</p>
          </details>
        ))}
      </div>
    </section>
  );
}
