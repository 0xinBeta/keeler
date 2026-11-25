# Keeler Lend: the keeper contract

What the web page (`web/src/keeler/lend*.ts`) sends to the keeper and what it accepts back. The
Worker (`worker/src/index.ts`, `worker/README.md`) sits between them: it checks the shape of every
request and reply, rate-limits, and forwards `/api/lend/<route>` to the keeper's `/lend/<route>`.
Before it charges a global limit or forwards a signed request, it checks `sessionAuth` (the
authorisation text rebuilt from its own `NETWORK` and `LEND_PROGRAM_ID`) and the request
`signature` exactly as the keeper does (steps 2 and 3 below), and answers 401 when either fails.
Expiry, freshness and replay need chain time and the nonce store, so only the keeper checks them;
the keeper also checks everything the Worker did. Every `LEND_POOLS` mint and `LEND_VAULTS`
collateral and debt mint must be in `ASSETS` (the page reads lend amounts with its decimals), or
every lend route and `/api/config` answer 500.

`web/tests/fixtures/lend-requests.json` holds signed requests and a signed ticket made from fixed
keys, with every intermediate byte string in hex, so a Rust keeper can test its verifier against
the page's signer. Regenerate it with `UPDATE_FIXTURES=1 pnpm test tests/lend-auth.test.ts`.

## Build settings

| Variable | What |
| --- | --- |
| `VITE_LEND_PROGRAM_ID` | The lend program. Lending is shown only when `/api/config` has `lend` and its `programId` equals this; otherwise Earn and Borrow stay hidden |
| `VITE_LEND_KEEPERS` | The program's keeper set (`KEEPERS`). Each keeper signs its tickets and pays for the transactions it builds; the page checks both against the keeper the reply names, which must be in this set. Lending is hidden without it, or when the service (`/api/config.lend.keepers`, the keepers the Worker runs) lists a keeper that is not in it |
| `VITE_LEND_POOL_VIEWING` | The pool viewing key (`POOL_VIEWING`), 66 hex: the key the page makes intents to |

Records in `localStorage` are keyed `keeler:lend:v1:<network>:<program>:<wallet>:<kind>:<id>`.
They hold note openings (amount, blinding, state) and transaction signatures, never key material.

## Routes

| Route | Method | Signed | Request | Reply |
| --- | --- | --- | --- | --- |
| `/lend/pools` | GET | no | none | `{pools: [{mint, at, size, utilisationBps, supplyRateBps, borrowRateBps, surplus, badDebt}]}`, at most 8 |
| `/lend/account` | POST | yes | the signed fields, and `tickets`: the open tickets the page holds (at most 8, not signed) | `{receipts: [{pool, shares, message}], positions: [{vault, position, collateral, debtShares, message}], intents: [{id, purpose, amount, expiry, status}], index?: {supply, borrow, at}}` |
| `/lend/ticket` | POST | yes | signed fields, `purpose`, `amount`, and `pool` (supply) or `vault` + `position` (deposit, repay) | `{id, expiry, signature, keeper}` |
| `/lend/build` | POST | yes | signed fields, `op`; redeem: `pool`, exactly one of `amount`, `shares`, and optionally `receipt`; draw: `vault`, `position`, `borrow`, `withdraw` | `{transaction, outputs, now, bound?, index?, keeper}` |

Amounts are decimal strings of base units (u64). `vault` is a number, `position` a decimal string
of a u63.

- **pools**: `at` is the chain time the figures are from; `size` is the pool's total supply in base
  units. These are coarse public figures; the page shows them as they are. `surplus` (idle plus
  debt less what suppliers are owed, 0 when short) and `badDebt` (the debt of positions whose
  collateral at `price − confidence` is worth less than it) are decimal strings of debt-token base
  units, two significant figures, `"0"` until the keeper's first pass with a price. While `badDebt`
  exceeds `surplus` the keeper refuses new borrowing (403).
- **account `tickets`**: each as the page got it, the reply's `id`, `expiry`, `signature` and
  `keeper` with the request's `purpose`, `amount` and `pool` (supply) or `vault` (a number) and
  `position` (deposit, repay). A keeper takes on a ticket another keeper of the set signed for this
  wallet when the signature checks against that keeper and the expiry is more than 15 s and at most
  900 s ahead, and fills its intent as its own; anything else is ignored. More than 8, or not an
  array of objects, is a 400. So when the keeper that issued a ticket is down, the next one fills it.
- **`keeper`** (ticket and build replies): the address of the keeper that answered: it signed the
  ticket, and it is the build's fee payer.
