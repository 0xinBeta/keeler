# Mainnet runbook

The whole launch, in order. Nothing here has been run on mainnet. **[owner]** marks a step that
needs the owner: funds, keys, signers, hosts or the domain. Do the steps in order; each list is a
checklist.

Mainnet values used below:

| What | Value |
|---|---|
| Genesis hash | `5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d` |
| Collateral (SOL, as the pool sees it) | `11111111111111111111111111111111` |
| Debt (USDC) | `EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v` |
| SOL/USD Pyth push account (shard 0) | `7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE` |
| SOL/USD feed id | `ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d` |
| Squads v4 program | `SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf` |

The debt token is valued at exactly 1 USD. If USDC trades below 1, liquidations come early, never
late.

## 1. Freeze

- [ ] Every circuit, the program, the keeper and the page are final. A circuit change after the
      ceremony means a new ceremony.
- [ ] The open questions in `docs/plan-mainnet.md` ("Open for the owner") are decided **[owner]**.
- [ ] Tag the commit. Every later step builds from this tag with a clean tree.

## 2. Audit

- [ ] The audit covers the tagged commit: `lend/`, `core/`, `cli/`, `worker/`, `web/` **[owner]**.
- [ ] Every finding is fixed or accepted in writing, then tag again. A fix to a circuit goes back to
      step 1.

## 3. Key ceremony

The committed verifying keys are test keys (`zolana zk compile --test`, a public seed): anyone can
forge a proof for them. A mainnet build refuses them (they `compile_error!` without
`insecure-test-setup`), and `scripts/deploy-mainnet.sh` stops here until this step is done.

- [ ] Run a Xark phase-2 ceremony for the 11 lend circuits (and the 3 swap circuits if the swap
      launches) with at least one contributor from outside the operator **[owner]**.
- [ ] Commit its verifying keys: lend's into `lend/program/src/verifier/<circuit>.vk.rs` (generated
      with `insecure_test_setup` false), the swap's into `ceremony/swap/{make,take,cancel}.vk.rs`.
- [ ] Keep the manifest: one `<sha256>  <circuit>/pk.bin` line per circuit, the format
      `zolana vks check --expect` reads (like `lend/lend-keys.CHECKSUM`). It is passed to every
      deploy command as `--manifest`.
- [ ] Copy the proving keys to each keeper host (`KEELER_LEND_KEYS_DIR`) and the three browser keys
      (`intent`, `cancel_intent`, `close`) into the site's package; check both against the manifest
      (`shasum -a 256 -c <manifest>` in the keys directory).
- [ ] `scripts/setup-lend.sh` and `lend/lend-keys.CHECKSUM` still build and check test keys; they
      need a ceremony mode before the site is built for mainnet.

## 4. Keys and the multisig

Keychain items on the deploy Mac (Solana keypair JSON arrays; create each with
`cargo run --release -p keeler --example deploy -- keygen` piped into `security`, as
`scripts/deploy-devnet.sh` does) **[owner]**:

