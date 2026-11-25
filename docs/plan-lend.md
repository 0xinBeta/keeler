# Keeler Lend: design

How the lending pool works and why it is built this way. Every statement here describes the code
in this repository. The threat model is `docs/threat-model.md`, the short reasons behind single
choices are `docs/design-decisions.md`, running a keeper is `docs/lend-keeper.md`, and the
page-keeper contract is `docs/lend-api.md`.

## Goal

- **Earn**: deposit a token into a shared pool, earn a floating rate, withdraw at any time while
  the pool has idle liquidity.
- **Borrow**: lock collateral in a vault, borrow the vault's debt token up to a maximum
  loan-to-value, repay at any time, no term. A position past the liquidation threshold is
  liquidated with a penalty.
- Amounts are hidden from the public chain: deposits, collateral, debt, health and liquidation
  price. "What is public, private, and seen by whom" gives the exact limits.

This version has one pool (tUSD on devnet) and one vault (tSOL collateral, tUSD debt). The vault's
mints, price account and feed id are build parameters; a mainnet build is set up for SOL/USDC.

## Background: Privacy Rings

Read from the Privacy Rings specification and the `spr-*` 0.10.0 sources, and confirmed by the
toolkit spike (`docs/notes-for-xark-builders.md`):

- In the Default Ring, the only live ring, owners are public signers on every spend, and output
  owner tags and counts are public. Amounts and assets of shielded transfers are hidden; a move
  into or out of the pool through the interface shows its amount. Hidden-sender modes
  are switched off on mainnet; custom rings are in audit until December 2026.
- A program holds funds as notes owned by its PDA; whoever builds a spend needs the note's opening.
  No key reads every note in the Default Ring.
- A note can carry a balance and private state and be spent and re-created (`DataUtxo::new_mut`);
  a `UniqueDataUtxo` is one state note at a fixed address, which the shielded pool lets be created
  only once.
- Several PDA owners plus a user signer fit in one transaction; zero-value outputs pad a
  transaction to a fixed output count.
- All output blindings of a transaction come from one seed.
- Output ciphertexts are not checked in the Default Ring: a writer can encrypt a note to nobody.
- A proof takes at most 11 public fields, and no byte arrays (a 32-byte mint is two `u128`
  halves). Xark compiles division, checked arithmetic and `u128`.
- Spending a user's own note needs that user's nullifier secret; nobody else can.
- PDA notes use the all-zero nullifier key, the note hash commits to the owner and not to the
  viewing key, and a transaction's writer key is derived from the writer's viewing secret and the
  first nullifier. So any holder of one viewing key can open and spend the PDA notes another
  holder wrote, while keeping its own signing and nullifier keys
  (`ShieldedKeypair::with_viewing_key`).

## Background: Jupiter Lend's public mechanism

Read from Jupiter Lend's developer documentation, its program source and mainnet accounts in
November 2026. Keeler Lend uses the same kind of mechanism (shared pool, floating rate, vaults,
partial liquidation); no Jupiter Lend code is used, and the project is not affiliated with Jupiter.

- Earn: shares = assets × 1e12 / exchange price, minted rounded down, burned rounded up. The supply
  rate is `borrow_rate × (1 − fee_on_interest) × utilisation`, with a 10% fee (per-token
  setting). Rates are in basis points, and the rate applied for a period is the one stored at the
  previous update.
- The rate curve is piecewise linear in utilisation. USDC: 0 → 5% at 88% → 7% at 93% → 15% at
  100%.
- Vaults: one collateral token and one debt token. SOL/USDC: collateral factor 80%, liquidation
  threshold 85%, liquidation max limit 90%, penalty 1%, no borrow fee.
- Liquidation is open to anyone and partial, back to the threshold; above the max limit the whole
  position is absorbed. It works through public per-tick debt sums, so every position's
  collateral, debt, liquidation price and owner are public.
- Oracle accounts are fixed in the vault configuration.

## What follows

1. A pool anyone can use unaided makes every amount public: each user would need the pool note's
   opening, and two successive openings differ by the amount in between.
2. Amounts stay private only if a known party holds the pool openings and builds the pool side of
   every transaction: the **keeper**. It sees amounts. It cannot take anyone's principal, because
   who is paid and how much is a circuit rule.
3. Because ciphertexts are unchecked, the keeper must also be the only party allowed to write the
   reserve, and only under a key every keeper can open.
4. A liquidator must see a position, so liquidation is done by the keepers, each time with a proof
   that the position is past the threshold at the oracle price. A tick engine needs public debt per
   tick and cannot be reproduced.