- **account**: the keeper's view of what it has filled for this wallet. `message` is base64 of the
  note opening, `amount u64 LE ‖ blinding [32] ‖ state` (the Borsh `ReceiptState` or
  `PositionState`). The page trusts none of it: it recomputes the note's hash under its own user
  PDA, checks the note is appended and unspent, and decodes the state with the WASM before using it.
  `intents` are not used (the page finds its intents on chain). A wallet may hold several receipts
  in one pool (each supply fill makes one); the page keeps and sums them all, keyed by note hash,
  and drops a receipt of zero shares or a position of no collateral and no debt. `index` (when the
  pool has a reserve) is the pool's supply and borrow price index accrued to chain time `at`, decimal
  strings, 1e12-scaled; the page takes it only if it is not below the last index it saw and not
  above that index grown at the curve's top rate for the time between.
- **ticket**: `id` is 32 lowercase hex (16 bytes), `expiry` unix seconds, `signature` the keeper's
  Ed25519 signature over the ticket bytes below. The page refuses a ticket whose expiry is not
  between 60 s and 900 s after chain time.
- **build**: `transaction` is a base64 wire transaction (at most 4,096 bytes: the v1 message
  followed by its signatures, what `@solana/kit`'s `getTransactionDecoder` reads and `sendTransaction`
  takes; not a bincode or serde encoding of `VersionedTransaction`), the keeper as fee
  payer, signed by the keeper, waiting for the owner's signature. `outputs` are the transaction's
  output hashes in slot order (64 hex each). `now` is the `now` in the instruction args. `bound`
  (draw) is the price bound in the args, 1e12-scaled. `index` is the pool's price index the
  transaction used, 1e12-scaled (supply index for redeem, borrow index for draw); the page needs it
  to recompute the owner's new note state and refuses a build without it, and keeps it only once
  the transaction passed its check.
- **redeem by `amount` or `shares`**: with `amount` the keeper pays exactly that and burns
  `shares_to_burn(amount, index)` (rounded up). With `shares` (a decimal string, the same rules as an
  amount) it burns exactly that many and pays `assets_of(shares, index)` (rounded down). The page
  sends `shares` to withdraw everything, so the receipt ends at zero shares, and `amount` for a
  partial withdrawal. It checks the payout is `assets_of(shares, index)` and the new receipt holds
  `held − shares`. The Worker refuses a redeem with both or neither, and `shares` on a draw.
- **`receipt`** (redeem only, 64 lowercase hex): the note hash of the receipt to spend. The page
  sends it, so the receipt it checks against is the one the keeper spends; the keeper answers 404
  when it holds no such receipt for the wallet. Without it the keeper picks the smallest receipt
  that covers the request (the largest, for `shares`). The Worker refuses it on a draw.
- **price**: on test networks `draw` and `liquidate` read the vault's test price account (PDA
  `["price", vault u64 LE]` of the lend program, 24 bytes: price u64, confidence u64, publish time
  i64, little-endian). The page reads that account through `/api/rpc` (`getAccountInfo`) for the
  vault's price, falling back to `/api/prices` where there is none, so its limits and its bound
  check use what the program reads. It sizes borrowing and withdrawals at
  `(price − confidence) × (1 − 0.5 %)`, the keeper's default draw bound.

## Session authorisation

The wallet signs once per browser session; each request is then signed by a session key.

1. At unlock the page has the wallet's 64-byte signature over the SPR key-derivation message (the
   same one that derives the private keys). It computes
   `session_seed = SHA-256(UTF-8 "keeler-lend-session-v1" ‖ derivation_signature)` and uses it as an
   Ed25519 private key seed. The session key is that key's public key, as a base58 address. It is
   held in memory only and is the same on every unlock of this wallet.
2. `sessionExpiry = chain_now + 86400 − 60`. The page asks for a new authorisation when fewer than
   300 s remain.
3. The wallet signs (Solana `signMessage`) this UTF-8 text, LF line ends, no trailing newline:

```
Keeler Lend: allow this browser session

This lets this browser send lending requests for this wallet to the Keeler keeper until the time below, without asking you each time. It cannot move funds: every transaction still needs your wallet's approval.

Network: <network>
Origin: <the page's origin, e.g. https://keeler.example>
The keeper can see your lending amounts.
Program: <lend program id, base58>
Wallet: <wallet, base58>
Session key: <session key, base58>
Expires: <sessionExpiry, decimal> (<sessionExpiry as ISO 8601 UTC, seconds, e.g. 2027-01-16T08:00:00Z>)
```

   `sessionAuth` is that signature, 128 lowercase hex.

### Signed fields

Every signed request carries:

