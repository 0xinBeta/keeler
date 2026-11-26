# Mainnet runbook

The whole launch, in order. Nothing here has been run on mainnet. **[operator]** marks a step that
needs the operator in person: funds, keys, signers, hosts or the domain. Do the steps in order; each list is a
checklist. No secret goes on a command line: each is read from the Keychain by command
substitution into an environment variable or a pipe.

Mainnet values used below:

| What | Value |
|---|---|
| Genesis hash | `5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d` |
| Collateral (SOL, as the pool sees it) | `11111111111111111111111111111111` |
| Debt (USDC) | `EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v` |
| SOL/USD Pyth push account (shard 0) | `7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE` |
| SOL/USD feed id | `ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d` |
| USDC/USD Pyth push account (shard 0), for the Worker's `PYTH_ACCOUNTS` only | `Dpw1EAVrSB1ibxiDQyTAW6Zip3J4Btk2x4SgApQCeFbX` |
| USDC/USD feed id | `eaa020c61cc479712813461ce153894a96a6c00b21ed0cfc2798d1f9a9e9c94a` |
| Squads v4 program | `SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf` |

Both Pyth accounts were read on 2026-11-25: owned by Pyth's receiver
`rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ`, fully verified, holding the feed id above.

The debt token is valued at exactly 1 USD, by the program and by the page (whatever
`/api/prices` says of USDC; the Worker needs a USDC account only because on mainnet every asset in
`ASSETS` must have one). If USDC trades below 1, liquidations come early, never late.

## 1. Freeze

- [ ] Every circuit, the program, the keeper and the page are final. A circuit change after the
      ceremony means a new ceremony.
- [x] The liquidation penalty and the bad-debt policy are decided: the whole 1% goes to the
      liquidator and there is no write-off (`docs/plan-lend.md`, "Rules"). Changing either changes a circuit.
- [x] P-1 from `docs/audit-2026-11-25.md` is fixed before the freeze: a partial repay worth less
      than one debt share is refused by the repay circuit (regenerated repay keys and checksums),
      by the Rust and TypeScript math and by the keeper before it builds.
- [ ] Tag the commit. Every later step builds from this tag with a clean tree.

## 2. Audit

- [ ] The audit covers the tagged commit: `lend/`, `core/`, `cli/`, `worker/`, `web/` **[operator]**.
- [ ] Every finding is fixed or accepted in writing, then tag again. A fix to a circuit goes back to
      step 1.

## 3. Key ceremony

The committed verifying keys are test keys (`zolana zk compile --test`, a public seed): anyone can
forge a proof for them. A mainnet build refuses them (they `compile_error!` without
`insecure-test-setup`), and `scripts/deploy-mainnet.sh` stops here until this step is done.

- [ ] Run a Xark phase-2 ceremony for the 11 lend circuits (and the 3 swap circuits if the swap
      launches) with at least one contributor from outside the operator **[operator]**.
- [ ] Commit its verifying keys: lend's into `lend/program/src/verifier/<circuit>.vk.rs` (generated
      with `insecure_test_setup` false), the swap's into `ceremony/swap/{make,take,cancel}.vk.rs`.
- [ ] Keep the manifest: one `<sha256>  <circuit>/pk.bin` line per circuit, the format
      `zolana vks check --expect` reads (like `lend/lend-keys.CHECKSUM`). It is passed to every
      deploy command as `--manifest`.
- [ ] Copy the proving keys to the deploy Mac and each keeper host (the directory
      `KEELER_LEND_KEYS_DIR` names) and the three browser keys (`intent`, `cancel_intent`, `close`)
      into the site's package; check each copy against the manifest (`shasum -a 256 -c <manifest>`
      in the keys directory).
