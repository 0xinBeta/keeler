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

Every section describes what the code does today, except the Nitro Enclave section, which is a
proposal and marks each of its parts **[exists]** or **[proposal]**.

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

## Running the keeper in an AWS Nitro Enclave

The aim: let a user check which code holds the keeper and pool keys, instead of trusting the
operator's hosts with them. It narrows what the operator is trusted for; it does not remove the
operator. References are at the end of the section.

### What exists today [exists]

- The keeper is a `keeler keeper` process on an operator's host. Its keeper key, the pool key and
  the HTTP key are read from the macOS Keychain (`keychain_secret` in `cli/src/config.rs`); the
  state file is JSON, mode 0600, on that host's disk, with a lock that stops two processes on one
  file (`docs/lend-keeper.md`, "The state file").
- The pool key is made once, on the deploy machine, and copied to each keeper host
  (`docs/mainnet-runbook.md`, section 4). The operator holds every key in clear.
- The only TEE in the system is Helius' prover for the shielded pool's proofs ("The prover"
  above). Nothing in this repository runs in, or checks, a Nitro Enclave.

### What is measured, and how the image is built [proposal]

- The keeper runs from an enclave image file (EIF) made with `nitro-cli build-enclave` from a
  tagged commit: the `keeler keeper` binary built with the deploy's build parameters
  (`docs/mainnet-runbook.md`, section 6), the ceremony's proving keys checked against the
  manifest, a minimal init, no shell.
- The PCRs, as AWS defines them: PCR0 measures the image file, PCR1 the kernel and boot ramfs,
  PCR2 the application, PCR8 the signing certificate (only for a signed image). PCR3 is a hash of
  the parent instance's IAM role ARN and PCR4 of the parent's instance id: they describe the host,
  not the code. The release publishes PCR0, PCR1 and PCR2; the KMS policy below may also pin PCR3.
- Anyone can rebuild the tag and compare. That needs a pinned `nitro-cli` with the kernel and init
  blobs it ships (they set PCR1), a container image pinned by digest, `cargo build --locked` with
  fixed paths and timestamps, and a file system with fixed order, owners and times. AWS's pages
  define the PCRs but say nothing about reproducible builds, so reproducibility has to be shown by
  independent rebuilds that match, not assumed.
- Production mode only. AWS states that an enclave booted in debug mode, or with an attached
  console, produces attestation documents whose PCRs are all zeros and that cannot be used for
  attestation. A KMS policy pinned to the published values refuses it, and so does every verifier
  below.

### Keys made inside the enclave and sealed to the measurement [proposal]

- The enclave generates the keeper key and the pool key itself and never exports them. The
  current pool key was made on the deploy machine, so this means a new deployment with new
  compiled `KEEPERS` and `POOL_VIEWING`; it cannot be applied to the present keys.
- To keep them across restarts, the enclave calls KMS `GenerateDataKey` with an attestation
  document as the `Recipient`. KMS returns the data key encrypted to the document's public key
  (`CiphertextForRecipient`; the plaintext field is empty) and a copy encrypted under the KMS key.
  The enclave seals its keys and state with the data key (AES-256-GCM) and gives the parent only
  ciphertext.
- After a restart the enclave calls `Decrypt` on that copy with a fresh attestation document. The
  key policy allows `Decrypt` and `GenerateDataKey` only when `kms:RecipientAttestation:ImageSha384`
  and `kms:RecipientAttestation:PCR<n>` (PCR0 to PCR2, and PCR3 for the role) equal the published
  values; a request without an attestation document cannot satisfy the condition.
- The policy denies `Encrypt` and `ReEncrypt*` to everyone. Neither takes an attestation document
  (the operations that do are `Decrypt`, `DeriveSharedSecret`, `GenerateDataKey`,
  `GenerateDataKeyPair` and `GenerateRandom`). With `Encrypt`, the operator could seal a key it
  knows in a form the enclave accepts; with `ReEncrypt`, it could move the sealed data key under a
  KMS key it may decrypt without attestation.
- A new release has a new PCR0, so the key policy is changed to admit it and the site is rebuilt
  with the new values. Who may change the key policy is set inside the operator's AWS account; it
  is not a public, on-chain change like a keeper-set upgrade through the multisig.

### How the page or a checker verifies an attestation [proposal]

- A keeper route, `/lend/attestation?nonce=<32 hex>`, returns a fresh attestation document: CBOR,
  COSE_Sign1 signed with ES384, carrying `module_id`, `timestamp`, `digest`, `pcrs`,
  `certificate`, `cabundle` and the optional `nonce`, `user_data` and `public_key` (each at most
  1,024 bytes). `nonce` is the caller's. `public_key` is an enclave-generated key the page
  encrypts its requests to. `user_data` is `SHA-256("keeler-keeper-attest-v1" ‖ program id ‖
  keeper address ‖ pool viewing key ‖ reply key)`, the reply key being a second enclave key that
  signs every keeper reply. The route's reply also carries a signature by the keeper key over the
  document, so the enclave shows it holds a compiled keeper key rather than only naming one.
