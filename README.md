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
  `swap/xark/.tools/`. Every download is the upstream scripts' own and is
  checked against a pinned SHA-256;
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
  is before the expiry. It splits off a note of exactly the quoted amount
  first when it has none.
- `keeler refund <config>` (trader config): cancels every unspent order of
  this trader whose expiry has passed. Running it again is a no-op.

Amounts are in base units. Mints are base58; SOL is
`11111111111111111111111111111111`. Run with `source .tools/env` so the
binary finds the proving keys (`SWAP_KEYS_DIR`).

## Configuration

`key` is `{"file": "<keypair.json>"}` for local test keys or
`{"keychain": "<name>"}`, read with `security find-generic-password -s <name>
-w`; the secret is a Solana keypair JSON array. A price is fixed
(`{"price": 15, "expo": -2}` is 0.15 USD) or `{"pyth": "<feed id>"}`, which
needs a `pyth` section; the Hermes API key is sent as a bearer token. Every
mint you trade needs a price.

Trader:

```json
{
  "rpc_url": "http://127.0.0.1:8899",
  "indexer_url": "http://127.0.0.1:8784",
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
  "rpc_url": "http://127.0.0.1:8899",
  "indexer_url": "http://127.0.0.1:8784",
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
