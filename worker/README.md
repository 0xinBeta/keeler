# Keeler Worker

One Cloudflare Worker. It serves the built site (`../web/dist`) and is the page's only
network peer, so the Helius key and the dealer key never reach the browser.

## Routes

| Route | Method | What it does |
| --- | --- | --- |
| `/api/config` | GET | Public config: `{network, rpcPath, sprPath, programId, assets:[{mint,symbol,decimals,maxSell?}], dealers:[{pubkey}], bandBps, minTtlSecs, maxTtlSecs, faucet, paused, lend?: {programId, pools, vaults}}` |
| `/api/prices` | GET | `{[mint]: {price: "<int>", expo, publishTime}}` from Pyth's on-chain price accounts when `PYTH_ACCOUNTS` is set, otherwise the fixed `REFERENCE` values stamped now and marked `"fixed": true` |
| `/api/rpc` | POST | Solana JSON-RPC proxy. Method allowlist, batches checked per element, 64 KiB body cap |
| `/v1/spr/*` | GET, POST | Helius Privacy Rings indexer and TEE prover proxy. Exact path allowlist, 1 MiB request cap, response cap 10 MiB (indexer) / 1 MiB (prover) |
| `/api/tee-policy` | GET | The signed TEE policy for `devnet`/`mainnet` (the browser checks the signatures). 404 on `local` |
| `/api/quotes` | POST | `{sell_mint, buy_mint, sell_amount, trader}` sent to every dealer's `/quote` (3 s each) → `[{dealer, quote: {buy_amount, expiry, signature}\|null}]`. A signature that is not 128 lowercase hex makes the quote `null`; the Worker never verifies it. 503 while paused |
| `/api/faucet` | POST | `{wallet}` sent to the first dealer's `/faucet`. 404 unless `FAUCET` is `"1"`; 503 while paused |
| `/api/lend/pools` | GET | Keeper `GET /lend/pools`. No query. See "Lending" below |
| `/api/lend/account` | GET | Keeper `GET /lend/account?wallet&nonce&timestamp&sessionKey&sessionExpiry&sessionAuth&signature` |
| `/api/lend/ticket` | POST | Keeper `POST /lend/ticket`. Not affected by `PAUSED` |
| `/api/lend/build` | POST | Keeper `POST /lend/build` (30 s). Not affected by `PAUSED` |
| everything else | | Static files. HTML gets the CSP, `nosniff` and `no-referrer`; `/assets/*-<hash>.*` is cached for a year |

Allowed RPC methods: `getAccountInfo getMultipleAccounts getProgramAccounts getLatestBlockhash
getBalance getTokenAccountBalance getTokenAccountsByOwner getSignatureStatuses getTransaction
sendTransaction simulateTransaction getSlot getMinimumBalanceForRentExemption getEpochInfo
getBlockHeight getBlockTime`.

Allowed `/v1/spr` paths (from `@heliuslabs/spr@0.9.0-alpha`):

| Path | Method | Goes to |
| --- | --- | --- |
| `/getEncryptedUtxosByTags`, `/getShieldedTransactionsByTags`, `/getShieldedTransactionsByNullifiers`, `/getShieldedTransactionsBySignature`, `/getMerkleProofs`, `/getNonInclusionProofs`, `/getUserRecords` | POST | indexer |
| `/attestation`, `/health`, `/proving-keys`, `/prove/<key>/status` | GET | prover |
| `/prove/<key>`, `/prove/<key>/indexed` | POST | prover |

Forwarded request headers: `content-type`, `Spr-Tee`, `Spr-Tee-Enc`, `Spr-Tee-Ciphertext`,
`X-Sync`, `X-Async`. Forwarded response headers: `content-type`, `Spr-Tee`, `retry-after`.

Cross-origin requests to `/api/*` and `/v1/*` are refused (403) and no CORS headers are sent.

## Settings