| Field | Form |
| --- | --- |
| `wallet` | base58 address |
| `sessionKey` | base58 address |
| `sessionExpiry` | unix seconds, number |
| `sessionAuth` | 128 lowercase hex |
| `origin` | the `Origin:` of the authorisation text; one of the keeper's `lend.origins` |
| `nonce` | 32 lowercase hex (16 random bytes) |
| `timestamp` | chain unix seconds, number |
| `signature` | 128 lowercase hex: the session key's signature over the request bytes |

The keeper verifies numbers against their decimal form.

### Request bytes

```
UTF-8 "keeler-lend-request-v1\n" ‖ UTF-8 <METHOD> ‖ "\n" ‖ UTF-8 <path> ‖ "\n"
  ‖ nonce (16 bytes) ‖ timestamp (u64 LE) ‖ SHA-256(canonical)
```

`<path>` is the keeper path (`/lend/account`, `/lend/ticket`, `/lend/build`), not the `/api` one.
`canonical` is every field except `signature`, `nonce` and `timestamp`, keys sorted by byte order,
each as `key=value\n`, numbers in decimal (so `sessionExpiry=1800086000\n`). Keys are
`[A-Za-z0-9_]+`; a value may not contain `=` or a newline.

### What the keeper checks, in order

1. `sessionExpiry` is in the future and at most 86,400 s after the keeper's chain time.
2. `origin` is one of `lend.origins`, and `sessionAuth` is `wallet`'s signature over the
   authorisation text rebuilt from the keeper's own network and program, `origin`, `wallet`,
   `sessionKey` and `sessionExpiry`.
3. `signature` is `sessionKey`'s signature over the request bytes.
4. Freshness: `timestamp` within 120 s of the keeper's chain time, either side.
5. Replay: `(wallet, nonce)` not seen before; the keeper remembers nonces for 240 s (twice the
   window), so an old request cannot come back once its nonce is forgotten.

Any failure is a 401 with `{error}`. The page answers one 401 by dropping its session, asking the
wallet again and resending once.

## Ticket bytes

```
UTF-8 "keeler-lend-ticket-v1" ‖ program (32) ‖ wallet (32) ‖ id (16) ‖ purpose (u8)
  ‖ target (u64 LE) ‖ amount (u64 LE) ‖ expiry (u64 LE)            126 bytes
```

`purpose` is 1 supply, 2 deposit, 3 repay. `target` is the pool id for supply, the position id for
deposit and repay. The pool id is the pool's index in `/api/config` `lend.pools` plus 1. A new
deposit takes a fresh random position id below 2^63, chosen by the page. The page proves an intent
with exactly the ticket's purpose, target, amount and expiry.

## Intents