5. Wallets stay public until custom rings are live.

## Approaches considered

1. **Wrap Jupiter Lend.** Amounts are public at every call and the user signs the public
   instruction. Rejected.
2. **A public pool with hidden owners.** Needs a hidden-sender mode, which is not live. Possible
   later, on a custom ring.
3. **A pool inside the ring, sequenced by a keeper set** (chosen). Amounts are private from
   everyone but the keepers, at the cost of one lane per pool and trust in the keepers for
   liveness.

## Design

Lend is its own program (`lend/program/`) with its own circuits; the swap program is separate.

### Notes

| Note | Owner | Balance | Private data |
|---|---|---|---|
| Reserve, one per pool | pool PDA `["pool", mint]`, at a fixed unique address derived from the mint | idle liquidity | supply shares, debt shares, supply price, borrow price, stored rate, last update, salt |
| Receipt (Earn) | the user PDA `["user", wallet]` | 0 | owner, pool id, supply shares |
| Position | the user PDA | collateral | owner, vault id, position id, debt shares |
| Intent | the user PDA | funds going in | maker, purpose (supply, deposit, repay), target pool or position id, expiry |

- The reserve is a `UniqueDataUtxo`, created once by `init_reserve` at
  `Poseidon("RSRV", mint_hi, mint_lo)` under the pool PDA. Every circuit recomputes its address
  from the mint; it is never a free witness. The program accepts only the canonical PDA bump and
  the pool's mint.
- Every circuit pins assets: the reserve holds the pool's mint, a position holds its vault's
  collateral mint, a receipt names its pool.
- The salt is chosen fresh by the keeper on every operation, so a user who knows a transaction's
  blinding seed cannot test guesses of the reserve's contents against its public hash.
- The reserve is encrypted to the pool viewing key. Receipts and positions are encrypted to their
  owner's viewing key; the keepers keep their openings from the builds and read their peers'
  through the writer key.
- Each supply fill makes a new receipt; a wallet may hold several in one pool.

### Instructions

Eleven circuits, one verifying key each, plus one program-only instruction for test builds.

| Instruction | Proved by | Spends | Creates | Authorised by |
|---|---|---|---|---|
| `init_reserve` | keeper | nothing | reserve | a keeper and the admin, once per mint |
| `intent` | browser | the user's note | intent, change | the user |
| `cancel_intent` | browser | intent | refund | the user, after expiry |
| `supply` | keeper | reserve, intent | reserve, receipt | a keeper, filling the intent |
| `redeem` | keeper | receipt, reserve | receipt, reserve, payout | a keeper and the receipt's owner |
| `deposit_open` | keeper | intent | position | a keeper, filling the intent |
| `deposit_add` | keeper | position, intent | position | a keeper, filling the intent |
| `draw` | keeper | position, reserve | position, reserve, debt payout, collateral payout | a keeper and the position's owner |
| `repay` | keeper | position, reserve, intent | position, reserve, change | a keeper, filling the intent |
| `liquidate` | keeper | position, reserve, the keeper's note | position, reserve, change, collateral payout | a keeper, with a proof the position is past the threshold |
| `close` | browser | a position with no debt | collateral to the owner | the owner |
| `set_test_price` | none | nothing | the vault's test price account | a keeper; only in a `test-price` build |

`draw` borrows, withdraws collateral, or both; both payouts are always present, so an observer
cannot tell which. Every instruction has a fixed number of outputs, padded with zero-value notes,
so a full and a partial redeem or repay look the same. `deposit_open` and `deposit_add` are
separate circuits (a data input cannot be optional), so a new position and a top-up can be told
apart.

### Rules

- **Who benefits is fixed by the circuit.** A receipt or position created or credited from an
  intent belongs to the intent's maker, change goes to the maker, and the intent names the pool or
  position it is for. Redeem and draw payouts go to the recorded owner, whose identity the program
  computes from the signer.
- **Keeper signature.** Every instruction that spends a reserve or fills an intent needs a signer
  that is one of the compiled `KEEPERS` (`require_keeper`), and every `pool_viewing` argument must
  equal the compiled `POOL_VIEWING` (`require_pool_viewing`). `intent`, `cancel_intent` and `close`
  need no keeper, so a user can always refund an expired intent and close a debt-free position.
- **Accrual** runs in-circuit first, from the reserve's stored state to a public `now`: the stored
  rate is applied for the elapsed time, then after the operation the new rate is computed from
  utilisation and stored. The borrow price rounds up, the supply price down. Zero supply means
  utilisation 0; utilisation is clamped at 100%; zero elapsed time changes nothing. The program
  checks `now <= clock <= now + 120`; the circuit checks `now >= last update`. So `now` is chain
  time.