- [ ] If the swap launches: install the ceremony's swap browser artifacts in the site package
      before the mainnet site build. For `make` and `cancel`, copy `circuit.xbc`, `pk.bin` and
      `vk.bin` from the ceremony's build into `web/public/swap/<circuit>/`, and the
      `proving-keys.json` of that build (the pk hashes the prover checks) into `web/public/swap/`,
      replacing the test ones `scripts/setup.sh` put there; check the `pk.bin` files against the
      swap manifest. Without them a mainnet page proves swaps with the test keys, which the
      program refuses.
- [ ] `scripts/setup-lend.sh` and `lend/lend-keys.CHECKSUM` still build and check test keys; they
      need a ceremony mode before the site is built for mainnet.

## 4. Keys and the multisig

Every key is a Keychain item (a Solana keypair JSON array, or a secret string); create each with
`cargo run --release -p keeler --example deploy -- keygen` piped into `security`, as
`scripts/deploy-devnet.sh` does **[operator]**. The names below are the ones the configs and commands
in this runbook use.

| Item | What | Deploy Mac | Keeper host 1 | Keeper host 2 |
|---|---|---|---|---|
| `keeler-mainnet-deploy` | Pays the deploy, first upgrade authority, the lend admin (co-signs the reserve's creation, nothing else) | yes | no | no |
| `keeler-mainnet-lend-program`, `keeler-mainnet-lend-buffer` (`-swap-` for the swap) | The program and buffer keys | yes | no | no |
| `keeler-helius-api-key` | The Helius key (or a Helius URL with `api-key`) | yes | yes | yes |
| `keeler-mainnet-keeper-1` | Keeper 1's key: in the compiled `KEEPERS`, pays the reserve's creation | for step 5 only | yes | no |
| `keeler-mainnet-keeper-2` | Keeper 2's key | no | no | yes |
| `keeler-mainnet-pool` | The pool key: no keeper's key, no deploy key. Whoever holds it reads every amount | for step 5 only | yes | yes |
| `keeler-mainnet-keeper-http` | The shared `x-keeler-key` (the Worker's `DEALER_KEY`), at least 16 characters | for step 7 | yes | yes |
| `keeler-price-rpc-url` | The Worker's `PRICE_RPC_URL`: a second provider's RPC URL (with its key), not Helius | for step 7 | no | no |

- [ ] A keeper key per host (2 hosts, at most 4 keepers). No keeper key is the deploy key.
- [ ] One pool key, made once, copied into the keeper hosts' Keychains and, for the deploy only,
      the deploy Mac's. Its viewing public key (66 hex starting `04`) is
      `keeler deploy viewing-key keeler-mainnet-pool` on a machine that holds it.
- [ ] The reserve's creation (step 5) signs with keeper 1's key and the admin key, and derives the
      pool viewing key from the pool key, all on the deploy Mac. Copy `keeler-mainnet-keeper-1` and
      `keeler-mainnet-pool` there before step 5 and delete both from it after step 5
      (`security delete-generic-password -s <item>`).

The multisig **[operator]**:

- [ ] Create a Squads v4 multisig with threshold at least 2, signers on separate devices, and no
      config authority (autonomous: changes to members or threshold need a proposal). The deploy
      refuses anything else: it reads the account from chain.
- [ ] Note its address (the multisig account, not the vault). The upgrade authority becomes its
      vault 0 (`--vault-index` to pick another).
- [ ] Rehearse on devnet (Squads v4 has the same address there): hand a devnet program to a devnet
      multisig with `scripts/set-upgrade-authority.sh --squads <multisig> 0 <program> <plan>` and
      run an upgrade through it. (Without `--squads` the script refuses a mainnet plan.)

## 5. Deploy

On the deploy Mac, from the audited tag with a clean tree (`.tools` from `scripts/setup.sh`).

- [ ] Cost: `scripts/deploy-mainnet.sh lend --estimate <parameters>` (the command below without
      `--manifest`, `--keeper-config` and `--keys-dir`; `--multisig` optional). It builds with
      the test keys (same size), checks the genesis hash and, with `--multisig`, the multisig, and
      prints the cost. On 2026-11-25 the lend build was 148,904 bytes: 1.516264800 SOL at peak, of
      which 0.757270520 SOL of buffer rent comes back; about 0.76 SOL stays spent. The swap:
      0.936017000 SOL at peak. The program data is exactly the `.so`'s size; a larger upgrade
      first extends it (section 11), at the rent of the added bytes then (6,960 lamports a byte
      today: 0.0713 SOL per 10 KiB).
- [ ] Fund `keeler-mainnet-deploy` with the peak **[operator]**, and keeper 1's address
      (`keeler-mainnet-keeper-1`) with at least 0.05 SOL for the reserve's creation.
- [ ] The keeper config for the reserve, `keeper-1.json`, as in section 6 with
      `"admin_key": {"keychain": "keeler-mainnet-deploy"}` added to its `lend` section.
- [ ] Deploy **[operator]**: types the program ID, then the multisig address, to confirm.

```sh
scripts/deploy-mainnet.sh lend \
  --program-id <keeler-mainnet-lend-program's address> \
  --payer <keeler-mainnet-deploy's address> --admin <the same> \
  --keepers <keeper 1>,<keeper 2> --pool-viewing <66 hex> \
  --collateral-mint 11111111111111111111111111111111 \
  --debt-mint EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v \
  --price-account 7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE \
  --feed-id ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d \
  --multisig <Squads multisig> --manifest <ceremony manifest> \
  --keeper-config keeper-1.json --keys-dir <ceremony proving keys>
```

It refuses a dirty tree, another cluster, a missing parameter, a vault other than the SOL/USDC one
of the table above (the four values are fixed in the script), a committed `<circuit>.vk.rs` that
does not name the manifest's proving key for that circuit (or a circuit the manifest does not
list), a test or unlisted verifying key, the test price, and an authority that is not an
autonomous Squads multisig with threshold at least 2, at least two members who can initiate and
two who can execute, and a time lock of at most two days (`--max-time-lock-secs` to allow
longer).
Then: it builds its own `keeler` with the eight build parameters above (`--locked`) and, before
anything is sent, checks everything the reserve's creation needs (`keeler deploy reserve check`:
the keeper config loads, `lend.network` is `mainnet`, the keeper, pool and admin keys come from
the Keychain, keeper 1 is in the compiled set, the pool key gives the compiled viewing key and is
no keeper's, the admin key is the compiled admin, the RPC is mainnet's, the compiled price account
is owned by Pyth's receiver and holds a fully verified update for the compiled feed id, keeper 1
holds 0.05 SOL).
Then build, cost, deploy, the marker checks again on the deployed bytes, the upgrade authority to
the vault (read back), and the reserve (`keeler deploy reserve create`, the keeper paying, the
deploy key co-signing; "reserve exists" when an earlier run made it). A rerun after a failure at
any step checks what is done (the deployed bytes, the authority, the reserve) and does only the
rest. `target/mainnet-lend/` keeps `build.json` (commit, tag and `.so` sha256),
`deployment.json` and `deploy.log`. It creates no token, runs no faucet and moves no funds.

