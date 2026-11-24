# Keeler Lend: the toolkit spike

Part 1 of `docs/plan-lend.md` section 0. Every item ran on a local network
(surfpool validator, Photon, the zolana prover; zolana CLI cli-v0.10.0,
spr-program 0.10.0, xark 0.3.1) in `spike/tests/spike.rs`. Result: **all six
items work**. One rule in the design must change (item 3), and one Xark
pitfall shaped the maths (below).

Run it: `lend/scripts/run-spike.sh` (port offset 2100, prover on 3022; it
stops the prover on exit, and the validator stops when the test ends).

## 1. Reserve as a `UniqueDataUtxo` — works

- `SpikeInit` creates the reserve with
  `UniqueDataUtxo::new_init(pool_owner, Poseidon("RSRV", mint_hi, mint_lo))`,
  holding a token balance (asset pinned to the mint) and `ReserveState`.
- `SpikeAccrue` spends it with `UniqueDataUtxo::new_mut(input, address, state)`.
  The address is recomputed in-circuit from the mint with
  `NewAddress::new(owner, seed).address()`. It re-creates the reserve with
  1,000,000 tokens from the keeper's note, new shares, accrued prices and a
  new rate.
- **A second reserve for the same mint is refused by the shielded pool**
  with `NullifierAlreadyQueued` (7043, `0x1b83`). Reserving an address works
  as a nullifier, so "created once" needs no extra state.
- Client side: `ProgramOwner::input(output, tree_id, leaf_index)` turns the
  output into a spendable input, and `spr_program::DataUtxo::<ReserveState>::try_from`
  decodes the state and checks the hash.

## 2. Three program-owned inputs under two PDAs, plus two signers — works

`SpikeTouch` spends three program-owned inputs: the reserve (pool PDA
`["pool", mint]`), the position and the intent (user PDA `["user", wallet]`).
The keeper pays (fee payer and SPP payer) and the user signs.

- Owner signer slots (`SppProofInputs::owner_signer_pubkeys()`: input owners,
  then data-output owners, deduplicated):
  - dummy top-up: `[pool PDA, user PDA]`
  - real top-up: `[pool PDA, user PDA, user wallet]`
- PDAs go in as non-signers and the CPI signs for both with `invoke::<20>`.
  The user wallet is a real transaction signer.
- One input tree account per `tree_contexts` entry. Nullifier PDAs come from
  `spr_program::instruction::nullifier_pda_accounts`.

## 3. Optional input and fixed output padding — works, with a constraint

- **Token inputs can be optional; data inputs cannot.**
  - `TokenUtxos::new_mut(&[top_up]).with_optional(maker, mint)` takes the
    maker's note or `WalletUtxo::dummy(tree_id)`. Both ran.
  - A dummy slot is dropped by the compact SPP layout: 3 inputs (shape
    3_4) instead of 4 (shape 4_4).
  - A `DataUtxo` input is fixed per circuit: `new_init` or `new_mut` is chosen
    at build time. Choosing between two `DataUtxo` values on a witness does
    not compile ("a witness-dependent memory copy is not supported").
  - **So "the position is created if absent" must not be a witness
    branch.** The plan has to open positions in their own circuit (deposit
    opens, later circuits always `new_mut`), or use one circuit per case.
- **Padding:** a token output that ends at zero is an empty slot
  (`empty_if_zero`). The touch outputs `[reserve, position, top-up change,
  repay change]`; zero ones are empty slots, so the shape stays 4 outputs.
  Closed intents (`DataUtxo::close`) emit nothing.

## 4. Fixed-point accrual in-circuit

The accrual is `program/src/circuits/math.rs`: the reference rules of
`core/src/lend.rs`, all 56 fixture vectors of the functions it implements
passing natively.

| Circuit | Constraints | Native proof (M-series, release) |
| --- | --- | --- |
| `SpikeMove` (accrue tx without maths) | 23,767 | 1.45 s |
| `SpikeAccrue` (accrue + shares + refresh) | 30,431 | 1.47 s |
| difference: one full accrue, mint, refresh | **6,664** | ~0.02 s |
| `SpikeTouch` (largest: accrue, repay, health, refresh) | 45,174 | 2.8 s |
| `SpikeInit` | 16,731 | 0.83 s |
| `SpikeFund` | 9,847 | 1.0 s |

SPP proofs from the local prover: 0.13–0.5 s (1–2 inputs), 0.7–2.2 s (3–4
inputs).

**Xark pitfall (cost me a debugging round; every circuit must follow it):**
a hint whose input is a *product of two witnesses* is given 0 by the
compiled circuit. `hint_div_rem(a * b, d)` passed native execution and the
shared vectors, but failed in the prover as soon as `a * b != 0`. The maths
module therefore takes every quotient from `hint_mulmod_divmod([a], [b],
[d])`, which takes the factors separately, and materialises a chained
product with `mul(a, b)` (pinned by `q == a * b`, remainder 0) first. With
that, accrual at a nonzero rate proves: borrow price 1e12 → 1,000,000,000,888.

Other Xark rules met:

- No `u128` division by a witness.
- A branch on a witness runs both arms, so out-of-range values are clamped
  after their `require`.
- An input (here `xark::input::from_fields`, used for the field → `u64`
  conversion) must not be read inside a witness branch.
- Every hint output must be constrained.

## 5. Price account and the public `now` — works

- `check_price` reads a 24-byte account the program owns
  (`price u64, confidence u64, publish_time i64`). It refuses an age over
  90 s, and `price - confidence < bound` with `0x4`; the bound is the touch
  proof's public `price_bound`.
- `check_now` refuses a public `now` outside `[clock - 120 s, clock]` with
  `0x1`.
- Both run before proof verification. All three refusals (low price, stale
  `now`, `now` ahead of the clock) were exercised on chain.

## 6. Compute units and v1 transaction size

| Instruction | CU | v1 tx bytes | ix data | accounts |
| --- | --- | --- | --- | --- |
| init (address reservation) | 242,298 | 1,139 | 789 | 8 |
| accrue (2 in) | 250,577 | 1,206 | 822 | 10 |
| fund (1 in, 3 out) | 246,813 | 1,461 | 1,110 | 9 |
| touch, dummy top-up (3 in, 4 out) | 264,753 | 1,725 | 1,146 | 13 |
| **touch, real top-up (4 in, 4 out): largest** | **269,015** | **2,106** | 1,493 | 15 |

All well under 1.4 M CU and the 4,096-byte v1 limit. Two Groth16
verifications (program and SPP) dominate.

## Notes for Part 2

- SPP proving keys for shapes 1_4, 3_4 and 4_4 are not in
  `~/.config/zolana/proving-keys`. The prover downloads them (about 68 MB) into the
  keys dir. The spike uses its own copy at `lend/target/proving-keys`.
- A dummy top-up removes an input slot, so a circuit's SPP shape depends on
  which optional inputs are real; the prover handles each shape.
