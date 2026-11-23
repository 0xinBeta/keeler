# Keeler web app: plan (revised after review, 2026-11-23)

## Goal

A person opens a `workers.dev` address, picks a wallet, moves test tokens into
a private balance, asks dealers for quotes, swaps privately with the best
in-band quote, and gets a refund if the dealer does not settle. Keys never
leave the browser. Local network first, then devnet. Mainnet only after the
gates at the end.

## Facts checked

- Rings pool `ring1Ga…` and user registry `regy3W…` are executable on devnet and
  mainnet. The swap example program `F4hjig…` is on neither; its README and
  generated verifier say it must not be deployed as is.
- `@heliuslabs/spr@0.9.0-alpha` (Apache-2.0, the only npm release) covers
  register, deposit, transfer, withdraw, balance, history, and exports the
  pieces for program transactions. Browser keys come from
  `LocalKeys.fromDerivationSeed`: one wallet signature over
  `ed25519DerivationMessage(publicKey)`. Rust `ShieldedKeypair::from_keypair`
  derives from the same signature.
- One Helius endpoint serves RPC, the Photon indexer and the attested TEE
  prover on devnet and mainnet. The stored key works on both. The Keychain item
  `keeler-helius-api-key` holds a full URL; code takes its `api-key` parameter
  and also accepts a bare key.
- Spike: with a `wasm` feature added, `zolana zk compile --test` builds a web
  package (7.0 MB WASM, 2.0 MB gzipped) exporting `makeTransaction`,
  `cancelTransaction`, `orderTermsDataUtxo`, `checkProvingKey`, `programProof`,
  `compressedProgramProof`, `prove`. The feature adds four crates to the
  example's `Cargo.lock`. No proof has been made in a browser yet.
- A make needs the pool's transact proof (hosted or local prover) and the make
  circuit proof (ours). Make is about 8.8k constraints and 1,373 bytes as a v1
  transaction.
- Research (2026-11-23): the Default Ring is live and permissionless on
  mainnet since 2026-11-10; a third-party program needs no registration to call
  the pool; the hosted prover proves only the pool's own circuits, so program
  circuits are proved by the client; Helius charges 1,000 credits per proof and
  10 per indexer read (the free plan is about 1,000 proofs); `zk setup` is
  single-party and a production setup is `xark ceremony` phase 2;
  `privacy-ring-examples` has no licence, while a different swap implementation
  sits under Apache-2.0 in `helius-labs/privacy-rings/sdk-tests/zk-program-swap`;
  an official browser wallet example exists in
  `helius-labs/privacy-ring-web-sdk/examples/wallet`. Not confirmed: that
  Phantom signs the derivation message bytes verbatim, or v1 transactions.
- Tooling here: Wrangler 4.152 logged in, `cloudflared`, `solana` CLI, pnpm,
  Node 25, wasm-pack 0.15.1. No Docker.

## Architecture

```
browser (web/)                     Cloudflare Worker (worker/)           this Mac
  wallet (Wallet Standard, or        static assets, CSP                   keeler dealer (HTTP, loopback)
    Keeler's in-page wallet)         POST /api/rpc      -> RPC            cloudflared quick tunnel
  @heliuslabs/spr: balances,         /v1/spr/*          -> indexer+prover swap `take` proofs (native)
    deposit, withdraw, register      GET  /api/tee-policy                 faucet (devnet only)
  swap WASM: make/cancel proofs      GET  /api/config
    (single-threaded, in a Worker)   POST /api/quotes   -> dealers
  band check, best quote             POST /api/faucet   -> dealer
  orders in localStorage
```

- The Worker holds the Helius key and a dealer shared secret. The page sees
  neither.
- Proof requests to the hosted prover are encrypted to the enclave, so the
  Worker sees only ciphertext for those. It does see RPC and indexer traffic,
  and it supplies the reference price and chain reads, so the user trusts the
  Worker for prices and status. The page says so.