- [ ] Check `build.json`'s `tag` (also in the log's first line) is the audited tag and `commit`
      is that tag's commit (`git rev-list -n 1 <tag>`) **[operator]**.
- [ ] Check it independently: `solana program show <program id>` names the vault as authority;
      `solana program dump <program id> /tmp/p.so` and
      `scripts/deploy-mainnet.sh lend --check-so /tmp/p.so --manifest <manifest>` pass.
- [ ] Delete `keeler-mainnet-keeper-1` and `keeler-mainnet-pool` from the deploy Mac.

## 6. Keeper hosts

Two always-on hosts **[operator]**, each with its own keeper key and the same pool key, both in the
compiled `KEEPERS`. A mainnet keeper runs on its own (`keeler keeper`), not inside a dealer and
not with `scripts/run-dealer.sh` (a devnet tool: quick tunnels, devnet deployments only).

On each host, with the Keychain items of section 4, the ceremony's proving keys checked against
the manifest, and a checkout of the audited tag with `.tools` (`scripts/setup.sh`):

- [ ] Build the keeper with the program's eight build parameters, exactly the deploy's:

```sh
KEELER_LEND_PROGRAM_ID=<program id> KEELER_LEND_ADMIN=<keeler-mainnet-deploy's address> \
KEELER_LEND_KEEPERS=<keeper 1>,<keeper 2> KEELER_LEND_POOL_VIEWING=<66 hex> \
KEELER_LEND_VAULT1_COLLATERAL_MINT=11111111111111111111111111111111 \
KEELER_LEND_VAULT1_DEBT_MINT=EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v \
KEELER_LEND_VAULT1_PRICE_ACCOUNT=7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE \
KEELER_LEND_VAULT1_FEED_ID=ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d \
CARGO_TARGET_DIR=target/mainnet-keeper \
  cargo build --release --locked -p keeler --bin keeler
```

