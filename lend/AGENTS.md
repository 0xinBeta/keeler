# Keeler Lend: notes for agents

The plan is `docs/plan-lend.md`; the spike results are `SPIKE.md`. `lend/` is
its own Cargo workspace (members `program`, `sdk`, `spike`), outside the root
one. Never touch `.tools/` or the main checkout's `target/`; read
`.tools/env` only.

## Who owns which files

| Piece | Files |
|---|---|
| Program core | `program/src/{lib.rs,error.rs,client.rs,wasm.rs}`, `program/src/instructions/{mod.rs,init_reserve.rs,intent.rs,cancel_intent.rs}`, `program/src/circuits/{mod.rs,notes.rs,math.rs,init_reserve.rs,intent.rs,cancel_intent.rs}`, `sdk/src/{lib.rs,proof.rs}` and the three core builders, `scripts/setup-lend.sh` |
| Earn | `supply` and `redeem`: `program/src/circuits/{supply,redeem}.rs`, `program/src/instructions/{supply,redeem}.rs`, their verifier slots, error ranges 20400 and 20500, their SDK builders and tests |
| Vault | `deposit`, `draw`, `repay`, `liquidate`, `close`: the same files for each, error ranges 20600-21000, the oracle check and the test price account |

Each later instruction already has a tag (`tag::*`), a module that returns its
`*NotBuilt` error, a doc-only circuit file, a commented verifier slot in
`program/src/verifier/mod.rs` and an error range of 100 codes
(`error.rs`). Replace the stub, uncomment the slot, add the circuit to
`circuits/mod.rs`, its id to `sdk/src/proof.rs`, and its files to
`lend-keys.CHECKSUM` (`scripts/setup-lend.sh --update-checksums`). Edits to
shared files (`mod.rs`, `error.rs`, `proof.rs`) are one line each; keep them
to that so pieces merge cleanly. Vault may split `deposit` into
`deposit_open` and `deposit_add` (an optional data input does not compile,
see `SPIKE.md`); take tag 11 for the second one.

## Reuse

- `instructions::{parse, Pda, cpi, check_now, clock, verified,
  require_keeper, require_admin, identity, mint_halves}`. Every instruction
  that spends a reserve calls `require_keeper` on its keeper account and
  `check_now(now)` on its public `now`. The price check is in
  `spike/src/instructions.rs` (`check_price`): move it into
  `instructions/mod.rs` when Vault needs it.
- `circuits::{reserve_seed, reserve_address, assert_mint, assert_note,
  assert_intent}`: every circuit that spends a reserve rebuilds its address
  from the mint, never takes it as a witness.
- `circuits::math`: `accrue(state, fee_bps, now)`, then the operation, then
  `refresh(state, pool, salt)`, which only stores the new rate and salt: it
  does not accrue, so every circuit that spends a reserve calls `accrue`
  itself first. Also share conversions, `assert_borrow_cap`, `VaultParams`. It passes
  `web/tests/fixtures/lend-vectors.json` (`cargo test -p keeler-lend
  --features build-circuits --lib`).

## Xark rules learnt the hard way

- A hint whose input is a product of two witnesses gets 0 in the compiled
  circuit: it passes natively and fails in the prover. Take quotients with
  `math`'s helpers (`hint_mulmod_divmod`), never `(a * b) / d` on fields.
- Read inputs (`to_u64`, `from_fields`) outside `if` on witness values; pin
  every hint output; clamp a value after `require`-ing its range, since a
  native `to_bits` panics.
- Data inputs cannot be optional; token inputs can
  (`TokenUtxos::new_mut(..).with_optional(owner, asset)`).
- `spr-keypair` and `spr-transaction` must stay out of the circuit build: they
  are target-gated in `program/Cargo.toml`.
- The viewing key of a PDA's shielded address is free: owner checks compare
  the owner (PDA identity), so the viewing key only says whom new notes are
  encrypted to.

## Known limits (not enforced by the circuits)

- `liquidate` seeds its blindings from the position, so the liquidated owner
  can derive the blinding of the liquidator's change note. The keeper should
  liquidate from a note of exactly `x` (empty change slot).
- An intent's expiry is private and unbounded: a far-future expiry locks the
  maker's funds until then. The keeper's ticket fixes the expiry and the page
  uses it; nothing stops an intent made by hand.

## Build and test

```sh
scripts/setup-lend.sh                     # circuits, keys check, wasm, .so (with test-price), lend/target/keeler-lend-env
cd lend
cargo test --release -p keeler-lend --features build-circuits --lib   # maths and vectors
cargo test -p keeler-lend --test program  # Mollusk, on target/deploy/keeler_lend.so
cargo test --release -p keeler-lend-sdk   # native builder tests
scripts/run-chain-test.sh                 # local network, port offset 2100
```

Features: exactly one of `insecure-test-setup` (the default) and `mainnet`.
`test-price` is not a default: `scripts/setup-lend.sh` passes it for the
local and devnet `.so`, and it cannot build with `mainnet`; such a `.so`
contains the string `KEELER_LEND_TEST_PRICE_BUILD`. A `mainnet` build needs
every build parameter set and refuses public test keys
(`program/build_params.rs`, with its unit tests).

Keepers and the pool key: any of `KEEPERS` (`KEELER_LEND_KEEPERS`) may sign
as the keeper; every `pool_viewing` argument must be `POOL_VIEWING`
(`KEELER_LEND_POOL_VIEWING`). A keeper builds with
`keeler_lend_sdk::keeper_keypair(keeper, pool_key)`: its own signing and
nullifier keys under the pool viewing key. Local tests use the fixture
keepers (actor 0 and the payer) and `fixture_keeper_keypair`.

Local networks: a port offset in 2100-2700 (`ZOLANA_PORT_OFFSET`, prover on
3001 + offset / 100), and stop what you start.