- No cross-origin isolation and no prover thread pool: make and cancel are
  small, so one Web Worker proves single-threaded. This keeps wallet popups
  working.

## Interfaces (fixed before the pieces start)

Dealer HTTP, loopback only, every request must carry `x-keeler-key`:

- `POST /quote` body `{"sell_mint","buy_mint","sell_amount":"<decimal string>","trader"}`
  → `{"quote":{"buy_amount":"<decimal string>","expiry":<u64>}}` or
  `{"error":"no quote"}`. Same shapes as `core/src/rfq.rs`, amounts as strings.
- `POST /faucet` body `{"wallet"}` → `{"ok":true}` or `{"error":"…"}`.
- `GET /health` → `{"ok":true,"now":<chain time>}`.

Worker:

- `GET /api/config` → `{ network, rpcPath, sprPath, programId, assets:[{mint,
  symbol, decimals}], dealers:[{pubkey}], bandBps, minTtlSecs, maxTtlSecs,
  reference:{[mint]:{price, expo}}, faucet: boolean }`.
- `POST /api/quotes` body as `/quote` → `[{ "dealer": "<pubkey>", "quote":
  {…} | null }]`, 3 s timeout per dealer.
- `POST /api/faucet` → forwards to the first dealer.
- Worker settings: secrets `HELIUS_API_KEY`, `DEALER_KEY`; var `DEALERS` as JSON
  `[{"url","pubkey"}]`; vars `UPSTREAM_RPC`, `UPSTREAM_SPR` override the Helius
  URLs for the local network; var `NETWORK`.

TypeScript library (`web/src/keeler/index.ts`), which the interface builds on:

```ts
type Asset = { mint: string; symbol: string; decimals: number };
type Quote = { dealer: string; sell: Asset; buy: Asset; sellAmount: bigint;
               buyAmount: bigint; expiry: number; inBand: boolean; vsFairBps: number };
type OrderStatus = 'pending' | 'locked' | 'settled' | 'refundable' | 'refunded' | 'failed';
type Order = { id: string; sell: Asset; buy: Asset; sellAmount: bigint;
               buyAmount: bigint; expiry: number; status: OrderStatus; error?: string };
type SwapStep = 'checking' | 'proving' | 'signing' | 'locked';
interface Keeler {
  config(): Promise<Config>;
  connect(wallet: Wallet): Promise<{ address: string }>;   // Wallet Standard
  unlock(): Promise<void>;
  isRegistered(): Promise<boolean>; register(): Promise<void>;
  balances(): Promise<{ asset: Asset; public: bigint; private: bigint; largestNote: bigint }[]>;
  deposit(asset: Asset, amount: bigint): Promise<void>;
  withdraw(asset: Asset, amount: bigint): Promise<void>;
  quotes(sell: Asset, buy: Asset, sellAmount: bigint): Promise<{ fair: bigint; quotes: Quote[] }>;
  swap(q: Quote, on: (step: SwapStep) => void): Promise<Order>;
  orders(): Promise<Order[]>;
  refund(orderId: string): Promise<void>;
  faucet(): Promise<void>;
  chainNow(): Promise<number>;
}
```

## Before the pieces start (main session)

Scaffold `web/` (pnpm, Vite, React, TypeScript, Vitest, Playwright, the
dependency list, `src/keeler/index.ts` with the types above and a mock
implementation) and `worker/` (Wrangler config), so no two pieces create the
same files. Ownership: A `cli/ core/ scripts/ patches/`; B `web/src/keeler/**`,
`web/tests/**`; C `web/src/ui/**`, `web/src/main.tsx`, `web/index.html`,
`web/src/styles/**`; D `worker/**`. A piece that needs a new dependency adds it
and says so; lockfiles are regenerated at integration.

## Pieces

### A. Chain and dealer (Rust)