- The check: the certificate chain from `cabundle` to the AWS Nitro Enclaves root certificate
  (whose fingerprint AWS publishes), each certificate's validity, the COSE signature, the nonce,
  the timestamp's age, PCR0 to PCR2 against the values compiled into the site (as the keeper set
  and pool key are today), no all-zero PCR, and the keeper signature against the compiled
  `KEEPERS`. Lending stays hidden when it fails, as it does today for an unknown keeper.
- What the page's check binds: a request encrypted to `public_key` can be read only by an enclave
  running the published image, and a reply signed by the reply key came from it. Both still pass
  through the Cloudflare Worker, where the page's TLS ends. The Worker cannot read those requests
  or forge those replies, but can delay, drop or replay them (the keeper's nonce and session
  checks stand against replay). Page JavaScript cannot see its TLS peer's key, so the page cannot
  bind a TLS channel to the enclave.
- What it cannot bind: the page is served by the operator, and a different page could skip the
  check ("The Worker operator" above). A standalone `keeler attest-check`, run by the user, does
  not depend on the page; when it connects to the enclave directly with a TLS key the enclave
  names in `user_data`, it can also bind that TLS channel to the enclave.

### Encrypted state outside the enclave [proposal]

- The state file (openings, tickets, pending builds, scan marks) is sealed under the data key and
  stored by the parent, which holds bytes it cannot read.
- The parent can hand the enclave an older blob. The chain is the anchor against that: on load
  the enclave checks each note it holds against its nullifier account and replays the history
  from its scan mark, as the keeper does today (`docs/lend-keeper.md`, "Recovery"), so an old
  reserve opening is seen to be spent and the current notes are read back from the chain. What a
  rollback can still change is the record that is not on chain: tickets and pending builds.
- No version counter is proposed: kept in a store the operator controls (a DynamoDB item, a
  file), it can be restored together with the blob.

### A single-writer lease [proposal]

- Today the lock stops two processes on one file, not two machines with one key
  (`docs/lend-keeper.md`, "Never"). An enclave fleet adds a lease per keeper key in a store with
  conditional writes (for example a DynamoDB item: holder, expiry, a fencing token that only
  grows). The enclave sends nothing and writes no state without an unexpired lease, and stops a
  margin before expiry when a renewal fails.
- A fencing token protects only what checks it: the state store must refuse a write with an older
  token. The chain checks no token.
- The expiry needs a time source the operator cannot skew, and the operator owns the lease table,
  can rewrite or delete the item, and carries the enclave's network traffic. So the lease guards
  against faults (a host that stalls and comes back, a failover run twice), not against the
  operator.
- The backstop is the chain: two builds on one reserve cannot both land (the reserve's
  nullifier), so two writers cost a fee. Deposits spend no reserve, so two writers can still pass
  `max_total_collateral` together (review K-2).

### What the enclave cannot fix [proposal]

- **Liveness.** The operator can stop the instance, cut its network or withhold the vsock proxy.
  The pool still halts; the trust statement stays.
- **The operator chooses the image and the key policy.** Users compare the PCRs with values the
  site compiles in, and the site is served by the same operator; independent parties who rebuild
  the tag and publish the PCRs are the check on that. The KMS key policy and CloudTrail are inside
  the operator's AWS account and not publicly readable, so "only the published image can decrypt
  the keys" is the operator's statement, not something a user can verify.
- **The hardware and cloud vendor.** AWS, the Nitro hypervisor and the Nitro root certificate
  become part of the trusted base.
- **Side channels.** The proposal claims no protection against timing or microarchitectural side
  channels, nor against what the parent learns from the size and timing of the enclave's traffic.
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

AWS documentation read on 2026-11-25:

- Nitro Enclaves, cryptographic attestation (the PCRs, debug mode):
  https://docs.aws.amazon.com/enclaves/latest/user/set-up-attestation.html
- Nitro Enclaves, the attestation document and its validation:
  https://docs.aws.amazon.com/enclaves/latest/user/verify-root.html
- Nitro Enclaves, using AWS KMS: https://docs.aws.amazon.com/enclaves/latest/user/kms.html
- `nitro-cli run-enclave` (debug mode):
  https://docs.aws.amazon.com/enclaves/latest/user/cmd-nitro-run-enclave.html
- AWS KMS, how Nitro Enclaves use KMS:
  https://docs.aws.amazon.com/kms/latest/developerguide/services-nitro-enclaves.html
- AWS KMS, attested calls (`Recipient`, `CiphertextForRecipient`):
  https://docs.aws.amazon.com/kms/latest/developerguide/attested-calls.html
- AWS KMS, attestation condition keys:
  https://docs.aws.amazon.com/kms/latest/developerguide/conditions-attestation.html
- AWS KMS API, `ReEncrypt`: https://docs.aws.amazon.com/kms/latest/APIReference/API_ReEncrypt.html
