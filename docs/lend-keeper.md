# Keeler Lend: running the keeper

The keeper is the program's `KEEPER` key. It holds the openings of the pool's notes (the reserve,
every receipt and position), signs tickets, fills ticketed intents, builds the owner-signed
`redeem` and `draw`, and liquidates. The page and Worker side is in `docs/lend-api.md`; the design
is in `docs/plan-lend.md`. The code is `cli/src/keeper.rs` (logic, HTTP, records),
`cli/src/lend_chain.rs` (the chain) and `cli/src/lend_auth.rs` (request and ticket signatures).

## Configure and run

The keeper runs inside the dealer, on the dealer's HTTP server, when the dealer config has a
`lend` section (it needs `http`):

```json
"lend": {
  "program_id": "A7qaBKJ1gJRcKwMWyyRdr9YhmbBmnMaemM9Sijah3EbC",
  "network": "devnet",
  "key": { "keychain": "keeler-lend-keeper" },
  "state_file": "/Users/me/.local/state/keeler/keeper.json",
  "pause_file": "/Users/me/.local/state/keeler/lend.pause",
  "price": "test",
  "limits": {
    "min_supply": 1000000,
    "min_collateral": 10000000,
    "min_deposit_add": 1,
    "min_repay": 10000,
    "tickets_per_wallet_hour": 30,
    "max_open_intents": 64,
    "reservation_secs": 20,
    "ticket_secs": 600,
    "bucket_secs": 600,
    "bound_margin_bps": 50
  },
  "poll_secs": 2
}
```

On devnet `scripts/run-dealer.sh <config>` runs it: for a config with `lend` it builds the binary
with the lend deploy's build parameters (from `target/devnet-<name>/program-id`, the swap deploy
summary and the `keeler-devnet-deploy` Keychain item) in `target/devnet-<name>/dealer-cargo`; the
keeper is the dealer key, and the Worker still gets only `DEALERS`.

Or alone, with no swap program: `keeler keeper <config>`, where the config is

```json
{
  "network": { "rpc_url": "...", "indexer_url": "...", "prover": "local" },
  "key": { "keychain": "keeler-lend-keeper" },
  "http": { "listen": "127.0.0.1:8789", "key": { "keychain": "keeler-lend-http" } },
  "lend": { ... as above ... }
}
```

| Field | Meaning |
| --- | --- |
| `program_id` | Must be the program this binary was built for (`KEELER_LEND_PROGRAM_ID` at build time) |
| `network` | The name in the session text the wallet signs (`Network: <network>`); must match the page's |
| `key` | The keeper key; default the config's `key`. It must be the program's `KEEPER` |
| `admin_key` | Only for `keeler lend-init-reserve` |
| `state_file` | Default `~/.local/state/keeler/keeper-<keeper pubkey>.json` |
| `pause_file` | While this file exists: no new supply tickets or fills, no deposit tickets or fills that open a position, no borrowing |
| `price` | `test`: the vault's test price account (local networks, devnet). `pyth`: the vault's Pyth `PriceUpdateV2` account compiled into the program |
| `limits.min_*` | Ticket minimums; the program's own minimums are the floor |
| `limits.tickets_per_wallet_hour` | Tickets one wallet may take in an hour |
| `limits.max_open_intents` | Open (unfilled, unexpired) tickets across all wallets |
| `limits.reservation_secs` | How long an owner build holds the pool for its wallet (5 to 120) |
| `limits.ticket_secs` | Ticket and intent lifetime (120 to 900; the page refuses outside 60 to 900) |
| `limits.bucket_secs` | `/lend/pools` refresh period (at least 60) |
| `limits.bound_margin_bps` | The draw bound sits this far below `price − confidence`, the liquidation bound this far above `price + confidence` |

Lend requests run on their own threads (up to 8 at once) with a 30 s deadline and a 16 KiB body
cap; the swap routes keep their 4 KiB cap and one-at-a-time server. Every route is loopback only and
needs the `x-keeler-key` header, as the swap routes do. Wallets must be in the user registry: an
unregistered wallet gets a 400 saying so.

`/lend/pools` gives coarse figures: the reserve accrued to the start of the current bucket
(`bucket_secs`, default 10 minutes), with size, utilisation and both rates rounded to two
significant figures, recomputed at most once per bucket.

## CLI commands