1. `patches/swap.patch` applied by `scripts/setup.sh` (idempotent): the `wasm`
   feature, the `circuits` module gate, the matching `Cargo.lock` hunk, and
   `declare_id!` taken from `KEELER_SWAP_PROGRAM_ID` at setup time (default:
   the example's ID). `declare_id!` is the only place the ID is written.
2. `setup.sh` builds the web package (`--single-threaded`,
   `--wasm-out web/public/swap`) and copies `pk.bin`, `vk.bin` and
   `circuit.xbc` for make and cancel beside it. Git-ignored.
3. `config.rs`/`chain.rs`: a `network` section (`rpc_url`, `indexer_url`,
   `prover: local | helius`, Helius key from the Keychain, never logged).
   **Proving must not go through the swap SDK's `prove`**, which hard-codes the
   local prover: call `.build()`, then `client.prove_transact(...)`, check the
   private transaction hash, then `swap_sdk::proof::prove`, and build the
   `Make`/`Take`/`Cancel` instruction from their public fields.
4. `dealer.rs`: the HTTP endpoint above next to the TCP one, same handler;
   `tiny_http`; loopback only; 4 KiB body cap; constant-time check of
   `x-keeler-key`. Poll interval configurable (default 1 s local, 5 s remote).
   The quote book drops expired quotes before refusing as full.
5. Faucet (off unless configured): two dealer-minted test tokens, tUSD (6
   decimals) and tSOL (9 decimals), plus 0.02 SOL for fees; per-wallet daily
   limit and a global daily cap; paid from a faucet key that is not the dealer
   or deploy key.
6. `scripts/localnet.sh`: starts the local network with the swap program,
   funds and registers a dealer, creates the two test tokens, runs
   `keeler dealer` with HTTP, prints the settings the Worker needs. Used by
   Playwright and by hand.
7. `scripts/deploy-devnet.sh`: creates deploy, dealer and faucet keys in the
   Keychain if absent, reads them by process substitution (never a file),
   funds them, builds with our program ID, deploys, creates the two tokens and
   their pool interfaces, deposits dealer inventory. Idempotent. **Not run
   until the owner approves publishing the example's code.**
8. `scripts/run-dealer.sh`: dealer plus `cloudflared` quick tunnel; writes
   `DEALERS` to the Worker.
9. Tests: HTTP handler unit tests (quote, bad key, oversize body, non-loopback
   refused, faucet caps); existing e2e still pass; a new e2e that quotes over
   HTTP and settles through the new proving path.

### B. Swap client (TypeScript library)

First milestone, before anything else: one make transaction built, proved and
confirmed from a browser page against `scripts/localnet.sh`.

- Wallets: any Wallet Standard wallet that signs v1 transactions and the raw
  derivation message, plus Keeler's own in-page wallet (an Ed25519 key in
  `localStorage`, registered through Wallet Standard). The in-page wallet is
  what the tests use and the fallback if an extension wallet cannot sign v1.
- Unlock: sign the derivation message twice and require identical signatures;
  if the wallet is already registered, the derived keys must match its
  registry record, otherwise stop with a clear error. The signature stays in
  memory only.
- Make: inputs from the synced wallet (smallest note that covers the amount,
  change returned, as the CLI trader does; no split). `makeTransaction` →
  `decodeProgramTransaction(finalizedTx, assets)` → rebuild external data with
  the order marker (`createExternalData`, `SppProofInputs`; view tag = taker's
  `confidentialViewTag()`, data = order hash ‖ maker pubkey) →
  `LocalKeys.transactionKeys` → `client.proveTransact` → compare
  `privateTxHash` with `paddingIndependentPrivateTxHash` → prove the circuit →
  `checkProvingKey` → `compressedProgramProof` → wincode `MakeIxData` and the
  account list exactly as `sdk/src/instructions/make/instruction.rs` →
  `compileUnsignedTransaction` → wallet signs → send and confirm. Cancel the
  same way with `NullifierKeyProofAuthority(ProgramOwner.nullifierKey())`.
  If the hosted prover refuses this transaction shape, use
  `proofDataSource: "client"` with `fetchProvingKeySet`.
