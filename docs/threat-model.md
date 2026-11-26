# Threat model

What each party can and cannot do to Keeler Lend, and what stops them. "Enforced by" names the
mechanism:

- **circuit**: a rule of the Xark circuit; holds only while nobody can forge a proof (see
  "Devnet today");
- **program**: a check in the Pinocchio program before or around proof verification
  (`lend/program/src/instructions/`);
- **SPP**: the shielded pool program's own proof and nullifiers;
- **page**: a check the web page makes before the wallet signs (`web/src/keeler/lend-check.ts`);
- **trust**: nothing enforces it; the party is trusted.

Every section describes what the code does today, except the enclave section, which is a proposal
and marks each of its parts **[exists]** or **[proposal]**.

## Devnet today

The deployed program (`CqfpHokrAewSvYhwN6sN8Uazt2UdBootqUj98hSi2ysL`, devnet) is built with the
test proving keys (`zolana zk compile --test`, a fixed public seed) and the `test-price` feature.
Two consequences override most of what follows:

- **Anyone can forge a lend proof.** Every "circuit" rule below is unenforced against a forger. The
  program checks (signers, canonical PDAs, the pinned pool key, `now`, the price account) and the
  shielded pool's own proof still hold. In practice: a borrower who holds a position's opening
  could forge a `close` proof and take the collateral without repaying; a keeper could forge any
  reserve spend. Devnet test tokens only.
- **The keeper sets the price.** The vault's test price account (PDA `["price", vault]`) is
  written by any keeper (`set_test_price`, `require_keeper`). A keeper can therefore make any
  position liquidatable at will. On a `mainnet` build the test price cannot compile
  (`compile_error!` in `lend/program/src/vaults.rs`) and the program reads only the pinned Pyth
  account.

Neither applies to the design as intended for mainnet, which is what the rest of this document
describes. Nothing is deployed on mainnet; `docs/mainnet-runbook.md` lists what has to happen
first (audit, key ceremony, multisig, keeper hosts).

## Assets

1. Users' principal: Earn deposits (in the reserve), collateral (in positions), intents in flight.
2. The privacy of amounts: deposits, collateral, debt, health, liquidation price, pool size and
   utilisation.
3. The pool's solvency: debt owed to suppliers, the accrued-fee buffer.
4. Availability: the ability to withdraw, borrow, repay, be liquidated fairly.
5. Keys: each keeper's signing key, the pool key (its viewing key reads every amount), the admin
   key, the program's upgrade authority, the Worker's `DEALER_KEY` and `HELIUS_API_KEY`.

## Actors

### A user

Can:

- Spend only its own notes: spending needs the note's nullifier secret (SPP).
- Make an intent with any expiry. The expiry has no upper bound in the circuit; a far-future
  expiry locks the maker's own funds until then (`cancel_intent` waits for it). The keeper's ticket
  fixes the expiry and the page uses it; an intent made by hand is the maker's own risk.
- Cancel its intent after expiry and close a debt-free position without the keeper (`cancel_intent`
  and `close`: user-signed, no keeper signature in the program).
