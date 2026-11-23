# Keeler

A private quote router on a local network. A trader wants to sell an amount
of one asset for another without showing size or price on-chain. Keeler asks
several dealers for a quote, drops quotes far from a reference price, and
settles the best one through the Helius Privacy Rings swap example program
(Xark). The design is in [`docs/plan.md`](docs/plan.md).

## What is private and what is public

- Private: the amounts and the price. They live inside shielded notes and the
  order terms, encrypted to the trader and the chosen dealer.
- Public at settlement: both wallets (the trader's and the dealer's Solana
  keys) and both asset ids, and that a swap was made and later taken or
  cancelled.
- Every dealer that is asked learns the pair, the size and the trader's
  public key, because the request for quote carries them in plain text.

## Setup

> **Local network only.** Keeler settles through the Helius Privacy Rings
> swap example (Xark). That program uses insecure test proving keys from a
> fixed public seed, so anyone can forge its proofs. Never deploy it or use it
> with real funds.

Needs macOS on Apple silicon or Linux x86-64, git, curl, Python 3.11+, rustup
and the Solana/Agave CLI (`cargo build-sbf`).

```bash
scripts/setup.sh      # idempotent; the first run takes a while
source .tools/env
cargo check -p keeler
```

`scripts/setup.sh`:

- clones `helius-labs/privacy-ring-examples` at the pinned commit into
  `.tools/privacy-ring-examples` (git-ignored; that repository has no licence,
  so none of its code is copied into this one);
- runs the example's own `swap/xark` steps: installs the `zolana` CLI
  (`cli-v0.10.0`) into `swap/xark/.tools/`, compiles the circuits and test
  keys into `swap/xark/target/zk/`, checks them against the pinned checksums,
  builds `swap_program.so` with SBF platform tools v1.54, and fetches the
  localnet artifacts (shielded-pool program, prover, Photon, surfpool) into
  `~/.config/zolana/cache` and the user-registry program into
  `swap/xark/.tools/`. The upstream scripts check each of these release
  downloads against a pinned SHA-256. The SBF platform tools are whatever
  Agave's `cargo build-sbf` fetches; Keeler adds no check of its own;
- applies `patches/swap.patch` (a `wasm` feature and its `Cargo.lock` lines),
  sets the program's `declare_id!` to `KEELER_SWAP_PROGRAM_ID` (default: the
  example's ID), and builds the single-threaded web package into
  `web/public/swap` with the make and cancel `pk.bin`, `vk.bin` and
  `circuit.xbc` beside it;
- writes `.tools/env` with `ZOLANA_CLI_BIN`, `SHIELDED_POOL_PROGRAM_PATH`,
  `PROVER_BIN`, `ZOLANA_PHOTON_BIN`, `SURFPOOL_BIN`,
  `USER_REGISTRY_PROGRAM_PATH`, `SWAP_KEYS_DIR` and `KEELER_SWAP_PROGRAM_SO`.

Outside the repository it also uses rustup (Rust 1.98.1), the Solana platform
tools cache (`~/.cache/solana`) and `~/.config/zolana/`. Expect several GB in
`.tools/` and `target/`.

The `cli` crate depends on `swap-sdk` and `swap-program` by path from
`.tools/`, so run `scripts/setup.sh` before any cargo command, and keep
`.tools/` in place: the binary reads the proving keys from it at run time.
Format with `cargo fmt -p keeler-core -p keeler`, never `cargo fmt --all`.

## Commands

All times are chain time, read from the Clock sysvar. On the local network
chain time runs at about 0.87 of wall time, so a 30 s quote lasts about 35 s
on the wall clock.

- `keeler swap <config> <sell_mint> <buy_mint> <amount>`: checks that one
  note covers `amount`, asks every dealer (2 s timeout each), keeps quotes
  within `band_bps` of the reference price whose time to live is within
  `[min_ttl_secs, max_ttl_secs]`, locks the largest, and waits on the order's
  nullifier. It prints `settled: received <n>`, or after expiry cancels and
  prints `not settled before expiry: refunded`.
- `keeler dealer <config>`: registers, serves quotes on `listen`
  (loopback only) at the reference price less `spread_bps`, and settles only
  orders that exactly match a quote it gave, while `now + settle_margin_secs`
  is before the expiry and, at the last look, the quoted amount is at most
  the fair amount at the current price plus `settle_tolerance_bps`. It signs
  every quote, keeps its quotes in a state file across restarts, and splits
  off a note of exactly the quoted amount first when it has none. The trader
  drops a quote not signed by the dealer's configured `pubkey`.
- `keeler refund <config>` (trader config): cancels every unspent order of
  this trader whose expiry has passed. Running it again is a no-op.
  It only sees orders in the assets the config lists: an order in any other
  mint is skipped by the SDK and needs a config that lists that mint.

Amounts are in base units. Mints are base58; SOL is
`11111111111111111111111111111111`. Run with `source .tools/env` so the
binary finds the proving keys (`SWAP_KEYS_DIR`).

## Web app support

- `scripts/localnet.sh start|stop <offset>` (offset 100 to 4900, a multiple of
  100): a local stack for the web app. Network on RPC `8899+offset`, Photon
  `8784+offset`, prover `3001+offset/100`; tUSD (6 decimals) and tSOL (9);
  a dealer with one private note of each, TCP quotes on `47000+offset`, HTTP
  on `47500+offset` with `x-keeler-key: keeler-localnet-dealer-key`, faucet
  on. Prints and writes `target/localnet/keeler-web.json` (and
  `keeler-web-<offset>.json`). Fixture test keys only.