- Golden tests: the TypeScript instruction bytes equal the Rust bytes for a
  fixed input (A exports a fixture).
- `price.ts`: `fairBuyAmount`, `withinBand`, `bestQuote` with `bigint`, tested
  against the Rust vectors.
- `swap()`: asks for a fresh quote from the chosen dealer at click, re-checks
  the band and the TTL bounds against chain time, refuses a quote with less
  than the minimum TTL left, writes the order record under its own storage key
  **before** sending (abort if the write fails), then proves and sends. A
  second call while one is in flight is refused.
- Status by nullifier as the CLI does; `refundable` only when chain time is
  past expiry. Amounts are strings on the wire and `bigint` in memory.
- The program ID comes from `/api/config`; if it differs from the one the
  WASM was built for, the first proof fails and the error says so.
- Tests: Vitest for price, encoding, storage; Playwright: swap → settle, and
  swap → refund, on the local network.

### C. Interface (React)

Built on the `Keeler` interface with the mock until B lands. See "Design".

### D. Worker

- Static assets from `web/dist`; CSP `default-src 'self'; script-src 'self'
  'wasm-unsafe-eval'; connect-src 'self'; frame-ancestors 'none'`; long cache
  for hashed files.
- `/api/rpc`: JSON-RPC method allowlist, batches checked element by element,
  64 KiB body cap.
- `/v1/spr/*`: exact allowlist of indexer and prover paths, GET and POST,
  forwards the query string and the `Spr-Tee`, `Spr-Tee-Enc`,
  `Spr-Tee-Ciphertext` headers, 1 MiB response cap.
- `/api/tee-policy`: the signed policy fetched server-side (signatures are
  checked in the browser).
- `/api/quotes`, `/api/faucet`: add `x-keeler-key`; per-IP rate limits.
- `/api/config` as above. Reference prices are fixed values in the Worker
  config; the test tokens have no market.
- Tests with the Workers Vitest pool: allowlists, caps, batch handling, header
  forwarding, fan-out with a slow and a failing dealer.

## Design

Reference: helius.dev/privacy (black page, luminous ring hero, quiet dark
cards, pill buttons). Keeler takes that register and its own identity from its
name: the Keeler Gap, a thin dark lane in Saturn's A ring kept open by the
moon Daphnis, which raises waves on the gap's edges as it passes.

- **Hero**: the swap panel is in the hero. Behind it, a tilted particle ring
  (raw WebGL2 points, additive blending, Keplerian drift) with one dark gap; a
  small bright moon orbits in the gap and drags waves along both edges. The
  ring takes three inputs from the app: dealers answering (lights on the
  ring), proving (edge waves grow), settled (one bright front runs round).
  `prefers-reduced-motion` and no-WebGL get a still frame. Paused when hidden;
  device pixel ratio capped.
