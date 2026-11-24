# Keeler Lend: a Jupiter-style pool with private amounts (revised after review, 2026-11-24)

The owner rejected the earlier fixed-term design. The brief: the Jupiter Lend
mechanism (shared pool, floating rates, vaults, liquidations) with Privacy
Rings privacy, so the line can be "like Jupiter Lend, but private".

## Goal

- **Earn**: deposit a token into a shared pool, earn a floating rate, withdraw
  at any time while the pool has idle liquidity.
- **Borrow**: lock collateral in a vault, borrow the vault's debt token up to a
  maximum loan-to-value, repay at any time, no term. Liquidated with a penalty
  when the position crosses the liquidation threshold.
- Amounts are hidden from the public chain: deposits, collateral, debt,
  health and liquidation price. See "What is public" for the exact limits.

Devnet first, test tokens. First version: one pool (tUSD) and one vault (tSOL
collateral, tUSD debt).

## Facts checked (2026-11-24; the reviewer re-checked those marked ✓)

Jupiter Lend (developers.jup.ag/docs/lend, docs.jup.ag,
Instadapp/fluid-solana-programs, live mainnet reads):

- Earn: shares = assets × 1e12 / exchange price, minted rounded down, burned
  rounded up. ✓ The supply rate is `borrow_rate × (1 − fee_on_interest) ×
  utilisation`, fee 10% (per-token config). Rates are in basis points and the
  rate applied is the one stored at the previous update.
- Rate curve is piecewise linear in utilisation. USDC: 0 → 5% at 88% → 7% at
  93% → 15% at 100%. ✓
- Vaults: one collateral, one debt token. SOL/USDC: collateral factor 80%,
  liquidation threshold 85%, liquidation max limit 90%, penalty 1%, borrow
  fee 0. ✓ One `operate` call does deposit, borrow, repay and withdraw.
- Liquidation is permissionless and partial, back to the threshold; above the
  max limit the whole position is absorbed and the user loses all collateral.
  It works through public per-tick debt sums; every position's collateral,
  debt, liquidation price and owner are public. ✓
- Oracle accounts are fixed in vault config; max age 600 s for operate,
  7,200 s for liquidate. ✓
- The source is Business Source License 1.1, no production use. ✓ The
  mechanism and numbers are public; the code may not be copied.

Privacy Rings (privacy-rings docs/spec.md, spr-* 0.10.0 sources, live reads):

- In the Default Ring, the only live ring, owners are public signers on every
  spend, and output owner tags and counts are public. Amounts and assets of
  transfers are hidden. Hidden-sender modes are switched off on mainnet;
  custom rings are in audit until December 2026.
- A program holds funds as notes owned by its PDA; whoever builds a spend
  needs the note's opening. No key reads every note in the Default Ring.
- A note can carry a balance and private state and be spent and re-created
  (`DataUtxo::new_mut` without `close`); a `UniqueDataUtxo` is one state note
  at a fixed address, claimable once. ✓
- Several PDA owners plus a user signer fit in one transaction; shapes (1,2),
  (2,2), (2,4), (3,4) exist; zero-value outputs can pad to a fixed count. ✓
- All output blindings of a transaction come from one seed. ✓
- Output ciphertexts are unchecked in the Default Ring. ✓
- A proof takes up to 11 public fields, no byte arrays. ✓ Xark compiles
  division, checked arithmetic and `u128`. ✓
- The keeper cannot spend a user's own notes: that needs the user's nullifier
  secret. ✓
- Swap instructions cost about 245k compute units, proof verification 95k. ✓
- Dealer and Worker cap request bodies at 4 KiB and time out at 3 s
  (`cli/src/dealer.rs`, `worker/src/index.ts:46-47`); the dealer serves one
  HTTP request at a time.

## What follows

1. A pool anyone can use unaided makes every amount public: each user would
   need the pool note's opening, and two openings differ by the amount in
   between.
2. Amounts stay private only if one party holds the pool openings and builds
   the pool side of every transaction: the **keeper**, a service like today's
   dealer. It sees amounts. It cannot take anyone's principal.
3. A liquidator must see a position, so liquidation is done by holders of the
   pool key, each time with a proof that the position is past the threshold at
   the oracle price. Jupiter's tick engine needs public debt per tick and
   cannot be reproduced.
4. Wallets stay public until custom rings are live.

## Approaches

1. **Wrap the real Jupiter Lend.** Amounts are public at every call and the
   owner signs; Jupiter's devnet is stale. Rejected.
2. **Our own public pool with hidden owners.** Needs hidden-sender mode, not
   live. Later, on a custom ring.