- [ ] `keeler-mainnet-deploy`: pays the deploy, is the first upgrade authority, and is the lend
      admin (it co-signs the reserve's creation, and nothing else).
- [ ] `keeler-mainnet-lend-program`, `keeler-mainnet-lend-buffer` (and `-swap-` for the swap).
- [ ] `keeler-helius-api-key`: the Helius key.

Keeper keys, each made on and kept in the Keychain of its own host **[owner]**:

- [ ] A keeper key per host (2 hosts, at most 4 keepers). No keeper key is the deploy key.
- [ ] One pool key, made once, copied into both keeper hosts' Keychains and nowhere else. It is no
      keeper's key. Whoever holds it reads every amount in the pool. Its viewing public key (66 hex)
      is `keeler deploy viewing-key <keychain item>` on a host that holds it.

The multisig **[owner]**:

- [ ] Create a Squads v4 multisig with threshold at least 2, signers on separate devices, and no
      config authority (autonomous: changes to members or threshold need a proposal). The deploy
      refuses anything else: it reads the account from chain.
- [ ] Note its address (the multisig account, not the vault). The upgrade authority becomes its
      vault 0 (`--vault-index` to pick another).
- [ ] Rehearse on devnet (Squads v4 has the same address there): hand a devnet program to a devnet
      multisig with `scripts/set-upgrade-authority.sh --squads <multisig> 0 <program> <plan>` and
      run an upgrade through it.

## 5. Deploy

- [ ] Cost: `scripts/deploy-mainnet.sh lend --estimate <parameters>` (below, without
      `--keeper-config` and `--keys-dir`). It builds with the test keys (same size), checks the
      genesis hash and, with `--multisig`, the multisig, and prints the cost. On 2026-11-25 the lend
      build was 148,904 bytes: 1.516264800 SOL at peak, of which 0.757270520 SOL of buffer rent
      comes back; about 0.76 SOL stays spent. The swap: 0.936017000 SOL at peak.
- [ ] Fund `keeler-mainnet-deploy` with the peak **[owner]**.
- [ ] Deploy **[owner]**: types the program ID, then the multisig address, to confirm.

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
  --keeper-config <keeper 1's config> --keys-dir <ceremony proving keys>
```

It refuses a dirty tree, another cluster, a missing parameter, a test or unlisted verifying key, the
test price, and an authority that is not an autonomous Squads multisig with threshold at least 2.
Then: build, cost, deploy, the marker checks again on the deployed bytes, the upgrade authority to
the vault (read back), and the reserve (`keeler lend-init-reserve`, the keeper paying, the deploy key
co-signing). `target/mainnet-lend/` keeps `build.json` (commit and `.so` sha256),
`deployment.json` and `deploy.log`. It creates no token, runs no faucet and moves no funds.

- [ ] Check it independently: `solana program show <program id>` names the vault as authority;
      `solana program dump <program id> /tmp/p.so` and
      `scripts/deploy-mainnet.sh lend --check-so /tmp/p.so --manifest <manifest>` pass.

## 6. Keeper hosts

Two always-on hosts **[owner]**, each with its own keeper key and the same pool key, both in the
compiled `KEEPERS`. Configs as in `docs/lend-keeper.md`, with these values on mainnet:

- [ ] `network.rpc_url` and `indexer_url` on `mainnet.helius-rpc.com`; the dealer and keeper check
      the genesis hash at start. Keys from the Keychain only (a file is refused on mainnet); no
      `faucet` section; `lend.price: "pyth"`.
- [ ] `lend.key` the host's keeper key, `lend.pool_key` the pool key. A keeper refuses to start
      unless the pool key's viewing key is the compiled one and the pool key is no keeper's.
- [ ] `lend.role`: `primary` on one host, `standby` on the other (it liquidates only after
      `standby_grace_secs`, default 45).
- [ ] `lend.origins`: the site's origin only.
- [ ] Caps (`lend.limits`, every one non-zero or the keeper refuses to start on mainnet), the same
      on both hosts. A canary: `max_pool_size`, `max_total_collateral`,
      `max_position_collateral`, `max_position_debt`, `max_supply_per_wallet`, small enough that
      losing all of it is acceptable **[owner]**.
- [ ] `min_position_debt`: a liquidation pays the liquidator 1% of the debt it repays, which must
      cover its cost: the base fees of its two transactions (a split to a note of exactly `x`, then
      the liquidation), the priority fee and the proofs' Helius credits. Set it so 1% of it is at
      least three times that cost. Example: costs of 0.0003 SOL at 200 USD are 0.06 USD, so
      `min_position_debt` of at least 18 USDC (18,000,000 base units).
- [ ] Priority fees: `network.priority_fee` (micro-lamports per compute unit) on every keeper and
      dealer transaction, `lend.liquidation_priority_fee` higher. Pick from recent fees on the
      program's accounts (Helius `getPriorityFeeEstimate` or `getRecentPrioritizationFees`) and
      recheck when transactions land slowly.
- [ ] Fund each keeper with SOL for fees, and the primary with USDC for liquidations (at least the
      largest position's debt, shielded) **[owner]**.
- [ ] Start the primary, then the standby (`scripts/run-dealer.sh`); `GET /health` answers on
      both.

## 7. Worker and site

- [ ] Worker: `NETWORK=mainnet`, `LEND_PROGRAM_ID`, `LEND_KEEPERS` (`[{"url","pubkey"}]`, primary
      first), `LEND_POOLS` and `LEND_VAULTS` exactly as compiled, `PYTH_ACCOUNTS` for the page's
      prices, the rate limits (`worker/README.md`). Only after the upgrade authority is the vault.
- [ ] Site: `VITE_NETWORK=mainnet`, `VITE_LEND_PROGRAM_ID`, `VITE_LEND_KEEPERS` (a JSON list, the
      same keepers as the Worker's), `VITE_LEND_POOL_VIEWING`, `VITE_DEALERS`; the ceremony's
      browser keys. Lending stays hidden unless `/api/config` matches the compiled keepers.
- [ ] The custom domain **[owner]**.
- [ ] Seed the pool with a first supply, then a small deposit, borrow, repay and close from a
      second wallet **[owner]** (real funds).

## 8. Kill switches

Fastest first. None of them stops repayments, adding collateral, refunds or closing a debt-free
position.

| Switch | Where | Stops |
|---|---|---|
| `LEND_PAUSED=1` | Worker variable | new supply tickets and every keeper-built transaction (borrowing); account reads, repay and deposit-add tickets still pass, marked paused so no new position opens |
| `lend.pause_file` | each keeper host: `touch` the file | new supply, deposits that open a position, borrowing. Touch it on every keeper |
| `PAUSED=1` | Worker variable | the swap's quotes and faucet only, not lending |
| `pause_file` | the dealer | new swap quotes; bound orders still settle |
| Stop the keepers | hosts | everything the keeper fills, including liquidations: last resort |
| Program upgrade | the multisig | anything, after a proposal reaches the threshold |

## 9. When the Pyth account stalls

Draws and liquidations need the push account at most 90 s old (the keeper: 65 s for a draw, 80 s
for a liquidation). Pyth sponsors updates about once a minute; if they stop, post them yourself with
Pyth's price pusher, which writes the same shard-0 account. Sources: the pusher's README
(`@pythnetwork/price-pusher` 11.3.0, https://cdn.jsdelivr.net/npm/@pythnetwork/price-pusher@11.3.0/README.md)
and Pyth's push-feeds page (https://docs.pyth.network/price-feeds/core/push-feeds), read
2026-11-25. Pyth's page says the pusher needs a Pyth Pro API key passed with
`--hermes-access-token`, version 10.5.0 or later **[owner]**: the key, and a small hot wallet
`keeler-mainnet-pyth-pusher` with SOL for fees.

`price-config.yaml`:

```yaml
- id: ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d
  time_difference: 30
  price_deviation: 0.5
  confidence_ratio: 1
```

Run (from a checkout of `pyth-network/pyth-crosschain`, `apps/price_pusher`, or the image
`public.ecr.aws/pyth-network/xc-price-pusher:v<version>`; there is no `latest` tag). The keypair is
read through a pipe from the Keychain, never written to disk:

```sh
pnpm run start solana \
  --endpoint "<mainnet RPC URL>" \
  --keypair-file <(security find-generic-password -s keeler-mainnet-pyth-pusher -w) \
  --shard-id 0 \
  --price-config-file ./price-config.yaml \
  --price-service-endpoint https://hermes.pyth.network/ \
  --hermes-access-token "<Pyth Pro key>" \
  --pyth-contract-address pythWSnswVUd12oZpeFP8e9CVaEqJg25g1Vtc2biRsT \
  --pushing-frequency 30
```

- [ ] Check the account's publish time moves (the keeper's log stops refusing draws).
- [ ] Stop it when Pyth's own updates resume. Anyone may post a verified update newer than the
      stored one; the program checks it is fully verified, for the pinned feed and fresh.

## 10. When a keeper dies

- [ ] The Worker sends lend requests to the next keeper when the first gives no reply; the standby
      issues tickets and builds, fills intents made through the dead keeper (the page sends its
      open tickets), and liquidates after its grace.
- [ ] Bring the host back with the same keeper key; it replays the chain history of every keeper
      and merges.
- [ ] If the host is lost or its keys may be exposed: replace it (below). Its pool key stays
      exposed: it can still read every amount.

## 11. Adding or replacing a keeper

The keeper set and the pool key are compiled in; changing them is a program upgrade. There is no
key rotation: a removed keeper that kept the pool key still reads every amount, past and future.

- [ ] New host and keeper key **[owner]**; copy the pool key into its Keychain **[owner]**.
- [ ] Build the upgrade from a clean tree with the new `--keepers` (same everything else):
      `scripts/deploy-mainnet.sh lend --build-only <parameters> --manifest <manifest>`.
- [ ] Write it to a buffer (`solana program write-buffer target/mainnet-lend/deploy/keeler_lend.so`),
      hand the buffer's authority to the vault (`solana program set-buffer-authority <buffer>
      --new-buffer-authority <vault>`), and propose the upgrade in Squads; the signers check the
      buffer's bytes against `build.json`'s sha256 first **[owner]**.
- [ ] After the upgrade: rebuild the site with the new `VITE_LEND_KEEPERS`, set the Worker's
      `LEND_KEEPERS`, start the new keeper. It replays the history and takes the reserve's notes
      from it.
- [ ] Stop the removed keeper; its signatures are refused from the upgrade on.

## 12. Raising caps

- [ ] Decide the new caps **[owner]**; keep `min_position_debt` covering a liquidation's cost.
- [ ] Set the same `lend.limits` on every keeper; restart the standby, check its health, then the
      primary. A cap is the keeper's check, not the program's.

## Trust statement

The pool keeps working when any one keeper is lost, and the multisig can add a keeper and hand it
the pool key. A keeper can still halt the pool by withholding or corrupting what it writes (the
ciphertext is unchecked until the custom ring), so the set is hosts the operator controls. With
every pool-key holder gone, Earn deposits and indebted positions cannot move; intents refund and
debt-free positions close without a keeper.