| Command | What |
| --- | --- |
| `keeler keeper <keeper config>` | The keeper and its HTTP routes without a dealer |
| `keeler lend-init-reserve <keeper config>` | Creates the pool's reserve once; the keeper pays, `lend.admin_key` co-signs |
| `keeler lend-set-price <keeper config> <vault> <price> <confidence>` | Writes the vault's test price (debt base units per collateral base unit x 1e12, so $10 per tSOL in tUSD is `10000000000`). Refused when `network` is `mainnet`; the program refuses it in a build without `test-price` |

## The queue

One lane for the pool: one keeper transaction at a time, and none while an earlier keeper
transaction may still land. Each pass settles the records first, then runs the most urgent job:

1. Liquidations: every position with debt is checked at `price + confidence` (plus the margin)
   on every pass; one past the threshold is sized with `core::lend::liquidate` (the SDK builder
   does the same) and repaid from the keeper's own notes of the debt token, merged into one note by
   a self-transfer when no single note covers it. Above the max limit the whole debt is repaid. A
   position the last liquidation left a hair past the threshold (repay under `min_repay`) is left.
   Liquidations run even while a wallet holds the reservation.
2. Repay fills, then deposit fills (into an existing position, then new positions).
3. Supply fills. Owner builds (`/lend/build`) take the lane between passes.

Only intents that match an issued, open ticket exactly (wallet, purpose, target, amount, expiry and
mint) are filled; intents are found by scanning the wallet's user PDA, which the ticket names, and
decrypted with the keeper's viewing key. Anything else (dust, unticketed, mismatched, or within 15 s
of its expiry) is ignored and left for the maker to cancel. A ticket that fails three builds is
given up. `now` is always chain time.

An owner build reserves the pool for its wallet for `reservation_secs`: other wallets get 409, the
keeper's fills wait, liquidations do not. The reservation ends when the build lands or the time
runs out.

## The state file

JSON, mode 0600, written atomically (a temp file then a rename) on every change:

- `reserve`, `receipts`, `positions`: note openings (amount, blinding, data, data hash, hash,
  nullifier, leaf).
- `tickets`: every ticket issued in the last week with its status (`open`, `filled`, `failed`).
  Tickets the keeper did not sign are dropped on load.
- `pending`: every built transaction not yet settled: the op, the wallet, the reserve nullifier it
  spends, all its input nullifiers and output hashes, and the new reserve, receipt and position
  openings. A build is saved here before it is sent or handed to a wallet; if the save fails, it is
  neither.
- `reservation`: the wallet holding the pool, and until when.

## Recovery

Several pending builds may spend one reserve (an owner build that was never sent, then another);
at most one lands. Each pass reads, for each pending build, whether its inputs are spent (from the
nullifier accounts, not the indexer). Spent: the indexer's spending transaction is read, and the
build whose note hashes it appended is the one that landed; its notes replace its inputs, and
every other build on those inputs is dropped. Unspent after `now + 125 s` (the program accepts a
`now` up to 120 s behind the clock): it can no longer land and is dropped.

On start the keeper settles the pending builds, then rebuilds from the chain: every transaction the
keeper key signed (`getSignaturesForAddress`), its lend instruction (which wallet), and each output
opened with the keeper's writer key, checked against the hash on chain (it waits up to 60 s for the indexer to have each one, and
refuses to start rather than rebuild without one). It keeps the unspent
reserve, receipts and positions, merged with the file. It refuses to start with two unspent
reserves. Tickets are not on chain: after a restart with an empty file the keeper fills no intent
made before it (their makers refund after expiry) and the account view shows no old intents.

## Never

- Run two keepers on one pool (two processes, or one with two state files). Both would build on
  the same reserve; at best one fails every operation, at worst a filled ticket is filled again.
- Delete or edit the state file while the keeper runs, or restore an old copy. It is the only
  record of tickets and of builds in flight. (An empty file is recoverable from the chain; a wrong
  one is not.)
- Put the keeper or admin key in a file or the config. Use the Keychain (`{"keychain": "..."}`).
- Run with `price: "test"` on mainnet, or expose the HTTP port beyond loopback; the Worker is the
  only client.
- Let the keeper's debt-token balance run out: liquidations need it. Keep at least the largest
  position's debt in its private balance.
- Change `program_id`, the vault mints or the keeper key without rebuilding the binary and the
  program together.