- [ ] The keeper config, `keeper-<n>.json` (fields in `docs/lend-keeper.md`):

```json
{
  "network": {
    "rpc_url": "https://mainnet.helius-rpc.com",
    "indexer_url": "https://mainnet.helius-rpc.com/v1/spr",
    "prover": { "helius": { "keychain": "keeler-helius-api-key" } },
    "priority_fee": 0
  },
  "key": { "keychain": "keeler-mainnet-keeper-<n>" },
  "http": { "listen": "127.0.0.1:8789", "key": { "keychain": "keeler-mainnet-keeper-http" } },
  "lend": {
    "program_id": "<program id>",
    "network": "mainnet",
    "pool_key": { "keychain": "keeler-mainnet-pool" },
    "role": "primary",
    "origins": ["https://<the site's domain>"],
    "pause_file": "/var/lib/keeler/lend.pause",
    "price": "pyth",
    "limits": { "...": "every max_* non-zero, below" }
  }
}
```

  - Keys from the Keychain only (a file is refused on mainnet); no `faucet` section;
    `lend.price: "pyth"`; the genesis hash is checked at start. The keeper refuses to start unless
    its key is in the compiled set, the pool key's viewing key is the compiled one and the pool key
    is no keeper's.
  - `lend.role`: `primary` on host 1, `standby` on host 2 (it liquidates only after
    `standby_grace_secs`, default 45).
  - `lend.origins`: the site's origin only.
- [ ] Caps (`lend.limits`, every one non-zero or the keeper refuses to start on mainnet), the same
      on both hosts. A canary: `max_pool_size`, `max_total_collateral`,
      `max_position_collateral`, `max_position_debt`, `max_supply_per_wallet`, small enough that
      losing all of it is acceptable **[operator]**.
- [ ] `min_position_debt`: a liquidation pays the liquidator 1% of the debt it repays, which must
      cover its cost: the base fees of its two transactions (a split to a note of exactly `x`, then
      the liquidation), the priority fee and the proofs' Helius credits. Set it so 1% of it is at
      least three times that cost. Example: costs of 0.0003 SOL at 200 USD are 0.06 USD, so
      `min_position_debt` of at least 18 USDC (18,000,000 base units).
- [ ] Priority fees: `network.priority_fee` (micro-lamports per compute unit) on every keeper
      transaction, `lend.liquidation_priority_fee` higher. Pick from recent fees on the program's
      accounts (Helius `getPriorityFeeEstimate` or `getRecentPrioritizationFees`) and recheck when
      transactions land slowly.
