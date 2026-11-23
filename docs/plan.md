# Keeler v1 plan: private quote router on a local network

Revised after plan review (23 Nov 2026).

## What it is

A trader wants to sell `sell_amount` of one asset for another without showing
size or price on-chain. Keeler asks several dealers for a quote, drops quotes
that are far from a reference price, and settles the best one through the
Helius Privacy Rings swap example program (Xark implementation).

Words used here: **trader** asks and locks funds (the swap program's "maker");
**dealer** quotes and settles (the swap program's "taker").

## Scope

In: Rust CLI with three commands (`keeler swap`, `keeler dealer`,
`keeler refund`), a quote protocol, a reference-price check, settlement and
refund on a local network, tests.

Out (named so nobody builds them): any new on-chain program or circuit, a web
UI, devnet or mainnet, partial fills, more than one hop, hiding who traded,
TLS on the quote protocol, dealers reserving balance per quote.

No on-chain Pyth check: the trader commits the price itself when it locks the
order, so the price check protects the trader just as well in the client. A
dealer that dislikes the price simply does not settle.

## Constraints found

- `helius-labs/privacy-ring-examples` has no licence. We do not copy its code.
  `scripts/setup.sh` clones it at commit
  `9e5d69ade7e0c107a2929306d3b758a8969e1089` into `.tools/` (git-ignored) and
  the `cli` crate uses `swap-sdk` and `swap-program` as path dependencies from
  that checkout. A cargo git dependency is not used because that repo has two
  packages named `swap-sdk`.
- The example program uses forgeable test proving keys and its own example
  program ID. Local network only. `proof.rs` finds the keys through
  `SWAP_KEYS_DIR` or a compile-time path into the checkout, so the binary
  needs `.tools/` at run time.
- The SDK is blocking. Transactions are v1 messages with a 1,400,000
  compute-unit limit.
- `take` is two inputs, two outputs, no change: the dealer must spend a note
  of exactly `buy_amount`. `make` spends one source note: the trader needs one
  note that covers `sell_amount`.
- The program checks expiry against the chain's Clock sysvar, not wall time.
- `index_taker` rescans all history each call, returns every order ever sent
  to the dealer (taken, cancelled and expired included), blocks until it has
  at least one, and errors on timeout. `index_maker` likewise returns every
  order the trader ever made.
- The SDK only supports tree id 0 and the local prover (`ZOLANA_PROVER_URL`).
- Both wallets and both asset IDs are public at settlement. Amounts and price
  are private. Each dealer that is asked learns the pair, the size and the
  trader's public key.

## Time

Every "now" is chain time, read from the Clock sysvar account over RPC:
quote expiry, the TTL filter, the dealer's settle margin and the trader's
wait. `core` takes `now` as an argument and never reads a clock.

## Flow

1. Trader checks it has one note covering `sell_amount` (else error, nothing
   sent), registers itself (`ensure_registered`) and sends `Rfq { sell_mint,
   buy_mint, sell_amount, trader }` to every configured dealer (TCP, one JSON
   line each way, 2 s timeout per dealer, one thread per dealer).
2. Dealer answers `{"quote": { buy_amount, expiry }}` or `{"error": "..."}`.
   It quotes from a cached balance (refreshed by its poll loop), refuses if
   the cached `buy_mint` balance is below `buy_amount`, and remembers the
   quote. The book is pruned of expired quotes on every insert and capped at
   1,024 entries (refuse when full).
3. Trader computes the fair amount from the reference price, keeps quotes
   within `band_bps` of fair on both sides and with `min_ttl_secs <= expiry -
   now <= max_ttl_secs`, and picks the largest `buy_amount` (lowest dealer
   index wins a tie). No quote left is an error; nothing is sent on-chain.
4. Trader locks the order: `MakeTransactionParams` with `OrderTerms {
   destination_mint, destination_amount: quote.buy_amount, maker: trader
   shielded address, expiry: quote.expiry, taker:
   Some(identity(dealer_pubkey)?) }`, `source_amount = sell_amount`,
   `taker_address` = the dealer's shielded address resolved from the user
   registry by the dealer public key in the trader's own config (never from
   the quote reply).
5. Dealer loop: `index_taker` (timeout counts as empty), then sleep. It keeps
   a seen-set keyed by order UTXO hash. For each unseen order:
   - spent already (nullifier lookup) → mark seen;
   - matches a remembered quote exactly (trader public key, `source_mint`,
     `source_amount`, destination mint and amount, expiry) and `now +
     settle_margin_secs < expiry` → bind that quote to this order hash;
   - otherwise → mark seen, log, never settle.
   For a bound order: if no note of exactly `buy_amount` exists, self-transfer
   to make one; fetch the leaf index with `get_merkle_proofs`; then
   `TakeTransactionParams` → `prove` → `send`. On failure retry on the next
   loop until the margin passes. On success mark seen and drop the quote.
6. Trader waits on the order nullifier
   (`get_shielded_transactions_by_nullifiers`, nullifier from
   `order_utxo.to_input_utxo(tree_id, leaf).nullifier`). Spent and a merkle
   proof exists for `derived_destination_output(nullifier).hash(tree_id)` →
   settled. Not spent and `now > expiry` → refund.
7. Refund (`keeler refund`, also what `swap` calls): `index_maker`, skip spent
   orders, cancel every unspent order whose expiry has passed
   (`CancelTransactionParams`). If a cancel fails, re-check the nullifier:
   spent → report what happened; unspent → retry, then error with the order
   hash. Never report a settlement that the nullifier check did not show.

## Reference price

Integer math in `u128`, checked, no floats.

- `Price { price: i64, expo: i32 }` per asset in USD.
- `fair_buy_amount(sell_amount, sell, buy, sell_decimals, buy_decimals) ->
  Option<u64>` = `sell_amount * sell.price * 10^e / buy.price` with `e =
  sell.expo - buy.expo + buy_decimals - sell_decimals` (either sign: multiply
  or divide), floored. `None` on overflow, non-positive price or zero result.
- `within_band(buy_amount, fair, band_bps)`: `buy * 10_000 >= fair * (10_000 -
  band_bps)` and `buy * 10_000 <= fair * (10_000 + band_bps)`. `band_bps` and
  `spread_bps` above 10,000 are config errors.
- `apply_spread(fair, spread_bps)` = `fair * (10_000 - spread_bps) / 10_000`,
  floored: the dealer's quote.
- Price per asset in the config is either fixed `{ "price", "expo" }` or
  `{ "pyth": feed_id }`. Pyth prices come from Hermes
  (`/v2/updates/price/latest?ids[]=..&parsed=true`); `parse_hermes(json, now,
  max_age_secs, max_conf_bps)` in `core` rejects stale prices and prices whose
  confidence is too wide. The HTTP call lives in `cli` (reqwest 0.12,
  blocking). Hermes staleness uses wall time; nothing else does.
- Decimals are read from the mint account; SOL is 9.
- A mint with no configured price is an error. There is no flag to skip the
  check.

## `core` API (the contract piece C builds on)

```rust
// price.rs
pub struct Price { pub price: i64, pub expo: i32 }
pub fn fair_buy_amount(sell_amount: u64, sell: Price, buy: Price, sell_decimals: u8, buy_decimals: u8) -> Option<u64>;
pub fn within_band(buy_amount: u64, fair: u64, band_bps: u16) -> bool;
pub fn apply_spread(fair: u64, spread_bps: u16) -> Option<u64>;
pub fn parse_hermes(json: &str, now: i64, max_age_secs: i64, max_conf_bps: u16) -> anyhow::Result<Vec<(String, Price)>>; // (feed id hex, price)

// rfq.rs — mints and keys are base58 strings; core has no Solana dependency
pub struct Rfq { pub sell_mint: String, pub buy_mint: String, pub sell_amount: u64, pub trader: String }
pub struct Quote { pub buy_amount: u64, pub expiry: u64 }
pub fn request_quotes(dealers: &[SocketAddr], rfq: &Rfq, timeout: Duration) -> Vec<Option<Quote>>; // same order as dealers
pub fn serve(listener: TcpListener, stop: &AtomicBool, handler: impl FnMut(Rfq) -> Result<Quote, String>) -> io::Result<()>;
pub fn best_quote(quotes: &[Option<Quote>], fair: u64, band_bps: u16, now: u64, min_ttl_secs: u64, max_ttl_secs: u64) -> Option<usize>;
```

`serve` handles one connection at a time with read and write timeouts, caps
request and reply lines at 4 KiB (the client caps replies too), and refuses a
listener that is not bound to a loopback address, because the protocol is
plain text.

## Files

Base commit (before the parallel work):

- `Cargo.toml` — workspace, members `core`, `cli`, `exclude = [".tools",
  ".worktrees"]`, `[profile.release] overflow-checks = true`.
- `rust-toolchain.toml` — 1.98.1 with clippy and rustfmt.
- `.gitignore` — `target/`, `.tools/`, `.worktrees/`.
- Stub `core` and `cli` crates with no dependencies, so cargo works in every
  worktree.

Piece A, toolchain (agent 1):

- `scripts/setup.sh` — idempotent. Clones the examples at the pinned commit
  into `.tools/privacy-ring-examples`, runs its `swap/xark/scripts/`
  `setup-zk-tools.sh`, `zk compile --test`, `check-keys.sh`, `cargo build-sbf`
  and `build-localnet.sh`, and writes `.tools/env` with the exports. Every
  download is the upstream script's own, checked against its pinned SHA-256.
- `cli/Cargo.toml` and `Cargo.lock` (seeded from the example's lock) — path
  deps into `.tools/`, `spr-*` 0.10.0 with `spr-client` features `reqwest`
  and `solana-rpc`, `reqwest` 0.12 declared directly. Proven by `cargo check`.
- Proof it works on this machine: the example's own `swap-test` passes.
  Measure how far the local network's Clock sysvar drifts from wall time.

Piece B, quote logic (agent 2, parallel with A, commits no lock file):

- `core/Cargo.toml` — `serde`, `serde_json`, `anyhow` only.
- `core/src/{lib,rfq,price}.rs` with unit tests.

Piece C, chain and commands (agent 1 again, in the same worktree, after B is
merged into it):

- `cli/src/chain.rs` — chain time, wallet scan and note selection, register,
  exact-note self-transfer, lock, nullifier check, refund, discover, settle,
  mint decimals, `fetch_asset_id`. Thin calls into `swap-sdk` and
  `spr-client`. Asserts tree id 0.
- `cli/src/config.rs` — one JSON config per role: RPC and indexer URLs, key
  (`{"file": path}` for local test keys or `{"keychain": name}`, read by
  running `security find-generic-password -s <name> -w` with an argument
  list; the secret is a Solana keypair JSON array), assets (mint, price),
  dealers (address, public key), `band_bps`, `spread_bps`, TTL bounds, settle
  margin. The config and the key are never printed.
- `cli/src/{trader,dealer,main}.rs` — `keeler swap <config> <sell_mint>
  <buy_mint> <amount>`, `keeler dealer <config>`, `keeler refund <config>`.
  Dealers `ensure_registered` at start.
- `cli/tests/e2e.rs`, `README.md`.

## Tests

- `core` unit tests: price math (decimals both ways, both signs of `e`,
  overflow, zero and negative price, both band edges, fair = 3 with a 100 bps
  band rejects 2, bps over 10,000), spread, Hermes parsing from a recorded
  reply (good, stale, wide confidence, missing feed), best quote (max, tie,
  empty, TTL bounds, out of band both sides), protocol over a real
  `TcpListener` (good reply, error reply, slow dealer times out, oversized
  line, garbage, non-loopback listener refused).
- `cli` e2e on the local network, started the way the example's
  `test/tests/shared.rs` does. The fixture funds four actors: one trader and
  three dealers. Dealers are funded with one note larger than any quote, so
  the exact-note split runs.
  1. Dealers at 10 bps, 30 bps, and one quoting 20% under fair. The trader
     sells; the 10 bps dealer settles; trader and dealer balances move by
     exactly the quoted amounts; the others do not move. Then the same trader
     swaps again on the same network and it settles again (history and
     seen-set).
  2. A dealer that quotes and never settles: after expiry `refund` returns
     the full sell amount, and a second `refund` is a no-op.
  3. An order designated to a dealer with terms it never quoted: the dealer
     does not settle it.
- `cargo fmt -p keeler-core -p keeler -- --check` (never `--all`: it would
  format the path dependencies), `cargo clippy --workspace --all-targets --
  -D warnings`.

## Risks

- The pinned toolchain may not build on this machine (Apple M4, surfpool
  1.0.0 installed). Piece A finds out first.
- If the local network's clock is far from wall time, quote TTLs measured in
  chain seconds behave oddly for a human; piece A measures it and e2e test 2
  depends on chain time passing the expiry.
- Path dependencies resolve against our lock file; it is seeded from the
  example's.
- A dealer inside the band can win and never settle; the trader's funds stay
  locked until expiry. Short TTLs limit it; nothing prevents it in v1.
- Quotes reserve nothing: a dealer can quote more than it can settle across
  concurrent requests.
- The Hermes call is only tested through a recorded reply, not live.
- Each worktree needs its own `.tools/` and `target/`; several GB each.
