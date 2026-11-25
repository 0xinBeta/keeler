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

Sections are marked **[exists]** for what the code does today and **[proposal]** for what it does
not.

## Devnet today [exists]

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
- Hold the pool's single lane by asking for owner builds and never sending them (audit K-1;
  bounded by `builds_per_wallet_hour` and the reservation cooldown, trust in the keeper's limits).

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
among recent prices (`docs/plan-lend.md`, "Prices"); it cannot post an unverified or older one
(program checks owner, discriminator, verification level, feed id, age).

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
- Choose the liquidation size `x` within the circuit's bounds and choose when to liquidate (see
  below).
- On devnet: forge proofs and set the test price (see "Devnet today").

Cannot (with real keys):

- Take principal. Who is paid and how much is fixed by each circuit: intent proceeds go to the
  maker's receipt or position, change to the maker, redeem and draw payouts to the recorded owner
  (circuit).
- Spend a user's own notes (SPP: needs the user's nullifier secret).
- Write the reserve under another key: `pool_viewing` must equal the compiled `POOL_VIEWING`
  (program, `require_pool_viewing`).
- Liquidate a healthy position: the program requires the liquidation bound at or above
  `price + confidence`, and the circuit requires the debt above the threshold at that bound.
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
- Sees account views in clear: `/lend/account` replies carry the wallet's note openings (audit W-3,
  accepted).
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
(audit W-2: the fix is a second provider). A faulty indexer could make a keeper's history replay
skip an output (audit K-3).

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

## Accepted findings from the internal audit

From `docs/audit-2026-11-25.md` (internal, not an external audit; the compiled constraints and the
proof system were not covered):

| ID | Finding | Why accepted |
| --- | --- | --- |
| P-1 | A repay worth less than one debt share is accepted and removes no debt (about one base unit) | The keeper refuses repays under `min_repay`; fixing it changes a circuit for a loss of 0.000001 of a token; it goes in with the next circuit change |
| K-2 | Two keepers filling deposits at once can pass `max_total_collateral`, because a deposit does not spend the reserve | Only during a failover; the caps are a per-keeper soft limit |
| W-3 | Account views cross the Worker in clear | The Worker is run by the keeper's operator and serves the page |

The other findings were decided "fix"; the audit document is the record of each one's status.
Also not enforced by any circuit, and documented in `lend/AGENTS.md`: the unbounded intent expiry
and the liquidator change blinding above.

## Running the keeper in an AWS Nitro Enclave [proposal]

Nothing in this section is built. The aim: make the claim "the keeper runs this code, and only
this code holds the keys" checkable by a user, instead of "trust the operator's hosts".

### What is measured

- The keeper is built reproducibly from a tagged commit into an enclave image (EIF): the
  `keeler keeper` binary built with the program's build parameters (`docs/mainnet-runbook.md`,
  section 6), the ceremony's proving keys checked against the manifest, a minimal init, no shell.
- The image's measurements are published beside the release: PCR0 (the whole image), PCR1 (kernel
  and boot), PCR2 (the application), and PCR8 (the signing certificate, if the image is signed).
  Anyone can rebuild from the tag and compare PCR0.
- The enclave runs in production mode only. A debug-mode enclave reports all-zero PCRs; every
  verifier refuses them.

### How a user or the page checks it

- The keeper gains a route, `/lend/attestation?nonce=<32 hex>`, that returns a fresh Nitro
  attestation document (CBOR, COSE_Sign1, signed by the Nitro hypervisor's certificate chain).
- Its `nonce` field is the page's nonce; its `user_data` is
  `SHA-256("keeler-keeper-attest-v1" ‖ program id ‖ keeper address ‖ pool viewing key ‖ nonce)`;
  its `public_key` is the enclave's TLS key, so the HTTPS session terminates inside the enclave
  and the parent instance sees only ciphertext.
- The page (and a standalone `keeler attest-check` command) verifies the chain to the AWS Nitro
  Enclaves root certificate, the signature, the freshness, that PCR0 is one of the values compiled
  into the site (as the keeper set and pool key are today), and that `user_data` names the keeper
  that signs tickets and pays builds. Lending stays hidden when the check fails, as it does today
  for an unknown keeper.

### Keys sealed to the measurement

- The keeper key and the pool key exist outside the enclave only as AWS KMS ciphertexts. The KMS
  key policy allows `Decrypt` only with a Nitro attestation whose image hash equals the published
  PCR0 (KMS condition keys of the `kms:RecipientAttestation:*` family; to be checked against AWS's
  documentation before implementation). KMS then returns the plaintext encrypted to the enclave's
  attested key, so the parent instance never holds it.
- The key policy denies policy changes to everyone but a separate administrator role behind the
  same Squads-style quorum used for the upgrade authority, and CloudTrail records every decrypt.
- A new release means a new PCR0, a policy change through that quorum, and a site rebuild with the
  new value: the same gate as a keeper-set change today.

### Encrypted state outside the enclave

- The state file (openings, tickets, pending builds, scan marks) is sealed with AES-256-GCM under
  a data key from KMS `GenerateDataKey`, released by the same attestation policy, and written by
  the parent to disk or S3. The parent stores bytes it cannot read.
- Each sealed write carries a version number. The enclave refuses a state blob older than the
  version recorded in the lease (below), so the operator cannot roll the keeper back to an old copy
  (today's `docs/lend-keeper.md` already says a wrong state file is unrecoverable; an empty one is
  rebuilt from the chain).

### A single-writer lease

- Today the state-file lock stops two processes on one file, not two machines with one key
  (`docs/lend-keeper.md`, "Never"). In an enclave fleet the lease is a DynamoDB item per keeper
  key: holder id, expiry, a fencing token that only grows, and the latest state version.
- The enclave takes and renews it with conditional writes over TLS that terminates in the enclave.
  It sends no transaction and writes no state unless it holds an unexpired lease, and stops a safe
  margin before expiry when a renewal fails. Each state write includes the fencing token; a write
  with an older token is refused.
- The chain remains the final arbiter: two builds on one reserve cannot both land (the reserve's
  nullifier), so a lease failure costs a fee, not funds.

### What the enclave cannot fix

- **Liveness.** The operator can stop the instance, cut its network or withhold the vsock proxy.
  The pool still halts; the trust statement stays.
- **The operator chooses the image.** Users check the measurement against a value the site
  compiles in, and the site is served by the same operator. Independent parties who rebuild the
  tag and publish PCR0 are the check on that.
- **Corrupt ciphertext.** An enclave running honest code will not write an unreadable reserve, but
  nothing on chain proves it did not.
- **The Worker.** Account views still cross it in clear (W-3) unless the keeper encrypts them to
  the browser's session key, a separate change.
- AWS and its Nitro root of trust become part of the trusted base.

### What Custom Rings would change

A ring that checks output encryption to an auditor key would remove the "corrupt ciphertext" halt
and let any successor rebuild the pool from the chain; hidden owners would remove the public
wallets. The keeper would still see amounts and could still go offline, so the enclave and the
ring answer different questions: the enclave makes the keeper's code checkable, the ring makes its
writes checkable.
