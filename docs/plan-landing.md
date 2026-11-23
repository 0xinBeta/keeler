# Keeler landing page: plan (revised after review, 2026-11-23)

Owner's brief: the page is too empty and too cautious. Add excitement, say how
good the product is, add sections, and do not put limits in plain sight or
scare people.

## Rules for the copy

- Confident and specific. Every claim must be true of the code as built.
- What is private: amounts and price. What is public: both wallet addresses,
  **which assets were traded**, the time, and that a swap happened. The
  dealers asked, and Keeler's Worker, see the request (wallet, assets, amount).
- Never claim: audited, anonymous, mainnet, real funds, "MEV-proof", hidden
  assets, a number of dealers, live or market prices on devnet, "firm" prices
  (dealers keep a last look), "all proofs on your device" (only the order's
  proof is; the pool's proof is made in Helius' attested enclave), "open"
  dealer software.
- Limits move out of the hero and the swap panel into the FAQ, phrased
  calmly. The network is a small pill in the navigation driven by
  `config.network` ("Devnet beta"), not a warning banner.
- One sentence stays next to the swap button because it describes what the
  button does: "If the dealer doesn't settle before the quote expires, you
  take your funds back in one click."

## Changes

- Remove the top warning banner. Add the network pill in the navigation.
- Navigation: How it works, Privacy, Dealers, FAQ.
- Remove the "Limits" section; its content goes into the FAQ.
- Correct the existing privacy table and panel copy to the rules above.

## Sections and copy

**Hero**
- Headline: "Swap without showing your hand."
- Text: "Keeler gets you signed quotes from dealers, checks them against a
  reference price, and settles the best one privately on Solana. The chain
  sees that you traded. It never sees how much, or at what price."
- Buttons (outside the swap panel): "Start a swap" (moves focus to the swap
  panel, which needs `tabIndex={-1}`), "See how it works".
- Three small chips: "Built on Helius Privacy Rings", "Settles on Solana",
  "Order proved in your browser".

**Facts** (four, large figure with one line each, static)
- "~1.5 s": "to prove your order, right in your browser"
- "0": "slippage. The quote you accept is the amount you get"
- "On-chain": "custody. Your funds sit in Solana programs, never in a Keeler
  account"
- "1 click": "to take your funds back if a dealer doesn't deliver"

**Why Keeler** — heading: "Everything a trade should be. Nothing it should leak."
1. Private where it counts: "Amounts and prices are encrypted on-chain.
   People can see that you traded, never how much or at what price."
2. Best price, by competition: "One request reaches every listed dealer. They
   quote without seeing each other, and the best signed quote wins."
3. Checked before you commit: "Every quote is signed and checked against a
   reference price. Anything outside the band can't be accepted."
4. No slippage, no surprises: "A quote is a fixed amount, signed by the
   dealer. You receive exactly that, or the trade doesn't happen."
5. Your keys, your browser: "Your private keys are created in your browser
   from your wallet. Keeler's servers never see them, and your order is
   proved on your device."
6. Always a way out: "If a dealer doesn't settle, only you can take the funds
   back, as soon as the quote expires."

**What the chain sees** — an interactive comparison with a two-way switch,
"A normal swap" and "On Keeler". Rows: wallet addresses (visible in both),
assets (visible in both), amount sold, amount received, price (visible in a
normal swap, masked on Keeler). Caption: "Who traded and which assets are
public. How much and at what price are not."

**How a swap works** (a real three-step sequence)
1. Ask: "One request goes to every listed dealer. Each answers with a signed
   quote."
2. Lock: "Keeler picks the best quote inside the band. Your browser proves
   the order and locks it so only that dealer can fill it."
3. Settle: "The dealer delivers and takes the order in one step, or not at
   all. If they don't, you take your funds back when the quote expires."

**Who it's for** — heading: "For traders who don't broadcast."
- Traders: "Your size and your price stay off the public record, so there is
  nothing for copy-traders to read."
- Treasuries and DAOs: "Rebalance without publishing how much."
- Funds and desks: "Fill at a fixed amount, with settlement that can't
  half-happen."

**Dealers** — heading: "Quote flow, privately."
- "Run a Keeler dealer: quote with your own prices and limits, sign every
  quote, keep the last look, and settle in one step on Solana."
- Link: "How dealers work" → the FAQ entry.

**FAQ** (accordion)
- What stays private? "The amounts and the price. The two wallet addresses,
  the assets, the time, and the fact that a swap happened are public. The
  dealers you ask see your request so they can quote it."
- Do I need a special wallet? "No. Your Solana wallet signs a message to
  create your private keys in the browser. It asks twice, to check your
  wallet signs consistently. Signing moves no funds."
- Who holds my funds? "On-chain programs on Solana: the Privacy Rings pool
  for your private balance, and Keeler's swap program while an order is
  locked. They are never in a Keeler account."
- Where are proofs made? "Your order's proof is made in your browser. The
  pool's own proof is made inside Helius' attested secure enclave; what your
  browser sends it is encrypted so only the enclave can read it."
- What if the dealer doesn't settle? "Your order stays locked until the
  quote expires, ten minutes at most. Then you take the funds back yourself,
  in one click."
- Can a dealer decline? "Yes. If the price moves before it settles, a dealer
  can pass. Nothing is lost: you take your funds back at expiry and ask
  again."
- What does it cost? "Solana network fees and the dealer's spread, which is
  already inside the quote you see. There is no protocol fee."
- What if I clear my browser? "Your orders can be found again from the chain
  with the same wallet. The browser test wallet keeps its key in this
  browser, so keep the site data while you use it."
- How do dealers work? "A dealer runs Keeler's dealer software with its own
  inventory, prices and limits, signs every quote, and settles through the
  same on-chain program. During the beta, Keeler lists the dealers."
- Is Keeler on mainnet? "Not yet. Keeler is in beta on Solana devnet with
  test tokens, so you can try every step for free. It has not been audited
  yet and uses test proving keys; mainnet follows the security review."

**Closing**: "Your next swap doesn't have to be public." Button: "Start a swap".

**Footer**: Keeler, "Built on Helius Privacy Rings", the network pill.

## Design

- Same identity (black, gold, ember, Unbounded and Onest, the ring). More
  life: each section gets its own small visual drawn from the ring and the
  gap (arcs, a masked-amount animation in the comparison, a three-node line
  for the steps), richer cards, a glow on the primary buttons.
- The hero ring grows: larger on desktop and visible behind the headline's
  right edge, with the swap panel kept readable.
- Motion: the comparison switch animates values into masks; the closing
  section reuses the ring as a slow arc. Respect reduced motion. No layout
  shift.
- Keep the floor: responsive to 360 px, keyboard and focus, AA contrast.

## Tests

- `pnpm build` and unit tests pass.
- The Playwright app test still passes. It depends on: the region "Private
  swap", the exact buttons "Swap" and "Balances", the heading "Swap
  privately", `orders-title`, `#sell-amt`, `.quote-amt`, and the labels "Get
  test tokens", "Get quotes", "Swap privately", "Take funds back", "Find my
  orders". Keep them, or update the test with the change.
- New screenshots at 1440, 768 and 390 px wide.
