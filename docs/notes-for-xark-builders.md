# Notes for Xark builders

What we hit while writing eleven Xark circuits for a Privacy Rings program (Keeler Lend), and what
we did about each. Versions throughout: `xark` 0.3.1, `spr-program` and the other `spr-*` crates
0.10.0 (`spr-hasher` 0.8.0), the `zolana` CLI `cli-v0.10.0`, SBF platform tools v1.54
(`lend/Cargo.toml`, `scripts/setup-lend.sh`).

Each pitfall says what was observed, where this repository works around it, and whether it looks
like a toolchain bug or a documented limit. "Documented" means we found it in Xark's
[What Rust compiles](https://github.com/blueshift-gg/xark/blob/master/docs/subset.md) page or in
the crate's own doc comments (read 2026-11-25; the page is the `master` branch, which may be newer
than 0.3.1). Reproductions marked "not re-run" are reduced from what we saw and have not been run
in the reduced form.

The second half of this file is the toolkit spike these notes came from, with its measurements.

## Pitfalls

### 1. A hint whose input is a product of two witnesses gets 0 in the compiled circuit

- **Observed.** `Field::hint_div_rem(a * b, d)`, with `a` and `b` both witnesses, passed native
  execution and every shared test vector, then failed in the prover as soon as `a * b != 0`: the
  compiled circuit handed the hint `q = r = 0`, so the constraint `q * d + r == a * b` could not
  hold. It cost a debugging round, because the native run is what the tests exercise.
- **Workaround.** Every quotient takes its two factors separately through
  `Field::hint_mulmod_divmod::<1>([a], [b], [d], 128)`, pinned by `q * d + r == a * b`, `r < d`
  and `q.to_bits::<128>()`. A product that is itself a factor is first made its own wire with
  `mul(a, b)` (the same hint with `d = 1`, pinned by `q == a * b` and `r == 0`).
  `lend/program/src/circuits/math.rs` (`quotient`, `mul`, `div_rem`) and
  `lend/program/src/circuits/vault.rs` (`div_floor`).
- **Classification.** Unconfirmed. It may be a toolchain bug: `hint_div_rem`'s doc comment (xark
  0.3.1, `src/lang.rs`) says it returns `[q, r]` with `a = b*q + r` and names no restriction on
  its inputs, and the native and compiled forms disagreed. The reduced reproduction below has not
  been run, so it is not shown that the failure needs only this shape.
- **Coverage here.** The workaround is exercised by every circuit that divides (accrual, shares,
  health, liquidation payout): the SDK's compiled-circuit tests (`lend/sdk/tests/refusal.rs`,
  `compiled_refuses`) run the built circuits, and the local-network tests prove them. The failing
  form below is not in any test.

Reproduction (not re-run):

```rust
use xark::prelude::*;

#[circuit]
pub fn product_hint(a: Private<Field>, b: Private<Field>, d: Public<Field>) {
    let [q, r] = Field::hint_div_rem(a * b, d);
    require_eq(q * d + r, a * b);
    require(r.lt::<64>(d));
}

#[test]
fn proves_natively_but_not_compiled() {
    product_hint(6, 7, 5).unwrap();
}
```

Expected: the built circuit proves `(6, 7, 5)` (`q = 8`, `r = 2`). What we saw in the same shape
inside a larger circuit: native execution passes, the compiled witness has `q = r = 0`, and proving
fails.

### 2. A `DataUtxo` input cannot be chosen on a witness