- **Colour**: void `#000000`, surface `#0B0C0F`, line `#1E2127`, ice `#F4EBD6`
  (text), dust `#8D93A1` (secondary), Saturn gold `#E9B872` (actions, ring
  body), ember `#FF6A3D` (ring's hot edge and live states only). AA contrast.
- **Type**: Unbounded (display, light weights, large) and Onest (interface,
  `tabular-nums`), self-hosted. The page makes no third-party request.
- **Layout**: left-aligned. Hero: headline and one sentence left, swap panel
  right, ring behind both and off the right and bottom edges. Below: how a
  swap works (three real steps), what is public and what is private (a plain
  table), limits. One column under 800 px, swap panel first.
- **Swap flow**: choose wallet → unlock (says what the signature does) →
  amounts (maximum = largest single note) → quotes (distance from reference
  price, countdown, out-of-band ones disabled with the reason) → progress
  (checking, proving, sign, locked, settled) → refund becomes the primary
  action after expiry. Dealer offline has its own message.
- **Floor**: keyboard reachable, visible focus, `aria-live` status, 44 px
  targets, errors say what happened and what to do, a persistent "devnet, test
  tokens, unaudited" banner.

## Order of work

1. Scaffold (main session), commit.
2. A, B, C, D in parallel worktrees.
3. Integrate on the local network: Playwright end to end.
4. Fresh code review, fixes.
5. Devnet (needs the owner's go-ahead to publish): deploy, run dealer, deploy
   Worker, run the flow by hand with the in-page wallet and with Phantom.

## Known limits of this version

- Orders live in `localStorage`; losing it before expiry loses the browser's
  ability to refund. Acceptable for test tokens only.
- One dealer on one Mac behind a quick tunnel.
- Quotes are unsigned and reserve no inventory; a dealer can win and not
  settle, and the funds stay locked until expiry (at least 120 s TTL).
- The local network proves with client-side proof data; the hosted-prover
  path is exercised only on devnet.

## Path to mainnet (owner's decisions, 2026-11-23)

Order: devnet working → fill the product gaps (item 5) → audit → key ceremony →
mainnet.

1. Licence: the owner says Helius is fine with shipping the swap example.
   Devnet publication approved. Worth having in writing before mainnet.
2. Audit: the owner will run it with their Daybreak access, after the code
   stops changing. Legal: the owner handles it.
3. Proving keys from a real Xark phase-2 ceremony with at least one outside
   contributor. A setup run only by the dealer's operator lets the dealer
   forge a take. Then: drop `insecure-test-setup`, `export-verifier
   --checksums`, `zolana vks check --program-id --expect`, and the browser
   refuses any key marked `insecureTestSetup`.
4. Deployment: about 0.65 SOL, a multisig upgrade authority (a compromised
   authority can block refunds), a kill switch, a canary with a hard
   per-order cap.
5. Product: orders rediscovered from the chain, signed quotes, a live price
   check at settlement, an npm SDK release that matches the deployed programs,
   a custom domain, an always-on dealer host with real inventory.


## Round 2: product gaps (added 2026-11-23, revised after review)

Each item goes to the piece that owns the files.

### G1. Signed quotes (A, B, D)

- The dealer signs every quote with its Solana Ed25519 key over a fixed
  167-byte message: `"keeler-quote-v1"` ‖ program ID (32) ‖ sell mint (32) ‖ buy
  mint (32) ‖ trader (32) ‖ sell amount (u64 LE) ‖ buy amount (u64 LE) ‖ expiry
  (u64 LE).
- `Quote` gains `signature`: `[u8; 64]` in Rust (so `Quote` stays `Copy`), 128
  hex characters on TCP and HTTP. The Worker passes it through after checking
  its shape.
- The CLI trader and the browser verify it against the dealer's public key
  and drop a quote that fails. Dealer keys are compiled into the site
  (`VITE_DEALERS`) and must match `/api/config`.
- What this buys: a hijacked tunnel address, a leaked dealer secret or a wrong
  `DEALERS` setting cannot produce quotes. What it does not buy: protection
  from whoever operates the Worker (it serves the page itself), or any
  on-chain enforcement.
- The dealer keeps its quote book and bound orders in a state file and
  reloads them on start, dropping expired entries, so a restart does not
  strand a trader who holds a valid quote.

### G2. Prices (A, D, B, C)

- Dealer, last look: before settling, recompute the fair amount at the
  current price and decline if `buy_amount > fair_now × (1 +
  settle_tolerance_bps)` (default 0: never settle at a loss against the
  current price). Prices are fetched once per poll; a failed fetch means do
  not settle. The trader refunds after expiry.
- Dealer limits: `max_sell_amount` per mint per order and `hourly_sell_cap`
  per mint (rolling hour over bound orders).
- The page says it plainly: "The dealer may decline if the price moves before
  it settles. Your funds stay locked until the quote expires, then you can
  take them back."
- Price source (changed 2026-11-23: Pyth's Hermes API now needs a paid key,
  so nothing uses it): Pyth's on-chain price feed accounts, read through the
  RPC we already have. Checked on mainnet: SOL/USD
  `7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE` and USDC/USD
  `Dpw1EAVrSB1ibxiDQyTAW6Zip3J4Btk2x4SgApQCeFbX` are fully verified and 22–35 s
  old. A reader must check: the account owner is Pyth's receiver program
  `rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ`, verification level is Full,
  the feed id is the configured one, price > 0, age and confidence within
  limits. Layout: 8-byte discriminator, 32-byte write authority, verification
  level (1 byte for Full), feed id (32), price i64, conf u64, exponent i32,
  publish time i64, all little-endian.
- Worker: `GET /api/prices` → `{[mint]: {price: "<int>", expo, publishTime}}`.
  With `PYTH_ACCOUNTS` (mint → `{account, feedId}`) it reads those accounts
  with one `getMultipleAccounts`, applies the checks above with
  `PYTH_MAX_AGE_SECS` and `PYTH_MAX_CONF_BPS`, caches 5 s and re-checks age on
  every read. A failed read or check is a 503, never a fall-back to fixed
  values. Without `PYTH_ACCOUNTS` it returns the fixed `REFERENCE` values; on
  `mainnet` a mint without an account is an error.
- Dealer: the same source and checks in Rust replace the Hermes source in
  `cli/src/config.rs` and `core/src/price.rs`.
- Browser: the band check uses `/api/prices`; the quotes view shows the
  price's age.

### G3. Orders found again from the chain (B)

- Port the swap SDK's maker scan (`sdk/src/index/`, `scan_maker`): it rebuilds
  the whole order (terms, blinding, source) from chain data; the leaf index
  comes from `getMerkleProofs`. Every indexer method it needs is already on
  the Worker's allowlist.
- `Order.id` is the order hash, so local and chain records merge. A local
  `locked` order the indexer has not shown yet is kept.
- The scan runs on unlock and on a "Find my orders" button only. Settled and
  refunded orders are stored and never checked again.
- Test: lock an order, delete only the order records from `localStorage`
  (the in-page wallet key stays), reload, the order is listed, and after
  expiry the refund succeeds.

### G4. Limits and pause (A, D, B, C)

- `/api/config` assets carry `maxSell`; the form's maximum is the smaller of
  `maxSell` and the largest note.
- Worker `PAUSED=1`: `/api/config` says `paused: true`; `/api/quotes` and
  `/api/faucet` return 503. The page shows "Swaps are paused" and disables
  quoting. Refunds, balances and withdrawals keep working.
- Dealer pause file: while it exists the dealer gives no new quotes and keeps
  settling.
- `/v1/spr/*` gets its own rate limit, stricter on `/prove/*` (1,000 Helius
  credits each).
- `proving-keys.json` is bundled into the site at build time; the browser
  checks each fetched proving key against it, and on `mainnet` refuses a key
  marked `insecureTestSetup`.

### G5. Deployment (A, scripts only, not run)

- `scripts/set-upgrade-authority.sh <program> <new-authority>`: checks the
  new authority account exists on the cluster, sets it, reads it back. The
  upgrade authority can change the program and therefore spend every open
  order, so on mainnet handing it to a multisig is mandatory and happens
  before the Worker points at the program.
- `run-dealer.sh` restarts the dealer and the tunnel with backoff and rewrites
  the Worker's `DEALERS` each time the tunnel address changes.
- A mainnet deploy script is not written until the key ceremony. It must not
  create tokens, run a faucet or move inventory; real funds are moved by the
  owner.

Not in this round: the always-on dealer host (needs a machine or a paid
plan; owner's choice), the custom domain, the key ceremony, the npm SDK
release.

Interface additions for this round (`web/src/keeler/index.ts`):
`Config.paused: boolean`; `Asset.maxSell?: bigint`; `quotes()` resolves to
`{ fair, priceTime, quotes }` (`priceTime` in unix seconds);
`rescanOrders(): Promise<Order[]>`; `KeelerError` code `"paused"`.