- `scripts/deploy-devnet.sh` (refuses any cluster but devnet),
  `scripts/run-dealer.sh` (restarts the dealer and the tunnel with backoff,
  rewrites the Worker's `DEALERS` when the tunnel URL changes) and
  `scripts/set-upgrade-authority.sh <program> <new-authority>`: devnet
  deployment, the dealer behind a quick tunnel, and the authority handoff.
  Not run until the owner approves publishing the example's code.
- Dealer HTTP (loopback only, every request carries `x-keeler-key`, 4 KiB
  body cap), amounts as decimal strings:
  `POST /quote` `{"sell_mint","buy_mint","sell_amount","trader"}` ->
  `{"quote":{"buy_amount","expiry","signature"}}` or `{"error":"no quote"}`;
  `POST /faucet` `{"wallet"}` -> `{"ok":true}` or `{"error"}` (404 off,
  429 over a limit, 502 transfer failed); `GET /health` ->
  `{"ok":true,"now":<chain time>}`. 401 bad key, 403 not loopback, 413 body
  too large, 400 bad request.
- Quote signature: Ed25519 by the dealer's key over 167 bytes,
  `"keeler-quote-v1"`, then program ID, sell mint, buy mint, trader (32
  bytes each), then sell amount, buy amount, expiry (u64 little-endian);
  128 hex characters on the wire.
- `web/tests/fixtures/{make,cancel}-ix.json`: golden instruction bytes for
  the web client, and `quote-sig.json`, a signed quote test vector;
  `KEELER_WRITE_FIXTURES=1 cargo test -p keeler --test fixtures`
  regenerates them (needed after a program ID change).

## Configuration

`network.prover` is `"local"` (the local prover at `ZOLANA_PROVER_URL`, proof
data from our indexer) or `{"helius": {"keychain": "<name>"}}`: Helius's
attested prover at `indexer_url`, with the API key read from that Keychain
item (a bare key, or a Helius URL with an `api-key` parameter) and added to
the RPC and indexer URLs at run time. Logs and errors redact `api-key`
values.

The dealer also takes, all optional: `"http": {"listen", "key": <key
source>}` for the HTTP API; `"poll_secs"` (default 1 with the local prover,
else 5); and `"faucet": {"key", "tokens": [{"mint", "amount"}], "lamports",
"per_wallet_daily", "daily_cap"}`, where `key` is the tokens' mint authority
and pays the fee SOL, and the limits count claims per UTC day. Also:
`"state_file"` (default `~/.local/state/keeler/dealer-<pubkey>.json`, mode
0600; unexpired quotes reload on start); `"pause_file"` (while it exists, no
new quotes; bound orders still settle); `"settle_tolerance_bps"` (default 0;
a failed price fetch settles nothing that poll); and `"limits": [{"mint",
"max_sell_amount", "hourly_sell_cap"}]` on what traders sell the dealer per
order and per rolling hour of bound orders (the hourly count restarts with
the dealer).

`key` is `{"file": "<keypair.json>"}` for local test keys or
`{"keychain": "<name>"}`, read with `security find-generic-password -s <name>
-w`; the secret is a Solana keypair JSON array. A price is fixed
(`{"price": 15, "expo": -2}` is 0.15 USD) or `{"pyth": "<feed id>"}`, which
needs a `pyth` section; the Hermes API key is sent as a bearer token. Every
mint you trade needs a price.

Trader:

```json
{
  "network": {"rpc_url": "http://127.0.0.1:8899", "indexer_url": "http://127.0.0.1:8784", "prover": "local"},
  "key": {"keychain": "keeler-trader"},
  "assets": [
    {"mint": "<spl mint>", "price": {"price": 2, "expo": 0}},
    {"mint": "11111111111111111111111111111111", "price": {"pyth": "<SOL/USD feed id>"}}
  ],
  "pyth": {
    "url": "https://hermes.example",
    "api_key": {"keychain": "keeler-hermes"},
    "max_age_secs": 60,
    "max_conf_bps": 100
  },
  "dealers": [
    {"address": "127.0.0.1:47001", "pubkey": "<dealer Solana public key>"}
  ],
  "band_bps": 50,
  "min_ttl_secs": 10,
  "max_ttl_secs": 120
}
```

Dealer:

```json
{
  "network": {"rpc_url": "http://127.0.0.1:8899", "indexer_url": "http://127.0.0.1:8784", "prover": "local"},
  "key": {"file": "dealer.json"},
  "assets": [
    {"mint": "<spl mint>", "price": {"price": 2, "expo": 0}},
    {"mint": "11111111111111111111111111111111", "price": {"price": 100, "expo": 0}}
  ],
  "listen": "127.0.0.1:47001",
  "spread_bps": 10,
  "quote_ttl_secs": 30,
  "settle_margin_secs": 5
}
```

## Tests

```bash
cargo test -p keeler-core
cargo test -p keeler --lib --test fixtures
source .tools/env && cargo test --release -p keeler --test e2e -- --nocapture
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt -p keeler-core -p keeler -- --check
```

The end-to-end tests start their own local network per test, the way the
example's tests do, and fund one trader and three dealers.

## Known limits

- Local network only: the swap program's test keys let anyone forge proofs.
- The upstream prover listens on `0.0.0.0:3001`, every interface, not just
  loopback. Keeler does not change it.
- The quote protocol is plain text on loopback, with no TLS.
- A dealer inside the band can win and never settle: the trader's funds stay
  locked until expiry, then `refund` returns them. Short TTLs limit this.
- Quotes reserve nothing: a dealer can quote more than it can settle across
  concurrent requests.
- The Pyth Hermes call is only tested through recorded replies, not live.
- Dealer discovery rescans the whole order history on every poll.
- Each worktree needs its own `.tools/` and `target/`, several GB each.
