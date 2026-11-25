# Deploying Keeler Lend to devnet

`scripts/deploy-lend-devnet.sh [--estimate] [name]` deploys the lend program as deployment `name`
(default `lend`). It spends SOL from the deploy key: run `--estimate` first, which sends
nothing.

## Before

- The swap is on devnet: `target/devnet/deploy-summary.json` from `scripts/deploy-devnet.sh`. Its
  dealer is the lend keeper, and its tSOL and tUSD are vault 1's collateral and debt.
- Keychain: `keeler-devnet-deploy` (funded; it pays, is the upgrade authority and is the program's
  admin), `keeler-devnet-dealer` (the first keeper; the script refuses if it is not the summary's
  dealer), `keeler-helius-api-key`. Optional: `keeler-devnet-<name>-standby`, a second keeper
  compiled in after the dealer (it must not be the dealer's key).
- `.tools/env` in the main checkout (`scripts/setup.sh`).

## What it does

1. Creates `keeler-devnet-<name>-pool`, `keeler-devnet-<name>-program` and
   `keeler-devnet-<name>-buffer` in the Keychain if absent. The pool key is its own key (not a
   keeper's, not the deploy key): its viewing key is compiled in as the pool viewing key, and
   whoever holds it reads every pool amount, so a keeper can be removed without holding it.
   `--estimate` creates none of them: with one missing it builds with stand-in addresses under
   `target/devnet-<name>/estimate/`, prints the cost and records nothing.
2. Builds the program once (`scripts/setup-lend.sh` with `KEELER_LEND_OUT`) with the program ID,
   admin, keeper list (dealer first, then the standby if any), pool viewing key and mints compiled in, and keeps `target/devnet-<name>/keeler_lend.so`. Reruns
   deploy those bytes; `program-id` records what they were built for. It refuses if the build
   changes a tracked file. The circuits and verifying keys do not depend on these parameters: the
   build checks them against `lend/lend-keys.CHECKSUM`, so the browser package is the local one.
   `build-params.json` records `program_id`, `admin`, `keepers` (a list), `pool_viewing` and the
   mints; a rerun refuses if they
   have changed since (another Keychain item or swap deploy), and `scripts/run-dealer.sh` builds
   the keeper from this file, not from the Keychain.
3. Copies the proving keys from `lend/target/zk/keeler-lend` to `target/devnet-<name>/keys/`
   once, checked against `lend/lend-keys.CHECKSUM` (and checked again on every rerun). The keeper
   reads these (`KEELER_LEND_KEYS_DIR`), so a later `scripts/setup-lend.sh` cannot change them
   under it.
4. `deploy estimate` (with `--estimate`, then stops) or `deploy program`: checks devnet's genesis
   hash, prints the rent, fees and peak, and sends nothing if the deploy wallet holds less than the
   deploy still needs. The deploy goes through a buffer and resumes after any failure; a finished one
   sends nothing. `summary.json` gets the program ID and upgrade authority.

Everything it writes is under `target/devnet-<name>/`. The tool builds in its own cargo target
there, so the live dealer's `target/release` is not touched. `build/` and `cargo/` can be deleted
after a deploy; `keeler_lend.so`, `program-id`, `build-params.json` and `keys/` must stay, for
reruns and the dealer. A deployment made before `build-params.json` and `keys/` existed gets both
from a rerun with `--estimate` (which sends nothing). A buffer left from a deploy of other bytes
cannot be resumed: the tool refuses and names it, and the way on is another deployment name (a
fresh buffer key); its rent stays locked until it is closed by hand.

## Cost

Rent scales with the size of the `.so`, so the cost changes with every build: `--estimate` prints
the peak for the build at hand. That peak is the buffer rent, which comes back when the deploy
closes the buffer, plus the program data rent, the program account's rent and the fees; the
program data rent stays spent, about half the peak. Run `--estimate` again after every rebuild.

## The price on devnet

The devnet build has the `test-price` feature (not a default: `scripts/setup-lend.sh` passes it,
and it cannot build with `mainnet`). The vault's Pyth account and feed stay at their defaults and are not used: `draw`
and `liquidate` are given the vault's test price account, the PDA `["price", vault id (u64 LE)]`.

The keeper sets the devnet price with the `set_test_price` command (instruction tag 12,
`keeler_lend_sdk::vault::set_test_price`): price in debt base units per collateral base unit x
1e12 (tSOL at 150 tUSD is `150_000_000_000`), a confidence in the same units, and the publish time.
The first call creates the account, with the keeper paying its rent. `draw` and `liquidate` refuse a
price older than 90 seconds; the running keeper re-posts it every `price_refresh_secs` (default 45,
`docs/lend-keeper.md`), each post a transaction fee. `keeler lend-set-price` sets the value the
keeper keeps posting; moving it down is how a liquidation is triggered by hand.

## After: the checklist

In order. **SOL** marks a step that spends SOL from a Keychain key; **public** one that changes what
others see or use. Nothing here is done by the deploy script.

1. **Keeper config.** Add a `lend` section to the dealer config (`docs/lend-keeper.md`):
   `program_id` from `target/devnet-<name>/summary.json`, `network` equal to the Worker's
   `NETWORK` (`devnet`), `pool_key` `{"keychain": "keeler-devnet-<name>-pool"}` (required:
   `scripts/run-dealer.sh` refuses any other), `origins` (required: the site's origin, for example
   `["https://<your site>"]`), `admin_key` the `keeler-devnet-deploy` item, `state_file`,
   `pause_file`, `price: "test"`, `price_refresh_secs` (45), `limits`. Leave `lend.key` out: the devnet
   keeper is then the dealer key (the config's `key`); `scripts/run-dealer.sh` accepts a `lend.key`
   only when its address is one of the program's keepers. Check that `limits.min_*` are not below the
   vault's minimums in `LEND_VAULTS`. A standby needs no config of its own:
   `scripts/run-dealer.sh --standby` writes it (`target/devnet/standby-keeper.json`: this `lend`
   section with `role: "standby"`, the key `keeler-devnet-<name>-standby`, its own default state
   file and the same `pause_file`, so one `touch` pauses both).
2. **Reserve** (**SOL**, once): `keeler lend-init-reserve <keeper config>`. This command and
   `lend-set-price` take a keeper config, not the dealer config (which they refuse): a file with
   only the dealer config's `network`, `key`, `http` and `lend`, and the same `state_file`, run with
   the binary and `KEELER_LEND_KEYS_DIR` that `scripts/run-dealer.sh` uses
   (`target/devnet-<name>/dealer-cargo/release/keeler`, `target/devnet-<name>/keys`; run
   `scripts/run-dealer.sh` once to build it). The keeper pays the fee and the admin co-signs. A
   second run is refused by the program (the reserve's address is taken), so it costs only a fee.
3. **First test price** (**SOL**: the account's rent and a fee): `keeler lend-set-price <keeper config> 1
   <price> <confidence>`, for example `150000000000 150000000` for 150 tUSD per tSOL with 0.1 %
   confidence. It also writes the control file the keeper keeps posting.
4. **Start the keeper** (**SOL**: a fee per price refresh, about one every 45 s, and the fees of
   its fills and liquidations): `scripts/run-dealer.sh <config>`. Check its log shows the price
   being posted and `GET /health` answers.
5. **Fund every keeper's debt token** (moves tokens): each keeper liquidates from notes at its own
   lend address, so the dealer's keeper and the standby each need enough tUSD for liquidations (at
   least the largest position's debt): `keeler lend-fund <keeper config> <amount>` shields it,
   `keeler lend-balance <keeper config>` shows it, and `keeler lend-sweep <keeper config>`
   unshields the tSOL liquidations paid. Keep each keeper's SOL topped up too.
6. **Seed the pool** (**SOL**, moves tokens, **public**: the pool's figures change): a first
   supply from a lender wallet through the page or the SDK, at least `minSupply`, so there is
   something to borrow.
7. **Worker variables** (**public**): set `LEND_PROGRAM_ID`, `LEND_POOLS` and `LEND_VAULTS`
   (`worker/README.md`; the pool's curve, fee, max utilisation and minimum, and the vault's
   factors and minimums exactly as compiled into the program), `LEND_KEEPERS` (a JSON list of
   `{"url","pubkey"}`, in the compiled order, each `https://`; the script prints its shape, and
   `scripts/run-dealer.sh` keeps it current), keep the keeper as the
   first `DEALERS` entry, and `REFERENCE` or `PYTH_ACCOUNTS` for the page's fallback price. Optionally
   the `LEND_*_LIMIT` rate limits. Deploy the Worker.
8. **Site build** (**public**): `VITE_NETWORK=devnet`, `VITE_DEALERS`, `VITE_LEND_PROGRAM_ID` (the
   program ID), `VITE_LEND_KEEPERS` (the JSON list of keeper pubkeys from `build-params.json`) and
   `VITE_LEND_POOL_VIEWING` (its `pool_viewing`), and `VITE_ASSETS` (tUSD and tSOL with their mints
   and decimals, from the swap deploy's summary; the symbols must equal the Worker's `ASSETS`); the
   script prints all four at the end. Then `pnpm build` in `web/` and
   deploy. The page reads the pools and vaults from `/api/config`.
9. **Check**: open the Earn tab (the pool shows its figures and the price), make a small deposit,
   borrow, repay all and close from a second wallet.