- Hold the pool's single lane with owner builds (review K-1). Builds never sent cost a wallet a
  doubling cooldown, so N such wallets hold the lane about N x 160 s an hour; a wallet that sends
  every build just after its reservation ran out is forgiven each time and can hold one
  reservation per build, up to `builds_per_wallet_hour` (`docs/lend-keeper.md`, "The queue";
  trust in the keeper's limits).

Cannot:

- Be credited into someone else's position or receipt, or redirect change: the maker and the
  position are bound in the circuit (`assert_intent`, `position.owner == intent.maker`).
- Withdraw or borrow from another's position: `owner_identity` is computed by the program from the
  signer and compared in the circuit.
- Borrow past the collateral factor at the bound, or past 95% utilisation (circuit, `draw`).
- Choose a favourable price: the program requires the draw bound at or below
  `price − confidence` (program).

### An outsider

Sees the public chain (table in `README.md`). Can call every instruction, but every instruction
that spends a reserve or fills an intent requires a keeper signer (program, `require_keeper`), and
note spends need openings it does not have (SPP). Can post a fully verified, newer Pyth update to
the push account (Pyth's `update_price_feed`), which makes the 90 s age limit a window for choosing
among recent prices (`docs/plan-lend.md`, "Prices"). Whether an older update may replace the one
the account holds is Pyth's push oracle's rule, not the lend program's. The lend program checks
the account's owner, discriminator, verification level and feed id, and that the price was
published within 90 s of the cluster clock.

### A keeper

One of the compiled `KEEPERS` (1 to 4). Holds the pool key's viewing key and its own signing key.

Can:

- **See every amount of every pool user.** It holds the opening of the reserve and of every
  receipt and position. This is the design (`docs/plan-lend.md`, "What follows"), not a leak. It
  is stated in the text every wallet signs at session start ("The keeper can see your lending
  amounts.").
- **Halt the pool.** Offline, nobody can supply, redeem, borrow, repay or be liquidated. By writing
  a reserve ciphertext nobody can open (output ciphertexts are unchecked in the Default Ring) it
  can halt the pool for its peers too. Earn deposits and indebted positions then cannot move;
  intents refund after expiry and debt-free positions close without a keeper (trust).
- Order operations and refuse service to a wallet (trust).
- Write a reserve nobody can open. The program pins the `pool_viewing` argument
  (`require_pool_viewing`), so an honest build encrypts the reserve to the shared key and a
  misconfigured keeper fails. It does not check the ciphertext (trust).
- Choose the liquidation size `x` within the circuit's bounds and choose when to liquidate (see
  below).
- On devnet: forge proofs and set the test price (see "Devnet today").

Cannot (with real keys):

- Take principal directly. Who is paid and how much is fixed by each circuit: intent proceeds go
  to the maker's receipt or position, change to the maker, redeem and draw payouts to the recorded
  owner (circuit). A keeper that withholds liquidation, its own positions' included, can still let
  bad debt grow, and there is no write-off: the loss falls on the suppliers who leave last
  (trust).
- Spend a user's own notes (SPP: needs the user's nullifier secret).
- Liquidate a healthy position at the price it uses: the program requires the liquidation bound
  at or above `price + confidence`, and the circuit requires the debt above the threshold at that
  bound. The keeper may itself post the verified Pyth update it liquidates at, so within the 90 s
  window it chooses among recent prices, as an outsider can.
- Liquidate more than the rules allow: `0 < x <= debt`, at least one debt share removed, payout
  capped at the collateral, and the position not left healthier than the threshold unless it was
  above the max limit (circuit).
- Pay itself more than the 1% penalty on what it repays (circuit, payout formula).
- Slip extra instructions into a transaction the owner signs (page: exact shape, see
  `docs/design-decisions.md`).

### A pool-key holder who is not a keeper

Reads every amount, past and future: there is no key rotation (`docs/mainnet-runbook.md`, section
11). Cannot sign as a keeper, so cannot move anything (program).

### The Worker operator

The Cloudflare Worker (`worker/src/index.ts`) serves the page and proxies RPC, indexer, prover and
keeper requests. In this design it is the keeper's operator.

- Serves the page's code. **The page checks protect users from a keeper, not from whoever serves
  the page**: a different page could skip them. Users trust the operator for the code they run.
- Sees account views in clear: `/lend/account` replies carry the wallet's note openings (review
  W-3, accepted).
- Holds `HELIUS_API_KEY` and `DEALER_KEY` (the keeper HTTP key); never returns them (it refuses an
  upstream reply that contains the API key).
- Can rate-limit, pause (`LEND_PAUSED`) or drop requests.
- Cannot forge a user request to the keeper: requests carry the wallet's session authorisation and
  a session-key signature, checked by the keeper (`docs/lend-api.md`, "Session authorisation").

### The RPC and indexer provider

Sees which wallets and PDAs are read and from where. Could return false data. The page recomputes
every note hash it is given and checks it is appended and unspent; the keeper reads nullifier
accounts from the RPC and outputs from the indexer and checks each against the hash on chain. A
provider that lies about the reference price could move a draw bound within the page's 2% margin
(review W-2: the fix is a second provider). A faulty RPC or indexer stops a keeper's history
replay with an error; the scan mark stays (review K-3).

### The prover

The shielded pool's transact proofs are made by a prover: the local `zolana` prover on a local
network, Helius' attested prover elsewhere, which the page and the keeper reach under the network's
signed TEE policy (`SignedTeePolicy` in `web/src/keeler/client.ts` and `cli/src/chain.rs`; the
Worker relays the policy file and the encrypted requests). The lend program's own proofs are made
by the browser (`intent`, `cancel_intent`, `close`) or by the keeper (the rest), never by the
hosted prover. What the TEE protects is Helius' claim and outside this repository; if it fails,
the prover sees the witnesses of the transact proofs it makes.

### The upgrade authority

Can replace the program, and with it every rule. On devnet it is one deploy key
(`docs/lend-deploy.md`). The mainnet path hands it to a Squads v4 multisig with threshold at least
2 and no config authority, checked on chain before the hand-over (`scripts/deploy-mainnet.sh`).
The keeper set and the pool key change only by an upgrade.

### A liquidated owner

Learns that it was liquidated and, from its own position's new note, how much. `liquidate` seeds
every output's blinding from the position, whose opening the owner holds, so the owner can derive
the blinding of the liquidator's change note. The keeper therefore liquidates from a note of
exactly `x`, made first by a self-transfer, so that change is a zero note
(`docs/lend-keeper.md`, "The queue"; trust in the keeper, not enforced).

