# Deploying Keeler Lend to devnet

`scripts/deploy-lend-devnet.sh [--estimate] [name]` deploys the lend program as deployment `name`
(default `lend`). It needs the owner's go-ahead for the SOL: run `--estimate` first, which sends
nothing.

## Before

- The swap is on devnet: `target/devnet/deploy-summary.json` from `scripts/deploy-devnet.sh`. Its
  dealer is the lend keeper, and its tSOL and tUSD are vault 1's collateral and debt.
- Keychain: `keeler-devnet-deploy` (funded; it pays, is the upgrade authority and is the program's
  admin), `keeler-devnet-dealer` (the keeper; the script refuses if it is not the summary's dealer),
  `keeler-helius-api-key`.
- `.tools/env` in the main checkout (`scripts/setup.sh`).

## What it does

1. Creates `keeler-devnet-<name>-program` and `keeler-devnet-<name>-buffer` in the Keychain if absent.
2. Builds the program once (`scripts/setup-lend.sh` with `KEELER_LEND_OUT`) with the program ID,
   admin, keeper and mints compiled in, and keeps `target/devnet-<name>/keeler_lend.so`. Reruns
   deploy those bytes; `program-id` records what they were built for. It refuses if the build
   changes a tracked file. The circuits and verifying keys do not depend on these parameters: the
   build checks them against `lend/lend-keys.CHECKSUM`, so the browser package is the local one.
3. `deploy estimate` (with `--estimate`, then stops) or `deploy program`: checks devnet's genesis
   hash, prints the rent, fees and peak, and sends nothing if the deploy wallet holds less than the
   deploy still needs. The deploy goes through a buffer and resumes after any failure; a finished one
   sends nothing. `summary.json` gets the program ID and upgrade authority.

Everything it writes is under `target/devnet-<name>/`. The tool builds in its own cargo target
there, so the live dealer's `target/release` is not touched. `build/` and `cargo/` can be deleted
after a deploy; `keeler_lend.so` and `program-id` must stay for reruns.

## Cost

The devnet build of 2026-11-24 was 151,056 bytes. `--estimate` gave a peak of 1.538139120 SOL:
0.768202680 SOL of buffer rent, which comes back when the deploy closes the buffer, plus
0.768243320 SOL of program data rent, 0.000833120 SOL of program rent and 0.000860000 SOL of fees.
About 0.77 SOL stays spent. A rebuild changes the size, so run `--estimate` again before asking
for SOL.

## The price on devnet

The devnet build has the `test-price` feature (a default feature of `lend/program`; never in a
mainnet build). The vault's Pyth account and feed stay at their defaults and are not used: `draw`
and `liquidate` are given the vault's test price account, the PDA `["price", vault id (u64 LE)]`.

The keeper sets the devnet price with the `set_test_price` command (instruction tag 12,
`keeler_lend_sdk::vault::set_test_price`): price in debt base units per collateral base unit x
1e12 (tSOL at 150 tUSD is `150_000_000_000`), a confidence in the same units, and the publish time.
The first call creates the account, with the keeper paying its rent. `draw` and `liquidate` refuse a
price older than 90 seconds, so the keeper must refresh it at least that often. Moving it down is how
a liquidation is triggered by hand.

## After

The program still needs its reserves and vault set up by the keeper, and the Worker needs
`LEND_PROGRAM_ID`, `LEND_POOLS` and `LEND_VAULTS` (`docs/lend-api.md`). The keeper is the dealer
(the Worker forwards `/api/lend/*` to the first `DEALERS` entry).