| Name | Kind | Meaning |
| --- | --- | --- |
| `HELIUS_API_KEY` | secret | Added as `api-key` to RPC, indexer and prover requests sent to the built-in Helius hosts only |
| `DEALER_KEY` | secret | Sent to dealers as `x-keeler-key` |
| `NETWORK` | var | `local`, `devnet` or `mainnet` |
| `DEALERS` | var | JSON `[{"url","pubkey"}]` |
| `UPSTREAM_RPC` | var | RPC URL. Default: `https://<network>.helius-rpc.com/`, or `http://127.0.0.1:8899` on `local` |
| `UPSTREAM_SPR` | var | Base URL for both indexer and prover paths. Default: `https://<network>.helius-rpc.com/v1/spr` |
| `UPSTREAM_INDEXER` | var | Overrides `UPSTREAM_SPR` for the indexer paths. Default on `local`: `http://127.0.0.1:8784` |
| `UPSTREAM_PROVER` | var | Overrides `UPSTREAM_SPR` for the prover paths. Default on `local`: `http://127.0.0.1:3001` |
| `PROGRAM_ID` | var | Swap program ID |
| `ASSETS` | var | JSON `[{"mint","symbol","decimals"}]` |
| `BAND_BPS`, `MIN_TTL_SECS`, `MAX_TTL_SECS` | var | Numbers |
| `REFERENCE` | var | JSON `{"<mint>":{"price","expo"}}`: fixed prices for mints without a Pyth account. Ignored on `mainnet` |
| `FAUCET` | var | `"1"` turns on `/api/faucet` |
| `PAUSED` | var | `"1"` stops `/api/quotes` and `/api/faucet` (503); everything else keeps working. It does not apply to `/api/lend/*`: repayments and withdrawals must work while liquidations run, so lending is paused at the keeper, which refuses only new supply, new positions and borrowing (a 403) |
| `PYTH_ACCOUNTS` | var | JSON `{"<mint>":{"account":"<base58>","feedId":"<64 hex>"}}`: Pyth price update accounts (e.g. mainnet SOL/USD `7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE`) |
| `PYTH_MAX_AGE_SECS`, `PYTH_MAX_CONF_BPS` | var | Required with `PYTH_ACCOUNTS`. A price older than this (or newer than now plus this), non-positive, or with confidence wider than this many bps of the price fails the whole call with 503 |
| `LEND_PROGRAM_ID` | var | Lend program ID (base58) |
| `LEND_POOLS` | var | JSON `[{"mint","curve":[{"utilisationBps","rateBps"}],"feeBps","maxUtilisationBps","minSupply"}]`. The curve has 2 to 8 points, utilisation strictly rising from 0 to 10000 |
| `LEND_VAULTS` | var | JSON `[{"id","collateralMint","debtMint","cfBps","ltBps","lmlBps","penaltyBps","minCollateral","minDebt"}]`. `id` 0 to 65535 and unique, `debtMint` one of the pools, `cfBps <= ltBps <= lmlBps` |

## Lending

With none of `LEND_PROGRAM_ID`, `LEND_POOLS` and `LEND_VAULTS` set, `/api/config` has no `lend`
and every `/api/lend/*` route is a 404. With any of them set, all three must be valid or
`/api/config` and the lend routes are a 500. `/api/config` then carries
`lend: {programId, pools, vaults}` exactly as configured.

Each route goes to the keeper, which for now is the first `DEALERS` entry, with `x-keeler-key`.
Bodies and queries must have exactly the fields below (unknown fields are refused, 400);
amounts and ids are decimal strings that fit a u64, `timestamp`, `expiry`, `now` and `at` are
whole seconds, `vault` is 0 to 65535. The Worker forwards only the checked fields, and returns
only a reply of exactly the shape below; anything else is a 502. A keeper 4xx with
`{"error": "<text>"}` is passed through (text cut to 200 characters). Bodies and replies are
capped at 16 KiB. Timeout 30 s for `build`, 10 s for the rest.