### Other pool users

Learn the pool's price indices at their own operations (the keeper returns the index a build
used, and the page checks it only grows within the curve's top rate). Over time they can estimate
utilisation and notice very large operations. `/lend/pools` publishes size, utilisation and rates
rounded to two significant figures, at most once per `bucket_secs`.

### The explorer

On devnet the page links the transactions it reports to their pages on Orb, Helius' explorer
(`orbTx` in `web/src/keeler/network.ts`). A visitor who opens one tells Orb which transaction they
are looking at, from their own network address. Orb's page for a devnet lend transaction of this
program, opened on 2026-11-26, is titled "Private transfer via Helius Privacy" and shows "Amount:
Confidential", the two signer wallets, the program and the fee, and no amount.

## Accepted findings from the internal security review

From `docs/audit-2026-11-25.md` (internal, not an external audit; the compiled constraints and the
proof system were not covered):

| ID | Finding | Why accepted |
| --- | --- | --- |
| K-2 | Two keepers filling deposits at once can pass `max_total_collateral`, because a deposit does not spend the reserve | Only during a failover; the caps are a per-keeper soft limit |
| W-3 | Account views cross the Worker in clear | The Worker is run by the keeper's operator and serves the page |

The other findings were fixed or mitigated; the review document records each one's status.
Also not enforced by any circuit, and documented in `lend/AGENTS.md`: the unbounded intent expiry
and the liquidator change blinding above.

## Running the keeper in an enclave

The aim: let a user check which code holds the keeper and pool keys, instead of trusting the
operator's hosts with them. It narrows what the operator is trusted for; it does not remove the
operator. The plan is Phala dstack, an Intel TDX confidential VM; the AWS Nitro Enclaves design is
kept as the alternative. Helius' prover contract accepts the same two platforms, `dstack-tdx` and
`aws-nitro`, behind one attestation route and one 64-byte binding, and the route below is shaped
after it. References are at the end of the section.

### What exists today [exists]

- The keeper is a `keeler keeper` process on an operator's host. Its keeper key, the pool key and
  the HTTP key are read from a macOS Keychain item (`keychain_secret` in `cli/src/config.rs`) on
  mainnet, which refuses any other source; elsewhere a config may read them from a file or the
  environment instead (`KeySource` in `cli/src/config.rs`); the
  state file is JSON, mode 0600, on that host's disk, with a lock that stops two processes on one
  file (`docs/lend-keeper.md`, "The state file").
- The pool key is made once, on the deploy machine, and copied to each keeper host
  (`docs/mainnet-runbook.md`, section 4). The operator holds every key in clear.
- On devnet that host is a Phala Cloud CVM (Intel TDX) since 2026-11-26: the dealer, which hosts the
  primary keeper, and the standby keeper run as two containers in it, from
  `deploy/phala/docker-compose.yml` and an image pinned by digest (`README.md`, "Where it runs";
  `docs/lend-deploy.md`, "On Phala Cloud"). Their keys are the existing devnet keys, made on the
  operator's machine, read from its Keychain by `scripts/deploy-phala.sh` and passed in as encrypted
  environment values; the key manager is Phala's (`--kms phala`). Nothing checks an attestation of
  this CVM: not the page, not the Worker, not a command in this repository. The operator still knows
  every key, and none of the properties below hold for it. Phala, as the host, can stop the CVM, and
  with it both devnet keepers.
- Phala's documentation describes a public Trust Center report for every app
  (`https://trust.phala.com/app/<app id>`), and also an app attestation route that answers only for
  CVMs that opted into public TCB information. This CVM was deployed with `--no-public-sysinfo`.
  Whether a third party can fetch its quote or measurements without the operator's Phala API key has
  not been established. What a third party can check today: the compose file in this repository, the
  image by its digest (a public package), and the log of the workflow run that built and pushed it,
  which prints that digest. The compose hash cannot be recomputed from this repository: Phala hashes
  its own `app-compose.json`, which wraps the compose file and is not committed here.
