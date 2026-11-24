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
  "price_refresh_secs": 45,
  "limits": {
    "min_supply": 1000000,
    "min_collateral": 10000000,
    "min_deposit_add": 1,
    "min_repay": 10000,
    "min_redeem": 10000,
    "min_borrow": 10000,
    "min_withdraw": 10000000,
    "tickets_per_wallet_hour": 30,
    "open_per_wallet": 4,
    "max_open_intents": 64,
    "builds_per_wallet_hour": 60,
    "reservation_secs": 20,
    "ticket_secs": 600,
    "bucket_secs": 600,
    "bound_margin_bps": 50
  },
  "poll_secs": 2
}
```

On devnet `scripts/run-dealer.sh <config>` runs it: for a config with `lend` it finds the
deployment whose `target/devnet-<name>/program-id` is `lend.program_id`, builds the binary in
`target/devnet-<name>/dealer-cargo` with the build parameters `scripts/deploy-lend-devnet.sh`
recorded in `build-params.json` (it refuses to run if their keeper is not the dealer), and points
the keeper at the deployment's own copy of the proving keys (`KEELER_LEND_KEYS_DIR=
target/devnet-<name>/keys`), so a rebuild of `lend/target` cannot change them under a running
keeper. The keeper is the dealer key, and the Worker still gets only `DEALERS`.

Inside the dealer the keeper starts on its own thread: the swap routes serve at once, and the lend
routes answer 503 until the keeper has connected and settled its records. A start that fails
(network, indexer) is logged and retried, from 5 s doubling to 5 minutes; swaps go on meanwhile.

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
| `network` | The name in the session text the wallet signs (`Network: <network>`); must match the page's. Lowercase letters, digits and `-`; any name containing `mainnet` is treated as mainnet |
| `poll_secs` | Seconds between passes (1 to 60) |
| `key` | The keeper key; default the config's `key`. It must be the program's `KEEPER` |
| `admin_key` | Only for `keeler lend-init-reserve` |
| `state_file` | Default `~/.local/state/keeler/keeper-<keeper pubkey>.json` |
| `pause_file` | While this file exists: no new supply tickets or fills, no deposit tickets or fills that open a position, no borrowing |
| `price` | `test`: the vault's test price account (local networks, devnet). `pyth`: the vault's Pyth `PriceUpdateV2` account compiled into the program |
| `price_refresh_secs` | `price: "test"` only: how often the keeper re-posts the test price (5 to 80, default 45; the program refuses a price older than 90 s, the keeper's own draws one older than 60 s) |
| `limits.min_supply`, `min_collateral`, `min_deposit_add`, `min_repay` | Ticket minimums; the program's own minimums are the floor |
| `limits.min_redeem` | Smallest redeem build, unless it empties the receipt |
| `limits.min_borrow`, `min_withdraw` | Smallest borrow and collateral withdrawal in a draw build (a withdrawal that empties the position is always allowed) |
| `limits.tickets_per_wallet_hour` | Tickets one wallet may take in an hour |
| `limits.open_per_wallet` | Open tickets one wallet may hold at once |
| `limits.max_open_intents` | Open tickets across all wallets whose intent the keeper has seen on chain; repays and deposits into an existing position are not counted or refused, so a position can always be saved |
| `limits.builds_per_wallet_hour` | Owner builds (`/lend/build`) one wallet may ask for in an hour |
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
| `keeler lend-set-price <keeper config> <vault> <price> <confidence>` | Writes the vault's test price (debt base units per collateral base unit x 1e12, so $10 per tSOL in tUSD is `10000000000`) and saves it as the value a running keeper keeps posting. Refused when `network` is `mainnet` or `price` is not `test`; the program refuses it in a build without `test-price` |

## The test price

With `price: "test"` (never on mainnet: the keeper refuses `test` there, and refuses to post a price
with `pyth`) the keeper keeps the vault's test price fresh: before each pass, when the account is
older than `price_refresh_secs` or does not hold the wanted value, it posts it again with chain time
as the publish time. The wanted value is the control file `<state_file without extension>.price.json`
(`{"vault", "price", "confidence"}`, mode 0600) that `keeler lend-set-price` writes, else the price
already on chain. So `lend-set-price` moves the price a running keeper keeps (how a liquidation is
triggered by hand), and the keeper never invents a price: with no control file and no account it
posts nothing. Each post costs a transaction fee.

## The queue

One lane for the pool: one keeper transaction at a time, and none while an earlier keeper
transaction may still land. Each pass settles the records first, then runs the most urgent job:

1. Liquidations: every position with debt is checked at `price + confidence` (plus the margin)
   on every pass, with a price no older than 75 s (the program allows 90 s); one past the
   threshold is sized with `core::lend::liquidate` (the SDK builder does the same) and repaid from
   a keeper note of exactly the repay plus the buffer, split or merged from the keeper's own debt
   token notes by a self-transfer first (an empty change slot: see `lend/AGENTS.md`). Above the
   max limit the whole debt is repaid. A position the last liquidation left a hair past the
   threshold (repay under `min_repay`, or under one debt share) is left. A liquidation that fails
   is logged and that position waits 60 s; the fills go on meanwhile. Liquidations run even while
   a wallet holds the reservation.
2. Repay fills, then deposit fills (into an existing position, then new positions).
3. Supply fills, oldest ticket first within each kind. Owner builds (`/lend/build`) take the lane
   between passes.

Only intents that match an issued, open ticket exactly (wallet, purpose, target, amount, expiry and
mint) are filled; intents are found by scanning the wallet's user PDA, which the ticket names,
from where the last scan stopped (at most 4 pages a pass), and decrypted with the keeper's viewing
key. Anything else (dust, unticketed, mismatched, or within 15 s of its expiry) is ignored and left
for the maker to cancel. A fill that fails is retried after 10 s, doubling each time; a supply or a
new position's deposit is given up after 5 failures, a repay or a deposit into an existing
position never is (until its ticket expires). A send the network refuses outright (preflight, a
transaction error, an expired blockhash) is dropped from the records at once and retried the same
way. `now` is always chain time.

Each process holds the in-flight notes of every build it makes (the keeper's and the dealer's
share one key) for 3 minutes, so a swap settlement and a liquidation never pick the same note.

An owner build reserves the pool for its wallet for `reservation_secs`: other wallets get 409, the
keeper's fills wait, liquidations do not. A later build by the same wallet does not extend it. It
ends when the build lands or the time runs out; a wallet that lets it run out unused gets 409 for
another `reservation_secs`. An owner build checks, in order: the wallet owns the receipt or
position (404), `builds_per_wallet_hour` (429), the lane and reservation (409), the minimums
(422), the price age (412, draws), the vault minimums (422) and health (423), and last the pool's
liquidity (410) at the amount rounded up to two significant figures, so a refusal tells little
about the pool's exact size. A build that takes longer than 25 s is answered 503 and not kept.

## The state file

JSON, mode 0600, written atomically (a temp file then a rename) on every change:

- `reserve`, `receipts`, `positions`: note openings (amount, blinding, data, data hash, hash,
  nullifier, leaf).
- `tickets`: every ticket issued in the last week with its status (`open`, `filled`, `failed`),
  its failed attempts and when it may be tried again (`retry_at`), and whether its intent has been
  seen on chain (`seen`). Tickets the keeper did not sign are dropped on load.
- `pending`: every built transaction not yet settled: the op, the wallet, the reserve nullifier it
  spends, all its input nullifiers and output hashes, and the new reserve, receipt and position
  openings. A build is saved here before it is sent or handed to a wallet; if the save fails, it is
  neither.
- `reservation`: the wallet holding the pool, and until when.
- `scanned`: the newest finalized keeper transaction already read back from the chain.

The keeper takes an exclusive lock on `<state_file without extension>.lock` for as long as it runs,
and refuses to start when another process holds it.

## Recovery

Several pending builds may spend one reserve (an owner build that was never sent, then another);
at most one lands. Each pass reads, for each pending build, whether its inputs are spent (from the
nullifier accounts, not the indexer). Spent: the indexer's spending transaction is read, and the
build whose note hashes it appended is the one that landed; its notes replace its inputs, and
every other build on those inputs is dropped. A build whose reserve is unspent but another of
whose inputs was spent elsewhere can never land and is dropped, as is one unspent after
`now + 125 s` (the program accepts a `now` up to 120 s behind the clock). A dropped fill is retried
as above.

On start, and whenever the keeper holds no reserve, it settles the pending builds, then reads back
from the chain every transaction the keeper key signed and paid for since `scanned`
(`getSignaturesForAddress`), its lend instruction (which wallet), and each output opened with the
keeper's writer key, checked against the hash on chain (it waits up to 60 s for the indexer to have
each one, and refuses to start rather than rebuild without one). A transaction another key paid
for is never read, so notes made with the public local test keys are never taken up. It keeps the
unspent reserve, receipts and positions, merged with the file, and moves `scanned` to the newest
finalized transaction it read. It refuses to start with two unspent reserves. Tickets are not on chain: after a restart with an empty file the keeper fills no intent
made before it (their makers refund after expiry) and the account view shows no old intents.

## Never

- Run two keepers on one pool. The state file's lock stops two processes on one file, not two
  state files (or two machines). Both would build on the same reserve; at best one fails every
  operation, at worst a filled ticket is filled again.
- Delete or edit the state file while the keeper runs, or restore an old copy. It is the only
  record of tickets and of builds in flight. (An empty file is recoverable from the chain; a wrong
  one is not.)
- Put the keeper or admin key in a file or the config. Use the Keychain (`{"keychain": "..."}`).
- Run with `price: "test"` on mainnet (the keeper refuses to), or expose the HTTP port beyond loopback; the Worker is the
  only client.
- Let the keeper's debt-token balance run out: liquidations need it. Keep at least the largest
  position's debt in its private balance.
- Change `program_id`, the vault mints or the keeper key without rebuilding the binary and the
  program together.