3. **Our own pool inside the ring, sequenced by a keeper** (chosen).

## Design

### A separate program

Lend is its own program in `lend/` in the repo, with its own intent circuits.
The swap program, its deployment and the running dealer are not touched, and
there is no swap cutover.

### Notes

| Note | Owner | Balance | Private data |
|---|---|---|---|
| Reserve, one per pool | pool PDA, at a fixed unique address derived from the mint | idle liquidity | supply shares, debt shares, supply price, borrow price, stored rate, last update, a random salt |
| Receipt (Earn) | the user's PDA | 0 | owner, pool, supply shares |
| Position | the user's PDA | collateral | owner, vault id, position id, debt shares |
| Intent | the user's PDA | funds going in | maker, purpose, target pool or position id, expiry |

- The reserve is a `UniqueDataUtxo`, created once by `init_reserve`. Every
  circuit recomputes its address from the mint; it is never a free witness.
- Every circuit pins assets: the reserve holds the pool's mint, a position
  holds its vault's collateral mint, a receipt names its pool.
- The salt is chosen fresh by the keeper on every operation, so a user who
  knows a transaction's blinding seed cannot test guesses of someone else's
  amount against the reserve's public hash.

### Instructions

| Instruction | Proved by | Spends | Creates | Authorised by |
|---|---|---|---|---|
| `init_reserve` | keeper | nothing | reserve | admin signer, once per mint |
| `intent` | browser | user's note | intent, change | user |
| `cancel_intent` | browser | intent | refund | user, after expiry |
| `supply` | keeper | reserve, intent | reserve, receipt | the intent |
| `redeem` | keeper | reserve, receipt | reserve, payout, receipt | owner signs |
| `deposit` | keeper | intent, position if any | position | the intent |
| `draw` | keeper | reserve, position | reserve, position, debt payout, collateral payout | owner signs |
| `repay` | keeper | reserve, position, intent | reserve, position, change | the intent |
| `liquidate` | keeper | reserve, position, liquidator's note | reserve, position, collateral and change to liquidator | proof of an unhealthy position |
| `close` | browser | position with no debt | collateral to owner | owner signs |

`deposit` opens a position or adds collateral. `draw` borrows, withdraws
collateral, or both; an observer cannot tell which. Every instruction has a
fixed number of outputs, padded with zero-value outputs, so a full and a
partial redeem or repay look the same.

Rules:

- **Who benefits is fixed by the circuit.** A receipt or position created or
  credited from an intent must belong to the intent's maker, change goes to
  the maker, and the intent names the position id it is for.
- **Every instruction that spends a reserve needs the keeper's signature.**
- **Accrual** runs in-circuit first, from the reserve's stored state to a
  public `now`: the stored rate is applied for the elapsed time, then the new
  rate is computed from utilisation and stored. Borrow price rounds up, supply
  price down. Zero supply means utilisation 0; utilisation is clamped at 100%
  for the supply rate; zero elapsed time changes nothing. The program checks
  `now <= clock <= now + 120`; the circuit checks `now >= last update`. `now`
  comes from chain time.
- **Shares**: supply shares minted rounded down and burned rounded up, debt
  shares the reverse. A supply that would mint zero shares is rejected.
- **Minimums and a cap**: minimum supply, collateral and debt per vault; a
  borrow may not take utilisation above 95%, so withdrawals cannot be frozen
  by one borrower.
- **Prices**: `draw` requires `debt <= CF × collateral × bound` and the
  program requires `oracle price − confidence >= bound`. `liquidate` requires
  `debt > LT × collateral × bound` with `oracle price + confidence <= bound`.
  The bound is the current price moved by a fixed margin, never derived from
  the position. One age limit for both, 90 s. The feed account address and
  feed id are program constants per vault; tUSD is valued at 1.
- **Liquidation**: the liquidator repays `x` and receives collateral worth
  `x` at the bound. A further `x × penalty` of collateral is sold the same way
  and its proceeds stay in the reserve as a buffer against bad debt, so the
  keeper earns nothing from liquidating. The circuit bounds the size, it does
  not fix it: `0 < x <= debt`; `x` removes at least one debt share (or clears
  the debt), so collateral is never taken without reducing the debt; and,
  unless the position is above the max limit, the position is left at or
  past the threshold, never healthier. Within that the keeper chooses `x`:
  it takes the largest that leaves the position at the threshold, or the
  whole debt above the max limit (`core::lend::liquidate`), with what is left
  returned to the owner. Debt left on a position with no collateral stays on the books and
  keeps accruing; the buffer covers it first, then the last suppliers to
  leave bear it.