- [ ] Fund **every** keeper **[operator]**: each liquidates from notes at its own lend address, so a
      standby with no USDC cannot liquidate when the primary is down.
  - SOL for fees: a plain transfer to the keeper's address.
  - USDC for liquidations, at least the largest position's debt, on each keeper. With the
    keeper's binary and `KEELER_LEND_KEYS_DIR=<ceremony proving keys>` set, as in the run command
    below: `keeler lend-fund keeper-<n>.json <base units>` shields USDC from the keeper's token
    account to its lend address; `keeler lend-balance keeper-<n>.json` shows what it holds.
  - Collateral that liquidations paid builds up at each keeper's lend address:
    `keeler lend-sweep keeper-<n>.json` unshields it to the keeper's own account, to sell or keep.
- [ ] Run it under a supervisor that restarts it (launchd), with the proving keys:

```sh
KEELER_LEND_KEYS_DIR=<ceremony proving keys> target/mainnet-keeper/release/keeler keeper keeper-<n>.json
```

- [ ] Put an HTTPS front on each host in front of `http.listen` (loopback only): a named Cloudflare
      Tunnel on the operator's domain, or a reverse proxy with a certificate, never a quick tunnel.
      Its `https://` URL is the keeper's entry in `LEND_KEEPERS`. The Worker refuses a plain
      `http://` keeper off a local network (`DEALER_KEY` goes with every request).
- [ ] Start the primary, then the standby; `GET /health` answers on both (with `x-keeler-key`).
- [ ] One process per key: the keeper's and the dealer's lock files stop a second process on the
      same state file on one host, not on a second host.

## 7. Worker and site

- [ ] Worker: `NETWORK=mainnet` (the Worker refuses any value but `local`, `devnet`, `mainnet`),
      `LEND_PROGRAM_ID`, `LEND_KEEPERS` (`[{"url","pubkey"}]`, `https://` URLs, primary first; the
      Worker asks each in order, up to four, when one gives no keeper reply or is still starting),
      `LEND_POOLS` and `LEND_VAULTS` exactly as compiled, `ASSETS`, `PYTH_ACCOUNTS` with both the
      SOL and the USDC account of the table above, the rate limits (`worker/README.md`). Only after
      the upgrade authority is the vault.
- [ ] `PRICE_RPC_URL`: an `https://` RPC of a second provider, not a Helius host (the Worker
      refuses one, and without it `/api/prices` answers 500 on mainnet). The dealer always reads
      prices through Helius, so the page's reference price must come from another provider: one
      provider could otherwise show both the same false price. A secret when its URL carries a key.
- [ ] The secrets go through a pipe, in `worker/`:

```sh
security find-generic-password -s keeler-mainnet-keeper-http -w | npx wrangler secret put DEALER_KEY
security find-generic-password -s keeler-helius-api-key -w | npx wrangler secret put HELIUS_API_KEY
security find-generic-password -s keeler-price-rpc-url -w | npx wrangler secret put PRICE_RPC_URL
```

  (`HELIUS_API_KEY` is the key itself: if the Keychain item holds a URL, pipe its `api-key`
  value.)