Signed fields, on `ticket`, `build` and `account` (the keeper checks the signatures and freshness;
the Worker checks shape only): `wallet` (base58), `nonce` (32 lowercase hex), `timestamp`,
`sessionKey` (base58, the browser's session key), `sessionExpiry` (unix seconds), `sessionAuth`
(128 lowercase hex: the wallet's signature over the authorisation text), `signature` (128 lowercase
hex: the session key's signature over the request). In the `account` query, `timestamp` and
`sessionExpiry` are read as numbers. The byte layouts, the authorisation text and the freshness and
replay rules are in `docs/lend-api.md`.

| Route | Request | Reply |
| --- | --- | --- |
| `pools` | none | `{pools: [{mint, at, size, utilisationBps, supplyRateBps, borrowRateBps}]}`, at most 8 |
| `account` | query: signed fields | `{receipts: [{pool, shares, message}], positions: [{vault, position, collateral, debtShares, message}], intents: [{id, purpose, amount, expiry, status}]}`, each at most 8; `message` is base64 up to 512 characters; `status` is `waiting`, `filled` or `refundable`; optional `index: {supply, borrow, at}` (decimal strings, chain time) |
| `ticket` | signed fields, `purpose` (`supply`, `deposit`, `repay`), `amount`, optional `pool`, `vault`, `position` | `{id (32 hex), expiry, signature (128 hex)}` |
| `build` | signed fields, `op` (`redeem`, `draw`), optional `pool`, `vault`, `position`, `amount`, `shares`, `borrow`, `withdraw`, `receipt` (64 hex); a redeem has exactly one of `amount` and `shares`, and only a redeem takes `shares` or `receipt` | `{transaction (base64, at most 5,464 characters = 4,096 bytes), outputs: [64 hex] (1 to 8), now, bound?, index?}`; `index` is the pool's price index (1e12-scaled) the transaction used |

Which optional fields an operation needs is the keeper's check, not the Worker's.

Prices: one `getMultipleAccounts` (base64) to the RPC upstream reads every configured account.
Each must exist, be owned by Pyth's receiver `rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ`,
start with the PriceUpdateV2 discriminator (first 8 bytes of sha256 of `account:PriceUpdateV2`),
be fully verified (byte 40 = 1), and hold the configured feed id; then the price, age and
confidence checks above apply. The result is cached for 5 s per isolate and the age is re-checked
on every read. Any failure is a 503 naming the asset, never the fixed values. On `mainnet` every
asset in `ASSETS` needs an account, or `/api/prices` is a 500.

A path below `/v1/spr` is appended to the chosen base, so `/v1/spr/getUserRecords` goes to
`<UPSTREAM_INDEXER>/getUserRecords` and `/v1/spr/prove/x` to `<UPSTREAM_PROVER>/prove/x`.

## Rate limits

Bindings in `wrangler.jsonc`, all with a 60 s period. Per-client limits key on the IPv4 address,
or on the IPv6 /64, since one subscriber usually holds a whole /64. Global limits use one
constant key for everyone. These are settings: tune them to your Helius plan. A proof costs
1,000 credits, an indexer read 10, and most RPC calls 1.

| Binding | Limit / 60 s | Key | Counts |
| --- | --- | --- | --- |
| `RPC_LIMIT` | 600 | client | each JSON-RPC call (a batch counts each call), TEE policy, prices |
| `RPC_GLOBAL_LIMIT` | 1200 | global | each JSON-RPC call |
| `SPR_LIMIT` | 300 | client | indexer calls and prover GETs (including proof status polls) |
| `SPR_GLOBAL_LIMIT` | 1200 | global | same |
| `PROVE_LIMIT` | 20 | client | POST `/prove/*` (a normal flow uses about 4 proofs in a few minutes) |
| `PROVE_GLOBAL_LIMIT` | 30 | global | POST `/prove/*` |
| `QUOTE_LIMIT` | 30 | client | `/api/quotes` |
| `FAUCET_LIMIT` | 3 | client | `/api/faucet` |
| `LEND_READ_LIMIT` | 60 | client | `/api/lend/pools`, `/api/lend/account` |
| `LEND_READ_GLOBAL_LIMIT` | 600 | global | same |
| `LEND_TICKET_LIMIT` | 10 | client | `/api/lend/ticket` |
| `LEND_TICKET_GLOBAL_LIMIT` | 60 | global | same |
| `LEND_BUILD_LIMIT` | 6 | client | `/api/lend/build` (each is two keeper proofs) |
| `LEND_BUILD_GLOBAL_LIMIT` | 12 | global | same |

The client limit is checked first. A global limit is charged only after the request passed
every check (method, path, size, content type, shape), so malformed requests cannot use up
everyone's share. A proof POST must be a JSON object (`application/json`) or a TEE-encrypted
body (`application/octet-stream` with `Spr-Tee`, `Spr-Tee-Enc` and at least 16 bytes);
anything else is a 400 or 415 before `PROVE_GLOBAL_LIMIT` is touched.

When a binding is missing, `NETWORK=local` means no limit. On any other network the request
is refused with a 503.

Other RPC limits:
- A batch holds at most 10 calls. A larger batch is refused whole.
- `getProgramAccounts` is allowed only for the user registry
  (`regy3WAeRXMYuMYxr5iK3Bf9Arr3PcxGxyzAddbiqDQ`), the pool
  (`ring1GaLCuJnkUMVkX2JeZLTTX17wKsV4fZmPVzXANW`) and `PROGRAM_ID`. These are the only
  programs the SDK scans.
- The Worker forwards the parsed and checked call, re-serialised with only `jsonrpc`, `id`,
  `method` and `params`. A call holding an integer above 2^53 is refused, because
  re-serialising it would change its value.
- The Helius key is added only for `devnet.helius-rpc.com` and `mainnet.helius-rpc.com`.
  An `UPSTREAM_*` override host never receives it.
- Every `/api/*` and `/v1/*` reply carries `X-Content-Type-Options: nosniff` and
  `Content-Security-Policy: default-src 'none'`. `/api/rpc` replies are always
  `application/json`.

### What an anonymous visitor can cost you

Anyone can load the page and call these routes without signing in. At the global limits above,
the worst case per minute is:
- about 30,000 credits from proofs (30 × 1,000);
- up to 12,000 credits from indexer reads (1,200 × 10);
- about 1,200 credits or more from RPC (a scan of the registry or pool costs more than 1).

That totals about 43,000 credits a minute, or roughly 60 million a day, if someone saturates
every limit around the clock. Two things raise that ceiling:
- Workers rate limits only allow periods of 10 s or 60 s, so a daily budget cannot be set here.
- The counters are kept per Cloudflare location, so a spread-out attacker can reach the global
  numbers in several locations at once.

To lower it: set the global limits to what your plan can absorb, watch Helius usage, and set
`PAUSED=1` or delete the Worker if spend spikes. `sendTransaction` and `simulateTransaction`
stay open (wallets need them) and count under the RPC limits.

## Run locally against a local network

`scripts/dev.sh [offset] [port]` does all of this in one command: it starts the local network,
builds the site with `VITE_DEALERS`, writes `worker/.dev.vars` from the network's
`target/localnet/keeler-web-<offset>.json` and runs `wrangler dev`; Ctrl-C stops it all.

By hand: start the local network (`scripts/localnet.sh`), build the site
(`cd ../web && VITE_NETWORK=local VITE_DEALERS='["<dealer pubkey>"]' pnpm build`; the site refuses a
service whose `NETWORK` differs from `VITE_NETWORK`), then put this in
`worker/.dev.vars` (git-ignored; placeholder values):

```sh
NETWORK=local
DEALER_KEY=replace-with-the-dealer-key
DEALERS=[{"url":"http://127.0.0.1:<dealer-http-port>","pubkey":"REPLACE_WITH_DEALER_PUBKEY"}]
PROGRAM_ID=REPLACE_WITH_PROGRAM_ID
ASSETS=[{"mint":"REPLACE_MINT_A","symbol":"tUSD","decimals":6,"maxSell":"1000000000"},{"mint":"REPLACE_MINT_B","symbol":"tSOL","decimals":9}]
BAND_BPS=100
MIN_TTL_SECS=120
MAX_TTL_SECS=600
REFERENCE={"REPLACE_MINT_B":{"price":"150","expo":0}}
FAUCET=1
# Only if they differ from the SDK's local ports:
# UPSTREAM_RPC=http://127.0.0.1:8899
# UPSTREAM_INDEXER=http://127.0.0.1:8784
# UPSTREAM_PROVER=http://127.0.0.1:3001
```

```sh
pnpm install
pnpm dev          # http://localhost:8787
pnpm test
npx tsc --noEmit
```

## Deploy

```sh
cd ../web && pnpm build && cd ../worker
npx wrangler secret put HELIUS_API_KEY
npx wrangler secret put DEALER_KEY
npx wrangler deploy --var NETWORK:devnet --var FAUCET:1 \
  --var DEALERS:'[{"url":"https://<tunnel>","pubkey":"<pubkey>"}]' \
  --var PROGRAM_ID:<id> --var ASSETS:'[...]' --var REFERENCE:'{...}' \
  --var BAND_BPS:100 --var MIN_TTL_SECS:120 --var MAX_TTL_SECS:600
```