- **Seeds**: an instruction that spends a position seeds its blindings from
  the position; `redeem` from the receipt; `supply` and a first `deposit`
  from the intent. An owner can always follow their own note's blinding.
- **Openings for the owner**: every operation carries a message encrypted to
  the owner's viewing key with the new note's contents. The page checks it
  before signing an owner-signed operation.
- **Scales**: token amounts u64; prices and the oracle bound are scaled by
  1e12 (debt base units per collateral base unit); rates and factors in basis
  points; a year is 31,536,000 s. Every product is at most u64 × u64 before
  its division.

Vault and curve parameters are constants selected by pool and vault id.
tSOL/tUSD vault: 80 / 85 / 90 / 1%. tUSD pool: the USDC curve, 10% fee.

### The keeper

Runs beside the dealer, on its own HTTP thread with its own limits.

- Holds the reserve opening and every position and receipt opening.
- **Admission**: before making an intent the page asks for a keeper-signed
  ticket (wallet-authenticated, minimum sizes, rate-limited). The keeper
  fills only intents it issued a ticket for; others refund after expiry.
- **Queue order**: liquidations, then repay and deposit, then the rest.
- **Owner-signed operations**: the page sends a wallet-signed request (nonce
  and timestamp, domain-separated from the key-derivation message). The
  keeper reserves the reserve for that wallet for 20 s (one at a time;
  liquidations pre-empt), returns the built transaction, and the page checks
  it, signs and sends it through `/api/rpc`.
- **Records**: every built transaction is saved at build time, keyed by the
  reserve nullifier it spends. The keeper learns which one landed from the
  chain by output hash, so the reserve opening is never lost.
- Watches positions against the oracle and liquidates from its own inventory.
- Publishes coarse pool figures in time buckets (size, utilisation, rates).

### What the page checks before a wallet signs a keeper-built transaction

The exact message shape, or it refuses: compute-budget instructions plus one
lend instruction with the expected program ID and tag; no lookup tables, no
durable nonce; the user's key read-only and not the fee payer; `now`, the
price bound and the expiry as requested; the expected output hashes at fixed
slots; the owner message opening to the expected note. Unit tests feed it
hostile transactions (an added token transfer or approval, a different
instruction, a different payout).

### What is public, private, and seen by whom

- **Public**: the wallets; which pool or vault; that an operation happened and
  its instruction (with `draw` hiding borrow versus withdraw); that a wallet
  was liquidated, and that its loan-to-value was then at the threshold at a
  public price; the price bound; anything a user later unshields.
- **Hidden from people outside the pool**: every amount, health and
  liquidation price; pool size and utilisation beyond the coarse figures.
- **Other pool users** learn the price indices at their own operations, so
  they can estimate utilisation over time and notice very large operations.
- **The keeper** sees all amounts of all pool users.
- **A liquidated owner** can derive the blinding of the liquidator's change
  note: `liquidate` seeds every output's blinding from the position, whose
  opening the owner holds. The keeper therefore liquidates from a note of
  exactly `x + buffer`, so the change slot is empty and the owner learns
  nothing about the keeper's other funds.
- **Intent expiry** is a private value with no upper bound in the circuit:
  a far-future expiry locks the maker's own funds until then, since
  `cancel_intent` waits for it and the keeper fills only an intent matching
  one of its tickets. The ticket fixes the expiry (`now + ticket_secs`) and
  the page uses the ticket's; an intent made outside the page with another
  expiry is the maker's own risk.

### Differences from Jupiter Lend

1. Liquidators hold the pool key; liquidation is not open to anyone.
2. Per-position liquidation instead of the tick engine.
3. Above the max limit the position is sold at the penalty and the rest
   returned, where Jupiter takes everything.
4. Deposit and repay need no price and are never refused; a zero-debt
   position can be closed without the keeper.
5. Earn shares are a private receipt, not a transferable token.
6. Collateral is not lent out and earns nothing.
7. No expanding limits, rewards, flash loans or smart vaults.
8. One operation at a time per pool.

### Interfaces (fixed before the pieces start)

- **Keeper HTTP** (behind `x-keeler-key`, 16 KiB bodies, 30 s timeout, own
  limiter): `GET /lend/pools`; `POST /lend/ticket`; `POST /lend/build`;
  `GET /lend/account`. Ticket, build and account need a fresh wallet
  signature.
- **Worker**: `/api/lend/*` forwards with matching caps; `/api/config` gains
  `lend: { programId, pools, vaults }`. The page refuses a config whose lend
  program ID is not the one compiled into the build.