- [ ] Site: `VITE_NETWORK=mainnet`, `VITE_LEND_PROGRAM_ID`, `VITE_LEND_KEEPERS` (a JSON list: the
      program's whole keeper set; the Worker lists the ones it runs), `VITE_LEND_POOL_VIEWING`,
      `VITE_DEALERS`; the ceremony's browser keys; and the assets with their decimals, compiled in
      (a mainnet build refuses to start without them):
      `VITE_ASSETS='[{"mint":"11111111111111111111111111111111","symbol":"SOL","decimals":9},{"mint":"EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v","symbol":"USDC","decimals":6}]'`.
      SOL is the all-ones address, as the pool and the Worker see it; the mints and both symbols
      must equal the Worker's `ASSETS` exactly, or the page refuses its config. Lending stays
      hidden unless `/api/config` matches the compiled keepers. A mainnet build names no test token and has no faucet or
      browser test wallet (`web/tests/build.test.ts`).
- [ ] The custom domain **[operator]**.
- [ ] Seed the pool with a first supply, then a small deposit, borrow, repay and close from a
      second wallet **[operator]** (real funds).

## 8. Kill switches

Fastest first. Only stopping the keepers stops repayments; no switch stops refunds of expired
intents or closing a debt-free position.

| Switch | Where | Stops | Never stops |
|---|---|---|---|
| `LEND_PAUSED=1` | Worker variable | new deposits into Earn (refused at the Worker), opening a position and borrowing (the Worker marks every other request `x-keeler-paused`, and the keepers refuse those) | repay, top-up of an open position, withdrawal from Earn, withdrawal of collateral, account reads. Tickets issued before the switch still fill: to stop those too, touch the pause files |
| `lend.pause_file` | each keeper host: `touch` the file | new supply tickets and fills, deposits that open a position, borrowing. Touch it on every keeper | the same as above |
| `PAUSED=1` | Worker variable | the swap's quotes and faucet only, not lending | lending |
| `pause_file` | the dealer | new swap quotes; bound orders still settle | lending |
| Stop the keepers | hosts | everything the keeper fills, including repayments and liquidations: last resort | refunds after expiry, closing a debt-free position |
| Program upgrade | the multisig | anything, after a proposal reaches the threshold | |

## 9. When the Pyth account stalls

Draws and liquidations need the push account at most 90 s old (the keeper: 65 s for a draw, 80 s
for a liquidation). Pyth sponsors updates about once a minute; if they stop, post them yourself with
Pyth's price pusher, which writes the same shard-0 account. Sources: the pusher's README
(`@pythnetwork/price-pusher` 11.3.0, https://cdn.jsdelivr.net/npm/@pythnetwork/price-pusher@11.3.0/README.md)
and Pyth's push-feeds page (https://docs.pyth.network/price-feeds/core/push-feeds), read
2026-11-25. Pyth's page says the pusher needs a Pyth Pro API key passed with
`--hermes-access-token`, version 10.5.0 or later **[operator]**: the key in the Keychain item
`keeler-pyth-pro-key`, and a small hot wallet `keeler-mainnet-pyth-pusher` with SOL for fees.

`price-config.yaml`:

```yaml
- id: ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d
  time_difference: 30
  price_deviation: 0.5
  confidence_ratio: 1
```

Run (from a checkout of `pyth-network/pyth-crosschain`, `apps/price_pusher`, or the image
`public.ecr.aws/pyth-network/xc-price-pusher:v<version>`; there is no `latest` tag). The RPC URL
(with the Helius key) and the Pyth Pro key are read from the Keychain into environment variables
and handed over in a `--config` file that is a pipe, so neither is on the command line nor on disk;
the keypair comes through a pipe too. The README says the pusher takes "a config file instead of
command line options"; check `pnpm run start solana --help` lists `--config` for the version you
run before relying on it:

```sh
(
  export HELIUS=$(security find-generic-password -s keeler-helius-api-key -w)
  export PYTH_PRO=$(security find-generic-password -s keeler-pyth-pro-key -w)
  pnpm run start solana \
    --config <(python3 -c 'import json, os
h = os.environ["HELIUS"]
url = h if h.startswith("https://") else "https://mainnet.helius-rpc.com/?api-key=" + h
print(json.dumps({"endpoint": url, "hermes-access-token": os.environ["PYTH_PRO"]}))') \
    --keypair-file <(security find-generic-password -s keeler-mainnet-pyth-pusher -w) \
    --shard-id 0 \
    --price-config-file ./price-config.yaml \
    --price-service-endpoint https://hermes.pyth.network/ \
    --pyth-contract-address pythWSnswVUd12oZpeFP8e9CVaEqJg25g1Vtc2biRsT \
    --pushing-frequency 30
)
```

- [ ] Check the account's publish time moves (the keeper's log stops refusing draws).
- [ ] Stop it when Pyth's own updates resume. Anyone may post a verified update newer than the
      stored one; the program checks it is fully verified, for the pinned feed and fresh.

## 10. When a keeper dies

- [ ] The Worker sends lend requests to the next keeper when one gives no keeper reply (or is
      still starting); the standby issues tickets and builds, fills intents made through the dead
      keeper (the page sends its open tickets, also after a reload), and liquidates after its
      grace, from its own USDC.
- [ ] Bring the host back with the same keeper key; it replays the chain history of every keeper
      and merges.
- [ ] If the host is lost or its keys may be exposed: replace it (below). Its pool key stays
      exposed: it can still read every amount.

## 11. Upgrades: adding or replacing a keeper

The keeper set and the pool key are compiled in; changing them is a program upgrade. There is no
key rotation: a removed keeper that kept the pool key still reads every amount, past and future.

- [ ] New host and keeper key **[operator]**; copy the pool key into its Keychain **[operator]**.
- [ ] Build the upgrade from a clean tree with the new `--keepers` (same everything else):
      `scripts/deploy-mainnet.sh lend --build-only <parameters> --manifest <manifest>`.
- [ ] If the new `.so` is larger than the program data (`solana program show <program id>`, "Data
      Length"), extend the program data first:
      `target/mainnet-lend/cargo-tool/release/keeler deploy extend target/mainnet-lend/plan.json <program id> <bytes>`,
      the deploy key paying the added rent (`<bytes>` at least 10,240 and at least the difference;
      round up for later upgrades). It reads the length back. Loader v3's `ExtendProgram` needs no
      upgrade-authority signature, only a payer, so it does not go through the multisig. Sources:
      the `ExtendProgram` documentation in `solana-loader-v3-interface` 9.0.0
      (`src/instruction.rs`: the accounts are the program data, the program, the system program
      and a payer; after the SIMD-0431 feature gate `additional_bytes` must be at least 10 KiB),
      and `agave-feature-set` 4.4.0, where the feature for the authority-checked variant
      (SIMD-0164, `ExtendProgramChecked`) is `ExtendProgCheckedWi11BeDe1eted11111111111111` and
      the instruction is gone from the interface since 7.0.0; read 2026-11-25 from the crate
      sources. Recheck both before an upgrade.
- [ ] Write it to a buffer (`solana program write-buffer target/mainnet-lend/deploy/keeler_lend.so`),
      hand the buffer's authority to the vault (`solana program set-buffer-authority <buffer>
      --new-buffer-authority <vault>`), and propose the upgrade in Squads; the signers check the
      buffer's bytes against `build.json`'s sha256 first **[operator]**.
- [ ] If the upgrade changes vault 1's parameters, change `TSOL_TUSD_VAULT` in
      `web/src/keeler/lend-math.ts` with them, or the page refuses the Worker's config.
- [ ] After the upgrade: rebuild every keeper (section 6) with the new `KEELER_LEND_KEEPERS`,
      rebuild the site with the new `VITE_LEND_KEEPERS`, set the Worker's `LEND_KEEPERS`, fund and
      start the new keeper. It replays the history and takes the reserve's notes from it.
- [ ] Stop the removed keeper; its signatures are refused from the upgrade on.

## 12. Raising caps

- [ ] Decide the new caps **[operator]**; keep `min_position_debt` covering a liquidation's cost.
- [ ] Set the same `lend.limits` on every keeper; restart the standby, check its health, then the
      primary. A cap is the keeper's check, not the program's.

## Trust statement

The pool keeps working when any one keeper is lost, and the multisig can add a keeper and hand it
the pool key. A keeper can still halt the pool by withholding or corrupting what it writes (the
ciphertext is unchecked until the custom ring), so the set is hosts the operator controls. With
every pool-key holder gone, Earn deposits and indebted positions cannot move; intents refund and
debt-free positions close without a keeper.