- **Observed.** Choosing between `DataUtxo::new_init` and `DataUtxo::new_mut` on a witness ("open
  the position if it does not exist") does not compile: "a witness-dependent memory copy is not
  supported". Token inputs can be optional
  (`TokenUtxos::new_mut(&[note]).with_optional(owner, asset)` takes a real note or
  `WalletUtxo::dummy(tree_id)`); data inputs cannot.
- **Workaround.** One circuit per case: `DepositOpen` (tag 6) creates the position,
  `DepositAdd` (tag 11) adds to it, and every later circuit uses `new_mut`.
  `lend/program/src/circuits/deposit.rs`. The cost is that the instruction tag tells an observer
  whether a deposit opened a position.
- **Classification.** Documented limit: the subset page says a memory fill or copy whose size
  depends on a witness is rejected, because the memory layout would depend on the witness.

### 3. A branch on a witness runs both arms

- **Observed.** `if` on a witness evaluates both arms and selects the result, so the arm not taken
  still runs on out-of-range values. Natively, a `to_bits` on such a value panics instead of
  failing a requirement.
- **Workaround.** Require the range, then clamp, so the untaken arm always sees an in-range value:
  `sub` in `lend/program/src/circuits/math.rs` (`require(a >= b)`, then `if a >= b { a - b } else
  { 0 }`), and `div_rem` (a zero divisor is replaced by 1 after `require(!zero)`). `math.rs`
  allows `clippy::implicit_saturating_sub` with a module attribute for this reason: the explicit
  branch is the form Xark lowers.
- **Classification.** Documented: the subset page says `if` and `match` run both arms and select
  the results. The native panic is the part that surprised us.

### 4. Every hint output must be pinned

- **Observed.** A hint allocates unconstrained witnesses; a prover may put anything there.
- **Workaround.** Every hint output is pinned by an equation and a range check (`div_rem`,
  `mul`, `div_floor` above). The SDK's compiled-circuit tests change one witness at a time and
  require the named rule to refuse it (`lend/sdk/tests/refusal.rs`, commit `970b8d1`).
- **Classification.** Documented: `hint_div_rem`'s doc comment says to pin both outputs, and
  `Field::inv` is documented as the pinned form of the raw `hint_inverse`.

### 5. Sharing a generic helper's instantiations changes other circuits

- **Observed.** Making `math`'s private `div_floor` public for the liquidation circuit changed the
  Earn circuits' witness programs, so their keys and the committed checksums
  (`lend/lend-keys.CHECKSUM`) would have changed although their source did not.
- **Workaround.** `lend/program/src/circuits/vault.rs` keeps its own copy of `div_floor` (its doc
  comment says why; merge commit `0018c0b`). Any change to code shared by circuits is checked
  against the checksum file: `scripts/setup-lend.sh` fails on a mismatch and
  `--update-checksums` rewrites it.
- **Classification.** Probably expected behaviour of a whole-crate compile, but undocumented and
  easy to miss when the circuits are frozen per program id. Not reduced.

### 6. The SPP shape depends on which optional inputs are real

- **Observed.** A dummy token input is dropped by the compact SPP layout: the spike's touch had 3
  inputs (shape 3_4) with a dummy top-up and 4 (shape 4_4) with a real one. Proving keys for
  shapes 1_4, 3_4 and 4_4 were not in `~/.config/zolana/proving-keys`; the prover downloaded them
  (about 68 MB).
- **Workaround.** None needed beyond provisioning: the prover handles each shape. The spike used
  its own key copy at `lend/target/proving-keys`.
- **Classification.** Behaviour, not a bug; worth knowing when sizing transactions, because the
  account count changes with the shape.

### 7. A proof takes at most 11 public fields

- **Observed.** `draw` needs pool owner, user owner, owner identity, two mint halves for each of
  two mints, vault, `now`, price bound and first nullifier: eleven, the most a proof takes
  (`docs/plan-lend.md`, "Background: Privacy Rings"). Byte arrays are not public inputs; a
  32-byte mint is two `u128` halves (`mint_halves` in `lend/program/src/instructions/mod.rs`).
- **Workaround.** `DrawPublicInputs` in `lend/program/src/circuits/draw.rs` is exactly eleven
  fields; the program recomputes every one from accounts and arguments before verifying.
- **Classification.** Limit of the program-proof interface in `spr-program` 0.10.0: the public
  hash takes at most 12 Poseidon inputs and one is the transaction hash. A non-byte array is
  accepted as a public field and hashed as one Poseidon input of 1 to 12 items
  (`spr-program-0.10.0/src/build/metadata.rs`, `validate_kind`); this repository does not use that
  form.

### 8. Client-only crates must stay out of the circuit build

- **Observed.** `spr-keypair` and `spr-transaction` cannot be linked into a circuit build
  (`cfg(xark)`).
- **Workaround.** Target-gated dependencies in `lend/program/Cargo.toml`
  (`[target.'cfg(all(not(xark), not(xark_client)))'.dependencies]`).
- **Classification.** Build configuration; not a bug.

### 9. `#[used]` on a static breaks the SBF program load

- **Observed.** Marking the test-price marker string `#[used]` marked the ELF's OS ABI as GNU, and
  the loader refused the program.
- **Workaround.** The marker is not `#[used]`; `set_test_price` reads it through
  `core::hint::black_box` so it is kept (`lend/program/src/vaults.rs`, `TEST_PRICE_MARKER`;
  commit `ac2d1d8`).
- **Classification.** A Rust/SBF toolchain interaction, not Xark. Not reduced.

### 10. Browser proving is single-threaded

- **Observed.** Multi-threaded WASM needs cross-origin isolation, which breaks wallet popups.
- **Workaround.** `zolana zk compile --test --single-threaded --wasm-out ...`
  (`scripts/setup-lend.sh`); proofs run in a Web Worker (`web/src/keeler/prover.worker.ts`).
- **Classification.** Platform constraint.

### 11. The viewing key of a PDA's shielded address is free

- **Observed.** A PDA owner's identity does not commit to a viewing key: owner checks compare the
  owner, and the viewing key only says whom new notes are encrypted to. Any `pool_viewing` would
  pass a circuit's owner check.
- **Workaround.** The program pins the argument: every instruction that takes `pool_viewing`
  refuses one that is not the compiled `POOL_VIEWING` (`require_pool_viewing` in
  `lend/program/src/instructions/mod.rs`). The key enters only the shielded address; nothing binds
  an output's ciphertext to it, so this stops a misconfigured keeper, not one that writes a reserve
  nobody can open.
- **Classification.** By design in the Default Ring; a program that writes notes others must read
  has to pin the key itself.

## The toolkit spike

Every item ran on a local network (surfpool validator, Photon, the zolana prover) in
`lend/spike/tests/spike.rs`. Result: all six items work. One rule in the design had to change
(item 3), and pitfall 1 shaped the maths.

Run it: `lend/scripts/run-spike.sh` (port offset 2100, prover on 3022; it stops the prover on exit,
and the validator stops when the test ends). The spike circuits (`SpikeInit`, `SpikeAccrue`,
`SpikeFund`, `SpikeTouch`) are prototypes, not the shipped circuits; their figures are a guide to
the shipped ones, not a measurement of them.

### 1. Reserve as a `UniqueDataUtxo`: works

- `SpikeInit` creates the reserve with
  `UniqueDataUtxo::new_init(pool_owner, Poseidon("RSRV", mint_hi, mint_lo))`, holding a token
  balance (asset pinned to the mint) and `ReserveState`.
- `SpikeAccrue` spends it with `UniqueDataUtxo::new_mut(input, address, state)`. The address is
  recomputed in-circuit from the mint with `NewAddress::new(owner, seed).address()`. It re-creates
  the reserve with 1,000,000 tokens from the keeper's note, new shares, accrued prices and a new
  rate.
- A second reserve for the same mint is refused by the shielded pool with
  `NullifierAlreadyQueued` (7043, `0x1b83`). Reserving an address works as a nullifier, so
  "created once" needs no extra state.
- Client side: `ProgramOwner::input(output, tree_id, leaf_index)` turns the output into a
  spendable input, and `spr_program::DataUtxo::<ReserveState>::try_from` decodes the state and
  checks the hash.

### 2. Three program-owned inputs under two PDAs, plus two signers: works

`SpikeTouch` spends three program-owned inputs: the reserve (pool PDA `["pool", mint]`), the
position and the intent (user PDA `["user", wallet]`). The keeper pays (fee payer and SPP payer)
and the user signs.

- Owner signer slots (`SppProofInputs::owner_signer_pubkeys()`: input owners, then data-output
  owners, deduplicated):
  - dummy top-up: `[pool PDA, user PDA]`
  - real top-up: `[pool PDA, user PDA, user wallet]`
- PDAs go in as non-signers and the CPI signs for both. The user wallet is a real transaction
  signer.
- One input tree account per `tree_contexts` entry. Nullifier PDAs come from
  `spr_program::instruction::nullifier_pda_accounts`.

### 3. Optional input and fixed output padding: works, with a constraint

- Token inputs can be optional; data inputs cannot (pitfall 2). So "the position is created if
  absent" must not be a witness branch: positions are opened by their own circuit.
- Padding: a token output that ends at zero can be an empty slot (`empty_if_zero`). The touch
  outputs `[reserve, position, top-up change, repay change]`; zero ones are empty slots, so the
  shape stays 4 outputs. Closed intents (`DataUtxo::close`) emit nothing. The shipped circuits use
  `with_zero_output` instead, so a zero payout is a real zero-value note and the output count never
  varies.

### 4. Fixed-point accrual in-circuit

The accrual is `lend/program/src/circuits/math.rs`: the reference rules of `core/src/lend.rs`,
with all 56 fixture vectors of the functions it implements passing natively (at the time of the
spike; the vectors are `web/tests/fixtures/lend-vectors.json`).

| Circuit | Constraints | Native proof (Apple M-series, release) |
| --- | --- | --- |
| `SpikeMove` (accrue transaction without maths) | 23,767 | 1.45 s |
| `SpikeAccrue` (accrue, shares, refresh) | 30,431 | 1.47 s |
| difference: one full accrue, mint, refresh | 6,664 | about 0.02 s |
| `SpikeTouch` (largest: accrue, repay, health, refresh) | 45,174 | 2.8 s |
| `SpikeInit` | 16,731 | 0.83 s |
| `SpikeFund` | 9,847 | 1.0 s |

SPP proofs from the local prover: 0.13 to 0.5 s (1 to 2 inputs), 0.7 to 2.2 s (3 to 4 inputs).

With pitfall 1's workaround, accrual at a nonzero rate proves: borrow price 1e12 became
1,000,000,000,888.

### 5. Price account and the public `now`: works

- `check_price` read a 24-byte account the program owns (`price u64, confidence u64,
  publish_time i64`). It refused an age over 90 s, and `price - confidence < bound`; the bound is
  the proof's public `price_bound`.
- `check_now` refused a public `now` outside `[clock - 120 s, clock]`.
- Both ran before proof verification. All three refusals (low price, stale `now`, `now` ahead of
  the clock) were exercised on chain. The shipped checks are `check_now` in
  `lend/program/src/instructions/mod.rs` and `check_draw_bound` / `check_liquidate_bound` in
  `lend/program/src/instructions/vault.rs`, which also read Pyth `PriceUpdateV2`.

### 6. Compute units and v1 transaction size

| Instruction (spike) | CU | v1 tx bytes | ix data | accounts |
| --- | --- | --- | --- | --- |
| init (address reservation) | 242,298 | 1,139 | 789 | 8 |
| accrue (2 in) | 250,577 | 1,206 | 822 | 10 |
| fund (1 in, 3 out) | 246,813 | 1,461 | 1,110 | 9 |
| touch, dummy top-up (3 in, 4 out) | 264,753 | 1,725 | 1,146 | 13 |
| touch, real top-up (4 in, 4 out): largest | 269,015 | 2,106 | 1,493 | 15 |

All under the 1.4 M CU limit and the 4,096-byte v1 transaction limit; three of the five are over
the 1,232-byte legacy packet, so these programs need v1 transactions. Two Groth16 verifications
(the program's and the shielded pool's) dominate the compute.