- **Web contract**: `pools()`, `lendAccount()`, `supply`, `redeem`,
  `deposit`, `draw`, `repay`, `closePosition`, each a tracked operation with
  steps. Stored records are scoped by program ID.
- **Shared math** (`core/src/lend.rs` and a TypeScript twin): the scales
  above, rate curve, accrual, share conversion, health, liquidation size,
  with shared vectors that the circuits must also pass.

## Work

Nothing touches `.tools`, the main checkout's `target/release` or
`target/devnet`; the live dealer runs from them. Agents copy
`web/public/swap` from the main checkout and do not run `setup.sh`.

### 0. Program core (one agent, first)

- New workspace `lend/` (program, SDK, WASM package), built with the same
  tools, with a per-checkout environment file.
- Spike on the local network, and report before going on: the unique reserve
  note with balance and state, spent and re-created; three program-owned
  inputs under two PDAs plus user and keeper signers; an optional input and
  fixed output padding; accrual in-circuit with constraint count and proving
  time; the program reading a price account and checking the bound; compute
  units and transaction size at the largest shape.
- Then the shared parts: note types, PDAs, accrual and share math against the
  vectors, `init_reserve`, `intent`, `cancel_intent`, and stubs for every
  remaining tag, module, verifier and error code so later agents do not
  collide. Owns the SDK and WASM exports for decoding and output hashes.

### 1. In parallel with 0

- **Shared math** in Rust and TypeScript with vectors (scales are fixed
  above).
- **Worker** routes, caps and config.
- **Interface**: Swap / Earn / Borrow, a pool card with rates, a vault page
  with a health bar and liquidation price, an account view, on the mock
  client. Landing copy under `docs/plan-landing.md` rules.

### 2. After 0, in parallel

- **Earn**: `supply`, `redeem`, with program tests.
- **Vault**: `deposit`, `draw`, `repay`, `liquidate`, `close`, the oracle
  check and the test price account (a program-owned account at a pinned
  address per vault, compiled out of a mainnet build with a `compile_error!`
  guard), with program tests for every rule above and time moved through the
  test Clock.

### 3. After 2, in parallel

- **Keeper** (`cli/`): chain layer, admission, queue, reservation, build,
  records and rebuild, liquidation watcher, local end-to-end tests: supply,
  borrow, interest, repay, redeem; price drop and partial liquidation; full
  liquidation above the max limit; keeper killed between build and landing;
  two users racing; a dust flood.
- **Web client** (`web/src/keeler/`): tickets, intents, the signing check,
  `close` proving, tracking and recovery from the keeper's account view and
  owner messages, unit tests.

### 4. Integrate, review, ship to devnet

- Merge; browser end-to-end against the real keeper; full test run; fresh
  Fable code review; fixes.
- Deploy the lend program with a deploy script that takes a deployment name
  (own Keychain items and output directory). Ask the owner for devnet SOL
  from the built size first (ten verifying keys; expect several SOL at peak).
- Start the keeper, publish Worker and site together.
- By hand on devnet: the local flows again, with the test price moved to
  trigger a liquidation.

## Risks and known limits

1. **Size.** Ten circuits, in-circuit interest math, a sequencer and a
   liquidator: several times the fixed-term design, with an audit and key
   ceremony to match.
2. **The keeper can halt the pool.** Offline, nobody can withdraw from Earn,
   borrow, repay or be liquidated; a position that could not be topped up may
   be liquidated when it returns. If it wrote an unreadable reserve record,
   nobody else could take over. It cannot take principal. A custom ring's
   checked auditor key closes this later. It must be stated on the page.
3. **The keeper sees every amount** and orders every operation.
4. **Bad debt.** One keeper on a laptop is the only liquidator on devnet.
5. **Toolkit unknowns** in the spike; any can change the design.
6. **One lane per pool**, a few operations a minute.
7. **Forgeable test keys on devnet.** Anyone can take any note the program
   owns. Test tokens only.
8. **Pyth on devnet** may not keep its fixed feed accounts fresh; the test
   price account is the fallback there.
9. **Naming.** "Like Jupiter Lend" is a comparison, not an affiliation; no
   Jupiter code is used. The owner handles legal questions.

## Not in this version

Hidden wallets; open liquidation; a transferable share token; yield on
collateral; more pools and vaults; expanding limits; rewards; flash loans;
changing parameters without a new program; more than one keeper.

## Mainnet gates added by this feature

Audit and ceremony for all circuits; frozen circuits per program ID; at least
two independent keepers and liquidators with a liquidator reward; real Pyth
feeds with tested limits; an exit that does not depend on one operator; the
move to a custom ring when Helius opens them.
