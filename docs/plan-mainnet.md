# Mainnet readiness: the code that must exist before the audit (revised after review, 2026-11-25)

Not covered here (owner's side, or after the audit): licence, audit, the key
ceremony, the multisig's signers, the always-on hosts, real inventory, a
custom domain, a pass with real wallet extensions. Nothing in this plan is
deployed to mainnet; the result ships to devnet.

## Goal

1. Lending prices come from the real Pyth account, with limits set from how
   that account behaves on mainnet.
2. More than one keeper: any keeper in a fixed set can run the pool, a second
   takes over when the first stops, and a liquidation does not lose money.
3. A mainnet deploy path that cannot ship test keys, test prices, a faucet or
   a single-key upgrade authority, with a canary's caps.
4. The contract changes deferred from the devnet review.

## Facts checked (2026-11-25; ✓ = the reviewer re-checked)

- The program reads Pyth already: `instructions/vault.rs` `pyth_quote` (owner
  = receiver, `PriceUpdateV2`, full verification, pinned feed id, age);
  `test-price` cannot compile into a `mainnet` build; the keeper has
  `price: "pyth"`. One age limit, `PRICE_MAX_AGE = 90` s; the keeper's own
  are 60 s (draw) and 75 s (liquidate). ✓
- Pyth's SOL/USD push account `7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE`
  on mainnet: receiver-owned, fully verified, right feed ✓. A 15-minute
  sample (271 reads): 17 updates, gap between publish times median 54 s,
  max 61 s; age p50 29 s, p99 56 s, max 60 s. It updates about once a
  minute. On devnet the same account was up to 3,487 s old.
- Anyone may post a fully verified update newer than the stored one to the
  push account (Pyth's `update_price_feed`); it need not be the latest. ✓
  So an age limit is also a window for choosing a price.
- The keeper is one compiled address, a program check, not a circuit input
  (`require_keeper`, nine sites). ✓
- `kp` (the keeper's shielded keypair) derives from its Solana key; history
  is read by the keeper's own address (`lend_chain.rs` `history`). ✓
- Notes written by one keeper can be opened and spent by another that shares
  the viewing key: PDA notes use the all-zero nullifier key, the note hash
  commits to the owner not the viewing key, the writer key is HKDF(viewing
  secret, first nullifier), and `ShieldedKeypair::with_viewing_key` gives
  each keeper its own signing and nullifier keys under a shared viewing key
  (spr 0.10.0 sources). ✓
- The page takes the pool viewing key from the user-registry entry of the one
  compiled keeper (`web/src/keeler/lend-chain.ts:94-109`,
  `client.ts:411-416`). ✓ `pool_viewing` is a free instruction argument. ✓
- `deposit_open` and `deposit_add` do not spend the reserve. ✓
- `liquidate`: the liquidator pays `x + floor(x p / 1e4)` and gets collateral
  worth that at the bound; the `x p` part stays in the reserve. The interest
  fee is never paid out and is already a buffer. ✓
- `lend/program/build.rs` under `mainnet` refuses a parameter equal to its
  default, but the real SOL mint and the real price account are the
  defaults; that branch has never been built. ✓
- The Worker has one lend upstream; a live keeper answers 503 on an internal
  error (`keeper.rs:718-721`). ✓ No transaction sets a priority fee. ✓

## Design

### 1. Prices

- Keep the pinned push account and the single 90 s limit for draw and
  liquidate (no program change). A looser liquidation limit would widen the
  window in which the liquidator, who is now paid, can choose a price.
- Keeper limits move to fit a feed that updates once a minute: a draw is
  built with a price at most 65 s old, a liquidation at most 80 s. In `pyth`
  mode a stale or unverified account answers 412 to draws, liquidations skip
  the pass and log once a minute; no other source is ever used.
- The debt token stays valued at 1 USD (a depeg below 1 makes liquidations
  early, never late; FAQ and runbook say so).
- Test on a local mainnet fork (surfpool, `SURFPOOL_BIN` in `.tools/env`)
  with the real account: a draw, a liquidation, and the age refusal.
- Runbook: how to refresh the push account by hand with Pyth's own pusher
  when the sponsor stalls.

### 2. A keeper set

Program (no circuit change):

- Build parameters, each read by `build.rs`: `KEELER_LEND_KEEPERS` (1 to 4
  base58 addresses, comma separated, no duplicates; replaces
  `KEELER_LEND_KEEPER`) and `KEELER_LEND_POOL_VIEWING` (66 hex characters:
  the pool viewing public key, 33 bytes). The program exposes
  `KEEPERS: &[Address]` and `POOL_VIEWING: [u8; 33]`.
- `require_keeper` accepts a signer that is any of `KEEPERS`.
- Every instruction that takes `pool_viewing` refuses unless it equals
  `POOL_VIEWING` (new error), so no keeper can write a reserve its peers
  cannot open by using another key.
- Default (local test) build: two fixture keepers and a fixture pool key that
  is neither's.
- `build.rs` under `mainnet`: every parameter must be set explicitly (set,
  not "different from the default"), and no admin or keeper may be a public
  fixture key. The validation is a plain function with unit tests: real
  SOL/USDC parameters pass, each fixture key fails, a duplicate keeper fails.
- The set and the pool key change only by a program upgrade.

Pool key:

- Keeper config `lend.pool_key` (a `KeySource`; default: the keeper key, for
  old configs on local networks). The pool viewing key derives from it; each
  keeper keeps its own signing and nullifier keys
  (`ShieldedKeypair::with_viewing_key`). A keeper refuses to start unless its
  viewing public key equals the compiled `POOL_VIEWING`, and on mainnet
  unless the pool key's address is not in `KEEPERS`.
- The page encrypts intents to the compiled pool viewing key, not to a
  keeper's registry entry.
- `lend.key` may differ from the dealer key; `run-dealer.sh` checks it is in
  the build parameters' keeper list instead of refusing it.
- Peers are `KEEPERS` minus self; there is no `peers` setting.

Following each other:

- `history` reads the transactions paid by every keeper in `KEEPERS`, with
  one scan mark per keeper address (`Book.scanned` becomes a map; an old
  single mark loads as this keeper's). Peers' new signatures are read every
  pass, not only when a reserve is lost, because deposits do not spend the
  reserve.
- When a note the keeper holds is spent by a transaction it has no pending
  build for, it drops its builds on that note and takes the peer's outputs
  from the history.

Roles and failover:

- `lend.role`: `primary` (default) or `standby`. A standby issues tickets and
  builds whenever asked, and liquidates a position only after it has been
  past the threshold for `standby_grace_secs` (default 45).
- Tickets: a ticket signed by any key in `KEEPERS` is valid for every keeper.
  The page sends its open tickets with every account read; a keeper that
  does not know a valid, unexpired peer ticket adopts it and fills its
  intent. So a repay made through a dead primary is filled by the standby.
- Racing keepers spend the same reserve: one transaction fails on the
  nullifier and costs a fee.
- Ticket and build replies carry `keeper` (the signer's address).
- Worker: `LEND_KEEPERS`, an ordered list `[{"url","pubkey"}]` (falls back to
  today's settings when unset). A lend request goes to the first; it is
  retried once on the next only when there was no response or the reply is
  not the keeper's JSON (not on a keeper's own 5xx). `LEND_PAUSED=1` refuses
  new tickets and builds at the Worker for all keepers; account reads, repay
  and deposit-add tickets still pass. A retried ticket request may leave an
  unused ticket on the first keeper; it expires.
- Page: the accepted keepers and the pool viewing key are compiled into the
  site (as the single keeper is today); `/api/config.lend.keepers` must
  match or lending stays hidden. A keeper-built transaction and a ticket are
  accepted from any compiled keeper, and the transaction's fee payer must be
  the `keeper` the reply names.
- Devnet test price: the standby re-posts it only when the account is 15 s
  older than `price_refresh_secs`.
- Priority fees: `network.priority_fee` (micro-lamports per compute unit,
  default 0) on every keeper and dealer transaction and
  `lend.liquidation_priority_fee` for liquidations; the page's signing check
  already allows compute-budget instructions and keeps doing so.

Liquidation pays its cost (the one circuit change):

- The liquidator transfers exactly `x` and receives
  `floor((x + floor(x p / 1e4)) × 1e12 / bound)` of collateral, capped at
  the position's. The formula for the payout is unchanged, so the sizing
  proof in `core::lend::liquidate` stands; only the funding changes. Nothing
  is paid into the reserve. The borrower's cost is unchanged (1%).
- `core::lend::liquidate`, its TypeScript twin, the vectors and the circuit
  change together; `Liquidation.buffer` goes away.
- Funding: the keeper's liquidation spends a note of exactly `x` (split or
  merged first, with `now` held across the two transactions); code and
  `docs/lend-keeper.md` must agree, with a test.
- Bad debt has no buffer from liquidations any more. The keeper computes, each
  pass, surplus (idle + debt − supply value) and bad debt (debt on positions
  whose collateral is worth less at the price), logs both, shows them in
  `/lend/pools` coarsely, and refuses new borrowing while bad debt exceeds
  surplus. Accrued fees are never withdrawn: they are the buffer.
- Keeper limit `min_position_debt` (a draw must leave no debt or at least
  this): the runbook sets it so a liquidation's 1% covers its fee. The
  circuit minimums stay as they are.

Trust statement (FAQ, runbook): the pool keeps working when any one keeper
is lost, and the multisig can add a keeper and hand it the pool key. A keeper
can still halt the pool by withholding or corrupting what it writes (the
ciphertext is unchecked until the custom ring), so the set is hosts the
operator controls. With every pool-key holder gone, Earn deposits and
indebted positions cannot move; intents refund and debt-free positions close
without a keeper.

### 3. Mainnet deploy path and canary

- Keeper caps in `lend.limits`: `max_pool_size` (supplied total including
  open supply tickets, rechecked at fill), `max_total_collateral`,
  `max_position_collateral`, `max_position_debt`, `max_supply_per_wallet`.
  A keeper whose network is mainnet refuses to start with any of them 0.
- Keeper and dealer check the cluster's genesis hash against their `network`
  at start, and on mainnet refuse a `faucet` section, `price: "test"` and a
  key read from a file.
- `scripts/deploy-mainnet.sh <swap|lend> [--estimate]`. It owns the mainnet
  build (it does not use `setup-lend.sh`, which builds test keys). It
  refuses unless: the tree is clean; the genesis hash is mainnet's; every
  build parameter is given explicitly; the built `.so`'s embedded
  verifying-key markers (`VERIFYINGKEY_SETUP_TXT`: `insecure_test_setup`,
  `proving_key_sha256`) say a real setup and match a ceremony manifest passed
  on the command line; the `.so` does not contain the test-price marker (a
  constant string compiled only with `test-price`, added by piece A); the
  new upgrade authority is a Squads vault whose multisig account has
  threshold at least 2 (read from chain; verify the layout against Squads'
  published source, do not assume). After the deploy it repeats the marker
  checks on the deployed program data. Order: estimate, deploy, verify, hand
  over the authority and read it back, then (lend) create the reserve. It
  records the commit and the `.so` sha256. No token, faucet or inventory.
  Tests: `--estimate` against mainnet read-only; each refusal, with the
  devnet `.so` as the negative case for the markers.
- `docs/mainnet-runbook.md`: freeze, audit, ceremony, deploy, multisig, two
  keeper hosts with one pool key, caps, `min_position_debt`, priority fees,
  kill switches (`LEND_PAUSED`, pause files, `PAUSED`), the Pyth refresh,
  adding or replacing a keeper (it replays the history; a removed keeper
  keeps reading: there is no key rotation), raising caps.
- Page on `mainnet`: no faucet and no in-page test wallet (verify; add tests
  if missing); network-dependent FAQ answers read the network.

### 4. Contract changes folded in

- Session text: after the `Network:` line, two lines: `Origin: <origin>` and
  `The keeper can see your lending amounts.` The session object gains
  `origin` (the page's `location.origin`); the Worker refuses one that
  differs from the request's `Origin` header; the keeper refuses one not in
  `lend.origins`. `web/tests/fixtures/lend-requests.json` is regenerated by
  piece C and consumed by B.
- `GET /lend/account` becomes `POST` with the signed fields in the body, plus
  `tickets` (the wallet's open tickets, see above).
- Quote signature v2: `"keeler-quote-v2"` (15 bytes) ‖ network (1 byte:
  0 local, 1 devnet, 2 mainnet) ‖ the v1 fields unchanged; 168 bytes. v1 is
  dropped. `core/src/rfq.rs` and `web/tests/fixtures/quote-sig.json` belong
  to piece B; C implements the same layout and takes B's fixture at
  integration.
- Not done (dropped after review): merging the 410 and 423 refusals.
- Page: a notice when a full liquidation leaves a position at 0 / 0; the
  liquidation note no longer credits the liquidation with a later repay.

## Work split

Phase 1, in parallel:

A. **Program, SDK, shared math, keys.** `lend/` (all of it),
   `core/src/lend.rs`, `web/src/keeler/lend-math.ts` and its vector files,
   `scripts/setup-lend.sh`. Keeper set, `POOL_VIEWING` and its check, the
   test-price marker, `build.rs` validation, the liquidation funding change,
   regenerated test keys. First commit: the interface only (`KEEPERS`,
   `POOL_VIEWING`, `Liquidation` without `buffer`, SDK signatures). Tests:
   mollusk for each keeper in the set and one outside it, a wrong
   `pool_viewing` on every instruction that takes it, the new liquidation;
   SDK chain tests; the vectors in Rust and TypeScript.
C. **Worker and page.** `worker/`, `web/` except `lend-math.ts` and its
   vectors and `web/tests/fixtures/quote-sig.json`. Keeper list, failover
   and `LEND_PAUSED`; compiled keepers and pool viewing key; intents
   encrypted to the pool key; ticket and build acceptance; tickets sent with
   account reads; session text and `origin`; account `POST`; quote v2;
   mainnet gating; the two notices; `lend-requests.json`. Tests: Worker
   (failover only on no reply, no double charge of limits, pause), page
   unit, mock browser.

Phase 2, from A and C merged, in parallel:

B. **Keeper and dealer.** `cli/`, `core/src/rfq.rs`, `scripts/run-dealer.sh`
   (also able to run a standby keeper with its own tunnel and publish
   `LEND_KEEPERS`), `scripts/lend-web-stack.sh`, `scripts/run-lend-e2e.sh`,
   `web/tests/fixtures/quote-sig.json`, `docs/lend-keeper.md`,
   `docs/lend-api.md`. Everything under "Pool key", "Following each other",
   "Roles and failover" (keeper side), priority fees, caps, genesis checks,
   bad-debt figures, `min_position_debt`, pyth-mode limits, session text and
   origin, account `POST`, quote v2. Tests: unit; a local two-keeper
   end-to-end with a pool key that is neither keeper's: primary fills a
   supply, a deposit, a top-up and a borrow; the user gets a repay ticket
   from the primary and makes the intent; the primary is killed; the standby
   adopts the ticket and fills the repay, serves a draw on the topped-up
   position and a redeem, and liquidates after the grace when the price
   drops; the primary restarts and merges; the surfpool mainnet-fork price
   test.
D. **Deploy path and runbook.** `scripts/deploy-mainnet.sh`,
   `scripts/deploy-lend-devnet.sh`, `scripts/set-upgrade-authority.sh`,
   `cli/src/deploy.rs` (only this file in `cli/`; B does not touch it),
   `docs/mainnet-runbook.md`, `docs/lend-deploy.md`, README.
   `build-params.json` is `{program_id, admin, keepers: [..], pool_viewing,
   collateral_mint, debt_mint, price_account, feed_id}`; D writes it, B reads
   it.

Integration (main session): branch `feat/mainnet-ready` from `feat/lend`;
merge A and C, then B and D; every suite and the real-stack browser suite;
PR to `main`; fresh Fable review of the diff; fixes in one pass; a new devnet
deployment (the deploy tool does not upgrade) with a pool key that is no
keeper's, the dealer as primary and a second keeper key as standby on this
Mac; by hand: deposit, borrow, liquidation by moving the test price, stop
the primary, repay and redeem through the standby.

## Open for the owner (changeable until the freeze; both change a circuit)

- The whole 1% penalty goes to the liquidator. The alternative is a larger
  penalty split between liquidator and reserve.
- There is no write-off for bad debt: it stays on the books and the last
  suppliers to leave bear it, as in `docs/plan-lend.md`.

## Risks

1. The liquidate circuit changes: new test keys, a new devnet program
   (about 0.8 devnet SOL).
2. Two keepers on one pool is new behaviour in the riskiest file; the
   two-keeper end-to-end test is the gate.
3. The pool key is one more secret; whoever has it reads every amount, and
   any keeper can still halt the pool.
4. The Pyth account updates once a minute and a third party sponsors it; if
   it stalls, draws and liquidations stop until someone posts an update.
5. `deploy-mainnet.sh` cannot run end to end before the ceremony.
6. A new pool has no bad-debt buffer until interest fees accrue; the caps and
   the borrowing halt are the only protection in the canary.