- **Shares**: supply shares minted rounded down and burned rounded up, debt shares the reverse. A
  supply that would mint zero shares is refused. A repay of at least the debt clears the shares
  and returns the change.
- **Minimums and the cap**: at least 1 tUSD per supply; at least 0.01 tSOL of collateral in a
  position after a deposit, and after a draw that leaves any; and 1 tUSD of debt left by a draw
  that leaves any. `liquidate` checks no minimum, so it can leave less. A borrow is refused above idle
  liquidity or when it would take utilisation above 95%, so one borrower cannot freeze
  withdrawals.
- **Prices and the bound**: `draw` requires `debt <= CF × value(collateral, bound)` and the program
  requires `price − confidence >= bound`; `liquidate` requires `debt > LT × value(collateral,
  bound)` and the program requires `price + confidence <= bound`. The bound is the oracle price
  moved by a fixed margin against the actor (the keeper's `bound_margin_bps`, default 50), never
  derived from the position, so it publishes nothing about the position's health. Both require
  the price to be published within 90 s of the cluster clock. The price account and feed id are build parameters per vault; the debt token is
  valued at exactly 1 USD. See "Prices" below.
- **Liquidation**: the liquidator transfers exactly `x` into the reserve and receives
  `floor((x + floor(x × penalty / 1e4)) × 1e12 / bound)` of collateral, capped at the position's
  collateral. The penalty (1%) is the liquidator's reward; nothing is paid into the reserve beyond
  `x`. The circuit bounds the size and does not fix it: `0 < x <= debt`; `x` removes at least one
  debt share, so collateral is never taken without reducing the debt; and, unless the position is
  above the max limit, it is left at or past the threshold, never healthier. Within that the keeper
  takes the largest `x` that leaves the position at the threshold, or above the max limit the
  largest `x` whose payout fits the collateral (`core::lend::liquidate`). What collateral is left
  stays in the position.
- **Bad debt**: debt left on a position with no collateral stays on the books and keeps accruing.
  The interest fee (10% of the borrow interest) is never paid out and is the only buffer; past it,
  the last suppliers to leave bear the loss. There is no write-off.
- **Seeds**: an instruction that spends a position seeds its output blindings from the position,
  except `close`, which the owner builds;
  `redeem` from the receipt; `supply` and `deposit_open` from the intent, each as
  `Poseidon(domain, blinding)`. An owner can always recompute their own outputs.
- **Openings for the owner**: a new receipt or position is encrypted to the owner's viewing key
  (`owner_viewing`), and a payout is a token note to the owner's own address. The page checks both
  before an owner signs (see "What the page checks").
- **Scales**: token amounts u64; prices and the bound scaled by 1e12 (debt base units per
  collateral base unit); rates and factors in basis points; a year is 31,536,000 s. The reference
  maths is `core/src/lend.rs`, with shared vectors (`web/tests/fixtures/lend-vectors.json`) that
  the Rust reference and the page's TypeScript twin pass; the circuits pass the vectors of the
  functions they implement.

Vault and curve parameters are constants selected by pool and vault id. tSOL/tUSD vault:
collateral factor 80%, threshold 85%, max limit 90%, penalty 1%. tUSD pool: the USDC curve above,
10% fee.

### Prices

- A vault reads one pinned Pyth `PriceUpdateV2` push account: owned by Pyth's receiver program,
  fully verified, the pinned feed id, published at most 90 s from the cluster clock, a positive
  price and a confidence below it. A `test-price` build also accepts a program-owned test price
  account that a keeper writes; that feature cannot compile with `mainnet` or without the test
  keys.
- Anyone may post a fully verified update newer than the stored one to the push account, and it
  need not be the latest. An age limit is therefore also a window in which a poster can choose
  among recent prices. One limit for draws and liquidations keeps that window as small as the
  feed allows; a looser limit for liquidations would widen it for the party that is paid to
  liquidate.
- The SOL/USD push account updates about once a minute (a 15-minute sample in November 2026: a
  median of 54 s and a maximum of 61 s between publish times). The keeper's own limits sit inside
  the program's: it builds a draw with a price at most 65 s old and liquidates with one at most
  80 s old. In `pyth` mode a stale or unverified account answers 412 to draws and makes the pass
  skip liquidations; the keeper uses no other source.
- Valuing the debt token at 1 USD means a depeg below 1 makes liquidations early, never late.
- If the sponsored updates stop, draws and liquidations stop until someone posts an update
  (`docs/mainnet-runbook.md`, "When the Pyth account stalls").

## The keeper

The code is `cli/src/keeper.rs`, `cli/src/lend_chain.rs` and `cli/src/lend_auth.rs`; the
configuration and operations are in `docs/lend-keeper.md`.

### The keeper set and the pool key

- The program compiles in 1 to 4 keeper addresses (`KEELER_LEND_KEEPERS`, no duplicates) and the
  pool viewing public key (`KEELER_LEND_POOL_VIEWING`, 33 bytes). Any keeper in the set may sign;
  the set and the key change only by a program upgrade.
- All keepers work under one pool key. The pool viewing key derives from it; each keeper keeps its
  own signing and nullifier keys under that viewing key. So any keeper can open and spend what
  another wrote. The program pins the `pool_viewing` argument, so an honest build encrypts the
  reserve to the shared key and a misconfigured keeper fails. It does not check the ciphertext: a
  keeper can still write a reserve nobody can open (trust).
- A keeper refuses to start unless its viewing key equals the compiled `POOL_VIEWING`, and on
  mainnet unless the pool key is not itself a keeper key.
- The page encrypts intents to the compiled pool viewing key and accepts tickets and builds from
  any compiled keeper; a build's fee payer must be the keeper its reply names.
- A `mainnet` build needs every build parameter set explicitly and refuses the public fixture keys
  as program, admin, keeper or pool key (`lend/program/build_params.rs`).

### Roles and following each other

- `role` is `primary` or `standby`. A standby serves tickets and builds when asked and liquidates a
  position only after it has been past the threshold for `standby_grace_secs` (default 45).
- Each keeper reads the transactions paid by every keeper of the set every pass, with one scan
  mark per keeper, so peers' fills, deposits and liquidations reach its book. When a note it holds
  is spent by a transaction it did not build, it drops its own builds on that note and takes the
  peer's outputs.
- A ticket signed by any keeper of the set is valid for every keeper. The page sends its open
  tickets with every account read; a keeper adopts a valid peer ticket and fills its intent, so a
  repay made through a stopped primary is filled by the standby.
- Racing keepers spend the same reserve: one transaction fails on the nullifier and costs a fee.
- The Worker forwards each lend request to the first keeper in `LEND_KEEPERS` and to the next only
  when there was no keeper reply.

### What a keeper does

- **Holds** the openings of the reserve and of every receipt and position.
- **Admission**: before making an intent the page asks for a keeper-signed ticket
  (wallet-authenticated, minimum sizes, rate-limited). A keeper fills only intents that match an
  open ticket exactly; others refund after expiry. The ticket fixes the intent's expiry.
- **Queue**: one lane per pool. Liquidations first, then repays, then deposits, then supplies.
- **Owner-signed operations** (`redeem`, `draw`): the page sends a signed request; the keeper
  reserves the lane for that wallet for `reservation_secs` (default 20; liquidations pre-empt it),
  returns the built transaction, and the page checks it, signs it and sends it.
- **Records**: every build is saved before it is sent or handed to a wallet, keyed by the notes it
  spends. The keeper learns from the chain which build landed by its output hashes, so the reserve
  opening is never lost.
- **Liquidation**: it watches every indebted position against `price + confidence` plus the
  margin and liquidates from its own debt-token notes. It pays from a note of exactly `x`, made
  first by one self-transfer when it has none, so the change slot is empty (see "What is public").
- **Pool figures**: size, utilisation and rates rounded to two significant figures, recomputed at
  most once per `bucket_secs`.

### Canary caps and the bad-debt halt

- Caps: `max_pool_size`, `max_supply_per_wallet`, `max_total_collateral`,
  `max_position_collateral`, `max_position_debt`, checked at the ticket and again at the fill. A
  top-up of an existing position is never held to `max_total_collateral`, and repays are never
  capped, so a position can always be saved. On mainnet the keeper refuses to start with any cap
  at 0.
- `min_position_debt`: a borrow must leave no debt or at least this much, set so that a
  liquidation's 1% covers its fee.
- Each pass the keeper computes the surplus (idle plus debt less what suppliers are owed) and the
  bad debt (debt of positions whose collateral at `price − confidence` is worth less than it),
  publishes both coarsely, and refuses new borrowing while the bad debt exceeds the surplus.
- The caps are per keeper: two keepers filling deposits at once during a failover can pass
  `max_total_collateral` (review K-2).

### Trust statement

The pool keeps working when any one keeper is lost, and the upgrade authority can add a keeper and
hand it the pool key. A keeper can still halt the pool by withholding or corrupting what it writes
(ciphertexts are unchecked until a custom ring), so the set is hosts the operator controls. With
every pool-key holder gone, Earn deposits and indebted positions cannot move; intents refund and
debt-free positions close without a keeper.

## What the page checks before a wallet signs a keeper-built transaction

`checkKeeperTransaction` (`web/src/keeler/lend-check.ts`) refuses anything but the exact shape the
page asked for: compute-budget instructions plus one lend instruction of the requested op and
program ID; no lookup table, no durable nonce; the fee payer is the keeper the reply names, which
must be a compiled keeper; the owner is one read-only signer; `now` at most 60 s behind and 10 s
ahead of chain time; a transact expiry at most 300 s ahead; no interface transfers; the draw bound
within 2% of the page's reference price; the owner's note spent first; every note the owner should
receive hashing to the output at its slot, with each payout's ciphertext opening under the owner's
viewing key. Unit tests feed it hostile transactions (an added transfer or approval, another
instruction, another payout).

## What is public, private, and seen by whom

- **Public**: the wallets; which pool or vault; that an operation happened and its instruction
  (with `draw` hiding borrow versus withdrawal, and `deposit_open` versus `deposit_add` visible);
  that a wallet was liquidated, and that it was then past the threshold at a public price; `now`,
  the price bound and an intent's expiry when it is filled or cancelled; anything a user later
  unshields.
- **Hidden from people outside the pool**: every amount, health and liquidation price; pool size
  and utilisation beyond the coarse figures.
- **Other pool users** learn the price indices at their own operations, so they can estimate
  utilisation over time and notice very large operations.
- **The keepers** and any holder of the pool key see all amounts of all pool users. The session
  text every wallet signs says so.
- **A liquidated owner** can derive the blinding of the liquidator's change note, because
  `liquidate` seeds every output's blinding from the position, whose opening the owner holds. The
  keeper liquidates from a note of exactly `x` so that the change note is zero; this is not
  enforced.
- **Intent expiry** is a private value with no upper bound in the circuit: a far-future expiry
  locks the maker's own funds until then, since `cancel_intent` waits for it and a keeper fills
  only an intent matching one of its tickets. The page uses the ticket's expiry; an intent made by
  hand with another one is the maker's own risk.
- **The Worker** sees account views in clear (review W-3).

## Differences from Jupiter Lend

1. Liquidation is done by the keeper set, not open to anyone.
2. Liquidation is per position, not a tick engine.
3. Above the max limit the liquidator takes collateral worth the whole debt plus the penalty and
   the rest stays with the owner, where Jupiter Lend absorbs the whole position.
4. Deposit and repay need no price and are never refused for one; a debt-free position can be
   closed without a keeper.
5. Earn shares are a private receipt, not a transferable token.
6. Collateral is not lent out and earns nothing.
7. No expanding limits, rewards, flash loans or smart vaults.
8. One operation at a time per pool.
9. Parameters are compiled into the program; changing them is an upgrade.

## Wire interfaces

The keeper's HTTP routes (`GET /lend/pools`, `POST /lend/ticket`, `POST /lend/build`,
`POST /lend/account`; loopback only, `x-keeler-key`, 16 KiB bodies, 30 s deadline), the session
authorisation and request signatures, ticket bytes, the Worker's `/api/lend/*` forwarding and the
page's build settings (`VITE_LEND_PROGRAM_ID`, `VITE_LEND_KEEPERS`, `VITE_LEND_POOL_VIEWING`) are
specified in `docs/lend-api.md`.

## Known limits

- The keepers see every amount, order every operation and can halt the pool; there is no key
  rotation for the pool key.
- One lane per pool: a few operations a minute.
- Wallets are public on every operation until custom rings are live.
- No liquidation buffer beyond accrued interest fees; a new pool has none until fees accrue, so
  the caps and the borrowing halt are its only protection.
- The Pyth push account is sponsored by a third party; if it stalls, draws and liquidations stop.
- On devnet the proving keys are test keys (anyone can forge a proof) and a keeper sets the test
  price.

The full list, with what enforces each rule, is `docs/threat-model.md`; the internal security
review and its decisions are `docs/audit-2026-11-25.md`.

## Not in this version

Hidden wallets; open liquidation; a transferable share token; yield on collateral; more pools and
vaults; expanding limits; rewards; flash loans; changing parameters without an upgrade; pool-key
rotation; a write-off for bad debt; a reserve share of the liquidation penalty.