An intent moves one private note's `amount` into a note the user PDA (`["user", wallet]`) owns
under the pool viewing key the program and the keepers were compiled with (`POOL_VIEWING`, the
page's `VITE_LEND_POOL_VIEWING`), slot 1 (slot 0 is the change). Any keeper of the set can open it. It carries no message:
the keeper knows the wallet from the ticket and scans that wallet's PDA, as for intents the Rust SDK
builds. The page sends it itself (the user pays) and then waits for the keeper to fill it. After
`expiry` the user can cancel it (`cancel_intent`) and take the funds back.

Receipts and positions are the user PDA's notes under the user's own viewing key (`owner_viewing` in
the keeper-built args), so the user can always decrypt them.

`close` is the user's alone: the page builds, proves and pays it (no keeper, no price), for a
position with no debt; the whole collateral comes back at slot 0.

## Pause

The Worker's `PAUSED` switch is the swap's and does not apply to `/api/lend/*`: repayments,
withdrawals and refunds must keep working while liquidations run. Lending is paused at the keeper,
which then refuses new supply, new positions and borrowing with a 403 (`paused` on the page, the
keeper's sentence shown) and keeps serving everything else: repay tickets, deposit tickets into an
open position, redeems and withdrawals. It pauses for a request when its `pause_file` exists, the
request carries `x-keeler-paused: 1` (the Worker's lend pause, `LEND_PAUSED`), or its book does not
add up (its notes' shares differ from the reserve's, logged as a warning) until it does.

## Errors

The keeper answers refusals as a 4xx with `{"error": "<a sentence for people>"}`; the Worker passes
it through (cut to 200 characters). The page maps the status to its error code:

| Status | Page code | Meaning |
| --- | --- | --- |
| 401 | `network` | Session or signature refused (the page retries once with a new session) |
| 403 | `paused` | Lending paused at the keeper (new supply, new positions, borrowing), or borrowing halted while the pool's bad debt exceeds its surplus |
| 404 | `storage` | No such position or receipt |
| 409 | `busy` | The reserve is in use, or this wallet let its reservation lapse unused within `reservation_secs`; try again shortly |
| 410 | `no-liquidity` | Not enough in the pool (checked last, at the amount rounded up to two significant figures) |
| 412 | `price-stale` | The oracle price is missing, unverified or older than 65 s (draws) |
| 422 | `below-minimum` | Under a minimum, or over a cap (pool size, supply per wallet, collateral in all or per position, debt per position) |
| 423 | `unhealthy` | The position would be unhealthy |
| 429 | `rate-limited` | Too many requests: tickets or builds per wallet per hour, open tickets per wallet, open intents in all, or the 8 receipts or positions a wallet may hold (open tickets count) |
| 502, 504 | `keeper-offline` | No keeper answered with its JSON (Worker): no answer, a reply that is not the keeper's, or a keeper's own 5xx `{"error": ...}` (such as a build that took over 25 s) |
| 503 | `keeper-offline` (`paused` if the text says paused) | Keeper not configured, lending paused, or every keeper still starting (Worker) |
| other | `network` | |

A keeper that is starting, or failing to start, inside the dealer answers 503 with
`{"starting": "<sentence>"}` and no `error` field. The Worker treats that as no answer and asks the
next keeper of `LEND_KEEPERS` (up to four, in order); when none is left and the last said
`starting`, it answers 503 "Lending is starting up. Try again in a minute.", otherwise 502. Keeper
URLs must be `https://` unless `NETWORK` is `local`; `NETWORK` takes only `local`, `devnet` or
`mainnet`. A live keeper's own 5xx with
`{"error": ...}` is its answer: it is not retried and reaches the page as a 502.

## Layouts the page checks

The page checks every keeper-built transaction before the wallet sees it (`lend-check.ts`). The
layouts below are read from the program (`lend/program/src/instructions/`, `circuits/`,
`lend/sdk/src/builders/earn.rs`, `lend/sdk/src/vault.rs`) and live in one table,
`web/src/keeler/lend-layout.ts`.

Instruction data is `[tag] ‖ wincode(args) ‖ wincode(TransactIxData)`; each args struct starts
with the 128-byte compressed proof.

| Item | Value |
| --- | --- |
| Tags | init_reserve 1, intent 2, cancel_intent 3, supply 4, redeem 5, deposit_open 6, draw 7, repay 8, liquidate 9, close 10, deposit_add 11, set_test_price 12 |
| intent args | proof, mint [32], viewing [33], user_bump |
| cancel_intent args | proof, viewing [33], user_bump, expiry u64 |
| redeem args | proof, mint [32], pool_viewing [33], pool_bump, owner_viewing [33], user_bump, now u64 |
| draw args | proof, vault u64, pool_viewing [33], pool_bump, owner_viewing [33], user_bump, now u64, bound u64 |
| liquidate args | proof, vault u64, pool_viewing [33], pool_bump, owner [32], owner_viewing [33], user_bump, now u64, bound u64 (the page reads only `bound`, to report a liquidation) |
| close args | proof, vault u64, viewing [33], user_bump |
| Keeper-built account head | keeper (writable signer, fee payer) at 0, owner (read-only signer) at 1; draw always has the price account at 2 |
| Owner-built account head | intent, cancel_intent and close: the owner (writable signer, fee payer) at 0 |
| intent slots | change 0, intent 1 |
| cancel_intent slots | refund 0 |
| redeem slots | receipt 0, reserve 1, payout 2 |
| draw slots | position 0, reserve 1, debt payout 2, collateral payout 3 |
| close slots | payout 0 |
| Owner output blindings | `outputBlinding(first nullifier, poseidon([domain, spent note's blinding]), slot)`; domain `"LRDM"` (redeem, from the receipt) or `"LPOS"` (draw, repay and liquidate, from the position) as a big-endian u32 in a field element; close uses no seed |
| Note state record | the Borsh state as one `utxoData` data record (`DataUtxo::try_from` in `circuits/notes.rs`) |
| close circuit inputs | `{private: {txContext, positionUtxo, positionState}, public: {userOwner, ownerIdentity, collHi, collLo, vault}}`, WASM `closeTransaction` |

The signing check refuses a keeper transaction unless: it decodes; it uses no lookup table and no
durable nonce; the keeper pays its fees and the owner does not; it has exactly one lend instruction
of the requested op, and nothing else but compute-budget instructions without accounts; the owner
is exactly one read-only signer, at head index 1, and the keeper is at index 0; `owner_viewing` in
the args is the owner's viewing key; `now` in the args equals the reply's `now` and is within 60 s
behind and 10 s ahead of chain time; the transact expiry is after chain time and at most 300 s
ahead; it moves nothing out of the pool (no interface transfers); for draw, `bound` equals the
reply's and is within 2 % of the page's reference price; the reply's `outputs` are the
transaction's; its first nullifier is the owner's note; and every note the owner should get
(payouts, the new receipt or position) hashes to the output at its slot.
