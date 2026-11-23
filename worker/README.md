# Keeler Worker

One Cloudflare Worker. It serves the built site (`../web/dist`) and is the page's only
network peer, so the Helius key and the dealer key never reach the browser.

## Routes

| Route | Method | What it does |
| --- | --- | --- |
| `/api/config` | GET | Public config: `{network, rpcPath, sprPath, programId, assets:[{mint,symbol,decimals,maxSell?}], dealers:[{pubkey}], bandBps, minTtlSecs, maxTtlSecs, faucet, paused}` |
| `/api/prices` | GET | `{[mint]: {price: "<int>", expo, publishTime}}` from Pyth Hermes when `PYTH_FEEDS` is set, otherwise the fixed `REFERENCE` values stamped now |
| `/api/rpc` | POST | Solana JSON-RPC proxy. Method allowlist, batches checked per element, 64 KiB body cap |
| `/v1/spr/*` | GET, POST | Helius Privacy Rings indexer and TEE prover proxy. Exact path allowlist, 1 MiB request cap, response cap 10 MiB (indexer) / 1 MiB (prover) |
| `/api/tee-policy` | GET | The signed TEE policy for `devnet`/`mainnet` (the browser checks the signatures). 404 on `local` |
| `/api/quotes` | POST | `{sell_mint, buy_mint, sell_amount, trader}` sent to every dealer's `/quote` (3 s each) → `[{dealer, quote: {buy_amount, expiry, signature}\|null}]`. A signature that is not 128 lowercase hex makes the quote `null`; the Worker never verifies it. 503 while paused |
| `/api/faucet` | POST | `{wallet}` sent to the first dealer's `/faucet`. 404 unless `FAUCET` is `"1"`; 503 while paused |
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
| `HELIUS_API_KEY` | secret | Added as `api-key` to every RPC, indexer and prover request |
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
| `REFERENCE` | var | JSON `{"<mint>":{"price","expo"}}`: fixed prices for mints without a Pyth feed. Ignored on `mainnet` |
| `FAUCET` | var | `"1"` turns on `/api/faucet` |
| `PAUSED` | var | `"1"` stops `/api/quotes` and `/api/faucet` (503); everything else keeps working |
| `PYTH_API_KEY` | secret | Hermes key, sent as `Authorization: Bearer` |
| `PYTH_FEEDS` | var | JSON `{"<mint>":"<feed id hex>"}` |
| `PYTH_URL` | var | Hermes base URL. Default `https://hermes.pyth.network` |
| `PYTH_MAX_AGE_SECS`, `PYTH_MAX_CONF_BPS` | var | Required with `PYTH_FEEDS`. A feed older than this (or newer than now plus this), non-positive, or with confidence wider than this many bps of the price fails the whole call with 503 |

Prices: Hermes is asked for `/v2/updates/price/latest?ids[]=…&parsed=true` (as the CLI does),
checked as `core::price::parse_hermes` and `cli/src/config.rs` do, and cached for 5 s per
isolate; the age is re-checked on every read. A Hermes failure is a 503, never the fixed values.
On `mainnet` every asset in `ASSETS` needs a feed, or `/api/prices` is a 500.

A path below `/v1/spr` is appended to the chosen base, so `/v1/spr/getUserRecords` goes to
`<UPSTREAM_INDEXER>/getUserRecords` and `/v1/spr/prove/x` to `<UPSTREAM_PROVER>/prove/x`.

Rate limits per client IP (bindings in `wrangler.jsonc`): `RPC_LIMIT` 600/min (RPC, TEE policy,
prices), `SPR_LIMIT` 600/min (indexer calls and prover GETs, including proof status polls),
`PROVE_LIMIT` 5/min (POST `/prove/*`, 1,000 Helius credits each), `QUOTE_LIMIT` 30/min,
`FAUCET_LIMIT` 3/min. Without a binding there is no limit.

## Run locally against a local network

Start the local network (`scripts/localnet.sh`), build the site (`cd ../web && pnpm build`),
then put this in `worker/.dev.vars` (git-ignored; placeholder values):

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
npx wrangler secret put PYTH_API_KEY   # only with PYTH_FEEDS
npx wrangler deploy --var NETWORK:devnet --var FAUCET:1 \
  --var DEALERS:'[{"url":"https://<tunnel>","pubkey":"<pubkey>"}]' \
  --var PROGRAM_ID:<id> --var ASSETS:'[...]' --var REFERENCE:'{...}' \
  --var BAND_BPS:100 --var MIN_TTL_SECS:120 --var MAX_TTL_SECS:600
```