- The only attestation this repository checks is Helius' prover's, through the Rings SDK ("The
  prover" above).

### Common to both platforms [proposal]

- A keeper route, `/lend/attestation?nonce=<32 bytes hex>`, answers `platform` (`dstack-tdx` or
  `aws-nitro`), `request_key`, `reply_key`, `evidence` and a keeper signature. The request key
  (X25519; the page encrypts every keeper request to it) and the reply key (Ed25519; it signs
  every keeper reply) are drawn at boot, held in memory only, never derived from a platform key
  and never sealed. They are not pinned: the page attests again when a request fails to decrypt,
  as Helius' clients do after `tee_decryption_failed`.
- So a request can be read only by the instance whose evidence carried the request key. A key
  derived from the dstack app key would be held by every compose approved under the app (Helius'
  contract says so of its own derived key) and by whoever holds the KMS root.
- One 64-byte binding, carried by both platforms:

  ```
  binding = SHA-512("keeler-keeper-attest-v1" ‖ nonce ‖ program id ‖ keeper address ‖
                    pool viewing key ‖ request key ‖ reply key)
  ```

  The fields have fixed lengths: the 23-byte ASCII tag, then 32, 32, 32, 33 (the pool viewing key
  as the program pins it), 32 and 32 bytes, 216 in all. On TDX the binding is the quote's
  `report_data`. On Nitro it is `user_data`, with `nonce` set to the caller's nonce and
  `public_key` to the request key. Caller-chosen bytes never go into either field.
- The keeper key signs `"keeler-keeper-attest-sig-v1" ‖ binding`, domain-prefixed as tickets are
  (`keeler-lend-ticket-v1`, `cli/src/lend_auth.rs`). It shows that the holder of a compiled keeper
  key endorsed this evidence, not that the key exists only inside the enclave.
- The check: the platform's evidence checks (below), the binding recomputed from the caller's
  nonce and the compiled program id, `KEEPERS` and `POOL_VIEWING`, the keeper signature against
  `KEEPERS`, and the measurements against values compiled into the site, as the keeper set and
  pool key are today. Lending stays hidden when it fails, as it does today for an unknown keeper.
- What the check binds: a request encrypted to the request key can be read only by the attested
  instance, and a reply signed by the reply key came from it, so where TLS ends (the Cloudflare
  Worker for the page, Phala's gateway on dstack) does not decide who reads a request. Page
  JavaScript cannot see its TLS peer's key, so the page binds no TLS channel and relies on these
  two keys alone. The Worker and the gateway can delay, drop or replay requests (the keeper's
  nonce and session checks stand against replay), but cannot read them or forge replies.
- The page attests each keeper and encrypts to the one it addresses, so failover
  (`docs/mainnet-runbook.md`, section 10) is a retry by the page, not a forward by the Worker.
- What it cannot bind: the page is served by the operator, and a different page could skip the
  check ("The Worker operator" above). A standalone `keeler attest-check`, run by the user, makes
  the same check without depending on the page.

### Keys made inside the enclave [proposal]

- The enclave generates the keeper key and the pool key itself, never exports them in clear, and
  seals them under a platform key: a key from dstack's `getKey`, or a KMS data key on Nitro. They
  are not derived directly with `getKey`: the dstack SDK says derived keys are "not suitable for
  holding assets", and suggests a random key sealed under the derived key instead.
- The current pool key was made on the deploy machine, so this means a new deployment with new
  compiled `KEEPERS` and `POOL_VIEWING`; it cannot be applied to the present keys.
- The measured image cannot compile in `KEEPERS` or `POOL_VIEWING` as the keeper does today
  (`docs/mainnet-runbook.md`, section 6; the binary refuses to run with other values), because it
  has yet to generate them. It would read its public keys from the sealed state and report them
  in the binding, and the program and the site would be built from an attested first boot.
- Two keepers hold one pool key and two keeper keys. The sealed pool key passes between the
  instances as ciphertext; each instance seals its own keeper key. On dstack this is not a trust
  boundary: every instance and every approved compose under one app id derives the same sealing
  keys.
- The pool key cannot rotate (`docs/mainnet-runbook.md`, section 11). If the sealing key is lost,
  or the platform root is rotated without a migration, the sealed pool key cannot be read and, as
  with every pool-key holder gone, Earn deposits and indebted positions cannot move. A backup
  outside the enclave puts the key back with an operator. Each platform's case is below.

### Encrypted state outside the enclave [proposal]

- The state file (openings, tickets, pending builds, scan marks) is sealed with an AEAD under the
  platform key and stored by the host, which holds bytes it cannot read. On dstack the data disk
  is LUKS2 `aes-xts-plain64` with no authentication tag; ZFS, the default, checksums every block,
  ext4 does not. The AEAD is what detects a changed file on either platform.
- The host can hand the enclave an older file (dstack's security model: the host can restore an
  earlier valid disk image). The chain is the anchor against that: on load the enclave checks
  each note it holds against its nullifier account and replays the history from its scan mark, as
  the keeper does today (`docs/lend-keeper.md`, "Recovery"), so an old reserve opening is seen to
  be spent and the current notes are read back from the chain. A rollback can still change the
  record that is not on chain: tickets and pending builds. No version counter is proposed: kept in
  a store the operator controls, it can be restored together with the file.

### A single-writer lease [proposal]

- Today the lock stops two processes on one file, not two machines with one key
  (`docs/lend-keeper.md`, "Never"). Phala's replica guide advises against replicas for a workload
  that assumes it is the only writer, and the dstack app policy counts no instances. The design
  adds a lease per keeper key in a store with conditional writes (for example a DynamoDB item:
  holder, expiry, a fencing token that only grows). The enclave sends nothing and writes no state
  without an unexpired lease, and stops a margin before expiry when a renewal fails.
- A fencing token protects only what checks it: the state store must refuse a write with an older
  token. The chain checks no token.
- The expiry needs a time source the operator cannot skew, and the operator owns the lease store,
  can rewrite or delete the item, and carries the enclave's network traffic. So the lease guards
  against faults (a host that stalls and comes back, a failover run twice), not the operator.
- The backstop is the chain: two builds on one reserve cannot both land (the reserve's
  nullifier), so two writers cost a fee. Deposits spend no reserve, so two writers can still pass
  `max_total_collateral` together (review K-2).

### Phala dstack: the plan [proposal]

- The keeper runs as a container pinned by digest in a dstack CVM (dstack facts here are from
  commit `63dde6a`). MRTD measures the virtual firmware; RTMR0 the virtual hardware (vCPUs,
  memory, devices); RTMR1 the kernel; RTMR2 the kernel command line, with the root file system
  hash, and the initrd; RTMR3 the runtime events, among them `app-id`, `compose-hash`,
  `instance-id`, `os-image-hash` and `key-provider`. `dstack-mr measure` computes MRTD and RTMR0
  to RTMR2 from the OS image for the chosen vCPUs and memory. The site pins those, the `app-id`,
  `compose-hash` and `os-image-hash` events and the `key-provider` id. dstack's
  `make os-repro-check` builds the OS twice and compares the archives, which is dstack's own check,
  not an independent rebuild; the keeper image's reproducibility is ours to show, as on Nitro.
- `compose-hash` is the hash of the `app-compose.json` file. The environment is not measured and
  the host can replace it, so every setting the keeper relies on is in the compose.
- RTMR3 replay, in the V1 digest format, the one Helius' verifier accepts:
  `digest = SHA-384(le32(0x08000001) ‖ ":" ‖ name ‖ ":" ‖ payload)`, then
  `rtmr3 = SHA-384(rtmr3 ‖ digest)` from 48 zero bytes. V2 hashes canonical JSON and is opt-in
  (`event_log_version: 2`). For V2 the dstack text says privileged code in the CVM can append
  measured events after boot, so a policy takes each trusted event from before `system-ready`
  and refuses duplicate names. The check would apply that rule to V1 too, and refuse a V1 event
  name that contains ":" and any event type other than 0x08000001, as dstack's own validation and
  Helius' contract do.
- Debug. Nothing is zeroed: the debug bit is in the TD attributes of the signed quote.
  `dcap-qvl` 0.6.6, the version in `web/pnpm-lock.yaml` through the Rings SDK, refuses it unless
  the caller sets `allowDebug`, as dstack's own quote check does; the page never sets it. A
  development OS image (`"is_dev": true` in its metadata; Phala's November report says it includes
  SSH) is never pinned. The compose sets `public_logs` to false: dstack documents container logs
  as public by default.
- Verification. `dcap-qvl` checks the quote against Intel's root CA with the collateral at the
  page's clock. It refuses TCB info or a QE identity past its `nextUpdate` and a `Revoked` TCB,
  and returns the other statuses (`OutOfDate` among them), so the site pins the statuses it
  accepts. The keeper relays the Intel-signed collateral, as Helius' prover does; Helius' README
  states the cost: a newly revoked TCB shows only after the prover refreshes its collateral. Here
  the bound is the relayed collateral's `nextUpdate`. `dcap-qvl` fetches from Phala's PCCS by
  default and also defines Intel's PCS, which the keeper would use.
- Keys. Every app key derives from the KMS root, the app id and the app epoch; the disk key adds
  the instance id. The compose hash and the OS image are checked before release but are not
  inputs, so any compose approved under the app id receives the same keys. Only the on-chain KMS
  is used: on Phala Cloud's own KMS, Phala's November report says, Phala sets the key-release
  policy. The routes to the keys:
  - the DstackApp owner: adding a compose hash or a device to any epoch row, `setAllowAnyDevice`,
    `setAllowAnyTeeVariant` or `setAllowedTeeVariants`, `setRequireTcbUpToDate`, the launch-ticket
    setters, `transferOwnership`, upgrading the app contract until `disableUpgrades()`; each a
    public transaction;
  - the DstackKms owner: upgrading the KMS contract, adding OS images, KMS measurements, KMS
    devices and root epochs, and setting TEE variants, SEV-SNP TCB requirements, KMS launch
    tickets and ownership. On Ethereum (`0xd343a3f5593b93D8056aB5D60c433622d7D65a80`) and Base
    (`0x2f83172A49584C017F2B256F0FB2Dca14126Ba9C`) it is
    `0x3926658a072f82baea5e5a15e1fed8a776f4a663`, which reports a threshold of 2
    (`getThreshold()`) and three owners (`getOwners()`), read 2026-11-26 at Ethereum block
    26498932 and Base block 62502083; its deployed bytecode was not compared with any source;
  - the holder of the KMS root, who derives every app key directly.
- The app policy pins no MRTD or RTMR: it checks the launch ticket, app epoch, TCB status, compose
  hash, device and TEE variant; the KMS contract adds its OS image allowlist. A client attests
  only the instance it reaches and never sees another one running another approved compose. So
  the design requires `disableUpgrades()`, a device allowlist,
  `setAllowedTeeVariants(["dstack-tdx"])` (the variant policy is unrestricted by default),
  `requireTcbUpToDate` (which refuses key release after an Intel TCB recovery until the hosts are
  patched, a liveness cost), and a DstackApp owner that is the operators' own multisig, a second
  governance surface beside the Solana upgrade multisig. The claim is that operators cannot
  obtain the keys without a public transaction, not that they cannot read amounts: account views
  cross the Worker in clear (W-3), and the operator serves the page.
- The check above reads no chain, so nothing in it sees those transactions. `keeler attest-check`
  would also read the DstackApp at the pinned `app-id`: its owner, that upgrades are disabled,
  the flags, and the whole event history of compose hashes and devices in every epoch row. A
  hash that was added and later removed still received the keys.
- Key loss and root rotation. A new root or app epoch gives a new `getKey` key, so the sealed keys
  are read under the old epoch (`getKey` takes epoch selectors) and sealed again. That needs the
  compose in the old app row and the OS image in the old root row, and the DstackKms owner
  controls the second (`docs/app-key-epochs.md`). When Phala
  replaced its legacy Cloud KMS root, its November report says, `getKey()` returned new keys, the
  old root stayed reachable as epoch 0 on request, apps using `getKey()` had to move what the old
  keys protected, and the migration did not revoke the attacker's copy. The pool key cannot move,
  so whoever holds a leaked root and the sealed file can read it.
- Trust base: Intel (the TDX module, the PCK chain and root CA, the PCS) and Phala (the KMS root
  and its nodes, the DstackKms owner, OS releases, the hosts, the VMM, the gateway), and Ethereum
  or Base with the RPC endpoint the KMS reads the policy through (`ETH_RPC_URL`). Intel's TDX
  overview: denial of service by the VMM is out of scope, replay of memory through physical
  attacks is not defended against, and a hardware attacker can see memory ciphertext.
- Phala's published incidents, as Phala's own posts state them:
  - From 2026-05-31, patched 2026-06-01: an API flaw let an attacker change some CVMs and add a
    pre-launch script that may have read decrypted environment variables; off-chain KMS CVMs only,
    on-chain KMS CVMs outside the affected scope (notice of 2026-06-01).
  - 2026-10-08 to 2026-10-12 (timeline from 2026-10-11): the guest agent's log endpoint returned
    container configuration, environment variables included, for a small set of apps (OS images
    0.3.1 to 0.6.0); TEE, sealing and KMS keys and on-chain KMS operations unaffected (post-mortem
    of 2026-10-19).
  - 2026-10-20 to 2026-11-01: through a monitoring CVM's default password, an attacker reached a
    VMM that took any request from loopback; requests to an experimental DeepSeek CVM on the
    development OS image are counted at risk (November report).
  - 2026-11-04 to 2026-11-06: an attacker reached the VMM on two Phala Cloud hosts, read the
    Cloud gateway's secrets and obtained the legacy Cloud KMS root; a KMS replica whose onboarding
    checked only that the OS image was known handed it over seven times on 2026-11-05, the first
    to a caller at the attacker's address; every key under that root is treated as exposed; a new
    root went live 2026-11-06; on-chain KMS applications not affected (November report, final
    version of 2026-11-23, with a penetration test and remaining migrations still open).
- No single-instance guarantee: replicas share the app id and compose hash, and so the app keys.

### AWS Nitro Enclaves: the alternative [proposal]

- The keeper runs from an enclave image file (EIF) made with `nitro-cli build-enclave` from a
  tagged commit: the `keeler keeper` binary built with the deploy's build parameters except the
  keeper set and pool key (`docs/mainnet-runbook.md`, section 6), the ceremony's proving keys
  checked against the manifest, a minimal init, no shell.
- The PCRs, as AWS defines them: PCR0 measures the image file, PCR1 the kernel and boot ramfs,
  PCR2 the application, PCR8 the signing certificate (only for a signed image). PCR3 is a hash of
  the parent instance's IAM role ARN and PCR4 of the parent's instance id: they describe the host,
  not the code. The release publishes PCR0, PCR1 and PCR2; the KMS policy may also pin PCR3.
- Anyone can rebuild the tag and compare. That needs a pinned `nitro-cli` with the kernel and init
  blobs it ships (they set PCR1), a container image pinned by digest, `cargo build --locked` with
  fixed paths and timestamps, and a file system with fixed order, owners and times. AWS's pages
  say nothing about reproducible builds, so reproducibility is shown by independent rebuilds that
  match, not assumed.
- Production mode only. AWS states that an enclave booted in debug mode, or with an attached
  console, produces attestation documents whose PCRs are all zeros and that cannot be used for
  attestation. A KMS policy pinned to the published values refuses it, and so does the check.
- Sealing: the enclave calls KMS `GenerateDataKey` with an attestation document as the
  `Recipient`. KMS returns the data key encrypted to the document's public key
  (`CiphertextForRecipient`; the plaintext field is empty) and a copy encrypted under the KMS key.
  The enclave seals its keys and state with the data key (AES-256-GCM) and gives the parent only
  ciphertext; after a restart it calls `Decrypt` on that copy with a fresh attestation document.
- The key policy allows `Decrypt` and `GenerateDataKey` only when
  `kms:RecipientAttestation:ImageSha384` and `kms:RecipientAttestation:PCR<n>` (PCR0 to PCR2, and
  PCR3 for the role) equal the published values; a request without an attestation document cannot
  satisfy the condition. A new release has a new PCR0, so the policy is changed to admit it and
  the site is rebuilt with the new values.
- The policy denies `Encrypt` and `ReEncrypt*` to everyone. Neither takes an attestation document
  (the operations that do are `Decrypt`, `DeriveSharedSecret`, `GenerateDataKey`,
  `GenerateDataKeyPair` and `GenerateRandom`). With `Encrypt`, the operator could seal a key it
  knows in a form the enclave accepts; with `ReEncrypt`, it could move the sealed data key under a
  KMS key it may decrypt without attestation.
- Key loss: it follows from the sealing that a deleted KMS key, or a policy that admits no running
  image, leaves the sealed pool key unreadable.
- The evidence is the attestation document: CBOR, COSE_Sign1 signed with ES384, carrying
  `module_id`, `timestamp`, `digest`, `pcrs`, `certificate`, `cabundle` and the optional `nonce`,
  `user_data` and `public_key` (each at most 1,024 bytes). The check: the chain from `cabundle` to
  the AWS Nitro Enclaves root certificate (whose fingerprint AWS publishes), each certificate's
  validity, the COSE signature, the nonce, the timestamp's age, PCR0 to PCR2 against the compiled
  values, no all-zero PCR, and `user_data` and `public_key` against the binding and request key.
- Trust base: AWS, the Nitro hypervisor and the AWS Nitro Enclaves root certificate.

### Side by side

| | Phala dstack (Intel TDX) | AWS Nitro Enclaves |
| --- | --- | --- |
| What is measured | MRTD, RTMR0 to RTMR2 (firmware, virtual hardware, kernel, command line, initrd), RTMR3 events (`app-id`, `compose-hash`, `os-image-hash`, `key-provider`) | PCR0 to PCR2 (image, kernel and ramfs, application); PCR3 and PCR4 describe the host |
| How debug shows | The TD debug attribute in the quote; the verifier must refuse it | PCRs all zeros |
| Who can release the keys | The KMS, under the DstackApp and DstackKms policies; also anyone holding the KMS root | AWS KMS, under the key policy's attestation conditions |
| Where the release policy lives | Ethereum or Base contracts; the DstackApp owner and the DstackKms owner change it in public transactions | The key policy in the operator's AWS account; changed by that account, not publicly |
| What a rebuild needs | The dstack OS release and `dstack-mr` for the vCPUs and memory; the keeper container by digest; the compose file | A pinned `nitro-cli` with its kernel and init blobs; the keeper container by digest |
| Root of trust | Intel's TDX module and root CA | The Nitro hypervisor and the AWS Nitro Enclaves root certificate |
| Also trusted | Phala: KMS root, DstackKms owner, OS releases, hosts, VMM, gateway | AWS |
| Single-writer support | The app policy counts no instances; replicas share keys; the lease above | Nitro Enclaves count no instances either; the lease above |

### What neither enclave fixes [proposal]

- **Liveness.** The operator, or on dstack Phala's hosts, can stop the instance, cut its network
  or, on Nitro, withhold the vsock proxy. On dstack the DstackKms owner can also stop key release
  at the next boot by removing the OS image. The pool still halts; the trust statement stays.
- **The operator chooses the image and the release policy.** Users compare measurements with
  values the site compiles in, and the site is served by the same operator; independent parties
  who rebuild the tag and publish the measurements are the check on that. On Nitro the key policy
  and CloudTrail are inside the operator's AWS account and not publicly readable, so "only the
  published image can decrypt the keys" is the operator's statement, not something a user can
  verify. On dstack the approvals are public, but the operators' multisig makes them.
- **Side channels.** The proposal claims no protection against timing or microarchitectural side
  channels, nor against what the host learns from the size and timing of the enclave's traffic.
- **Corrupt ciphertext.** An enclave running honest code will not write an unreadable reserve, but
  nothing on chain proves it did not.
- **Public wallets.** Wallets are public on every operation in the Default Ring; the enclave
  changes nothing on chain.
- **The Worker.** Account views still cross it in clear (W-3) unless the keeper encrypts them to
  the browser's session key, a separate change.

### What Custom Rings would change [proposal]

A ring that checks output encryption to an auditor key would remove the "corrupt ciphertext" halt
and let any successor rebuild the pool from the chain; hidden owners would remove the public
wallets. The keeper would still see amounts and could still go offline, so the enclave and the
ring answer different questions: the enclave makes the keeper's code attestable, the ring makes its
writes checkable on chain.

### References

All read on 2026-11-26. dstack files are under
https://github.com/Dstack-TEE/dstack/blob/63dde6a76e5c59402ab7ae3c26519ed1d10cbeae/:

- `docs/attestation-tdx.md` (MRTD, RTMRs, `dstack-mr`); `docs/building-guest-os.md` (development
  image, `os-repro-check`); `docs/onchain-governance.md`, `docs/app-key-epochs.md` (app ids,
  epochs).
- `docs/security/security-model.md` (storage, rollback, TCB status);
  `docs/security/security-best-practices.md` (digest pinning, public logs, event-log V2);
  `docs/security/cvm-boundaries.md` (`app-compose.json`, `compose-hash`, `storage_fs`).
- `sdk/js/README.md`, SDK 0.6.0 (`getKey`, holding assets); `dstack/kms/src/main_service.rs`
  (`derive_app_keys`); `dstack/kms/auth-eth/contracts/DstackApp.sol` and `DstackKms.sol`;
  `dstack/kms/auth-eth/README.md` (`ETH_RPC_URL`).
- `dstack/cc-eventlog/src/runtime_events.rs` (V1 and V2 digests);
  `dstack/dstack-attest/src/attestation.rs` (debug check);
  `dstack/dstack-util/src/system_setup.rs` (the RTMR3 events, `aes-xts-plain64`).

Helius: `WIRE_CONTRACT.md` and `README.md` under
https://github.com/helius-labs/privacy-rings/blob/5571dc0e1031bea5c77496584a6e1d970fbbd3fe/prover/tee/.
`@phala/dcap-qvl` 0.6.6: `src/verify.js`, `src/tcb_info.js` and `src/collateral.js` under
https://unpkg.com/@phala/dcap-qvl@0.6.6/.

- Phala, Cloud KMS and on-chain KMS (contract addresses):
  https://docs.phala.com/phala-cloud/key-management/cloud-vs-onchain-kms
- Phala, multi-replica scaling: https://docs.phala.com/phala-cloud/cvm/multi-replica-scaling
- Phala, network security (gateway TLS): https://docs.phala.com/phala-cloud/networking/security
- Phala, June 2026 notice:
  https://phala.com/posts/security-incident-notice-phala-cloud-api-vulnerability
- Phala, October 2026 post-mortem:
  https://phala.com/posts/2026-10-post-mortem-dstack-guest-agent-vulnerability
- Phala, November report: https://phala.com/posts/phala-november-detailed-incident-report
- Intel, Trust Domain Extensions overview (threat model):
  https://cdrdv2-public.intel.com/856790/Intel%20Trust%20Domain%20Extensions%20Overview%20-%20June%202025.pdf
- On-chain reads (`owner()`, `getThreshold()`, `getOwners()`): https://ethereum-rpc.publicnode.com
  and https://mainnet.base.org.

AWS pages, under https://docs.aws.amazon.com/:

- Nitro Enclaves: `enclaves/latest/user/set-up-attestation.html` (PCRs, debug mode),
  `enclaves/latest/user/verify-root.html` (the attestation document and its validation),
  `enclaves/latest/user/kms.html` (using AWS KMS), `enclaves/latest/user/cmd-nitro-run-enclave.html`
  (debug mode).
- AWS KMS: `kms/latest/developerguide/cryptographic-attestation.html`,
  `kms/latest/developerguide/attested-calls.html` (`Recipient`, `CiphertextForRecipient`),
  `kms/latest/developerguide/conditions-nitro-enclave.html` (condition keys),
  `kms/latest/APIReference/API_ReEncrypt.html`.
