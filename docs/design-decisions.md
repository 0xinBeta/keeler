# Design decisions

Short reasons for the choices a reviewer asks about first. The full design is
`docs/plan-lend.md`; the keeper set is `docs/plan-mainnet.md`; the threat model is
`docs/threat-model.md`.

## The reserve is one `UniqueDataUtxo`

The pool's state (supply shares, debt shares, both price indices, the stored rate, the last update,
a salt) and its idle liquidity are one note owned by the pool PDA `["pool", mint]`.

- **One spend updates balance and state together.** Accrual, the operation and the new rate are
  proved in one circuit over one input; there is no second account to keep consistent.
- **Its address is fixed by the mint.** `init_reserve` creates it with
  `UniqueDataUtxo::new_init(pool_owner, Poseidon("RSRV", mint_hi, mint_lo))`. The shielded pool
  refuses a second reservation of the address (`NullifierAlreadyQueued`, seen in the spike), so
  "created once" needs no program state. Every circuit that spends it rebuilds the address from
  the mint (`reserve_address` in `lend/program/src/circuits/notes.rs`); it is never a witness, so a
  keeper cannot substitute a reserve of its own.
- **The program pins the PDA.** `Pda::new` accepts only the canonical bump, and `require_pool_mint`
  only the pool's mint, so no second pool PDA can own a second reserve
  (`lend/program/src/instructions/mod.rs`).
- **Cost: one lane per pool.** Every operation that touches the reserve spends the current note,
  so operations are strictly sequential; the keeper queues them (`docs/lend-keeper.md`, "The
  queue"). A few operations a minute is the expected throughput.

## Fixed output counts, padded with zero-value notes

Output counts and owner tags are public in the Default Ring. So every circuit has a fixed set of
outputs, and a payout that happens to be zero is still a note (`with_zero_output`):

- `draw` always has four outputs (position, reserve, debt payout, collateral payout) and always
  reads the price account, so a borrow, a withdrawal and both at once look the same
  (`lend/program/src/circuits/draw.rs`).
- `redeem` always writes a receipt, so a full and a partial withdrawal look the same.
- `repay` always writes a change note; `liquidate` always writes the liquidator's change and
  payout.

What this does not hide: the instruction tag. `deposit_open` and `deposit_add` are separate
circuits because a data input cannot be optional (`docs/notes-for-xark-builders.md`, pitfall 2),
so an observer can tell a new position from a top-up.

## A keeper signs every reserve spend

`require_keeper` runs first in every instruction that spends a reserve, and in `set_test_price`.

- **Amounts stay private only if one party builds the pool side.** Whoever builds a reserve spend
  needs the reserve's opening, and two successive openings differ by the amount in between. If
  users built their own reserve spends, each would learn the others' amounts. So the keepers hold
  the openings and build those transactions.
- **It restricts who can write a note the others must read.** The Default Ring does not check
  output ciphertexts. A reserve written by someone outside the keeper set could be encrypted to
  nobody, which would halt the pool. With a keeper signature required and `pool_viewing` pinned
  to the compiled key, only the set writes the reserve, and only under the key the set shares.
- **It serialises the lane.** Racing keepers spend the same reserve; one transaction fails on the
  nullifier.

The signature grants no power over principal: what a keeper may do with a reserve spend is the
circuit's rule (who is paid, how much, at what price).

## The price bound is never derived from the position

`draw` and `liquidate` take a public `bound`, the collateral price the health check uses. The
program requires `price − confidence >= bound` for a draw and `price + confidence <= bound` for a
liquidation (`check_draw_bound`, `check_liquidate_bound` in
`lend/program/src/instructions/vault.rs`), with the price no older than 90 s.

- **A bound derived from the position would publish its health.** A liquidation price, or a bound
  computed from collateral and debt, is a function of the private amounts. The bound is instead the
  market price moved by a fixed margin (`bound_margin_bps`, default 50), so it says nothing an
  observer could not read from the oracle.
- **It keeps the program check one comparison.** The circuit uses the bound for the health rule;
  the program only checks the bound against the oracle. Neither needs the other's data.
- **The direction is always against the actor.** A draw is checked at a bound at or below the low
  end of the price, a liquidation at or above the high end, so neither a borrower nor a liquidator
  gains by choosing the bound inside the oracle's interval.

## Owner-signed operations are built by the keeper and checked by the page

`redeem` and `draw` spend the reserve, so the keeper must build them; they pay the owner, so the
owner must sign (`owner_identity` is a public input the program computes from the signer).

The page does not trust the keeper's transaction. Before the wallet sees it, `checkKeeperTransaction`
(`web/src/keeler/lend-check.ts`) refuses anything but the exact shape it asked for: no lookup
table, no durable nonce, the keeper as fee payer and the owner as one read-only signer, one lend
instruction of the requested op and nothing but compute-budget instructions besides, `now` within
60 s behind and 10 s ahead of chain time, a transact expiry at most 300 s ahead, no interface
transfers, the draw bound within 2% of the page's reference price, the owner's note spent first,
and every note the owner should receive hashing to the output at its slot, with each payout's
ciphertext opening under the owner's viewing key. The blindings come from a seed the owner can
derive (`Poseidon(domain, spent note's blinding)`), which is what makes this check possible.

## Liquidation is sized off-chain and only bounded in-circuit

The keeper computes the repay `x` with `core::lend::liquidate` (the largest that leaves the
position at the threshold, or the whole debt above the max limit). The circuit does not recompute
it; it requires (`lend/program/src/circuits/liquidate.rs`):

- the position is past the threshold at the bound;
- `0 < x <= debt`, and `x` removes at least one debt share, so collateral is never taken without
  less debt;
- the payout is `floor((x + floor(x × penalty / 1e4)) × 1e12 / bound)`, capped at the collateral;
- unless the position was above the max limit, it is not left healthier than the threshold.

Proving "largest" in-circuit would add the sizing maths and its rounding to the largest circuit
for no gain to the owner: within these bounds a keeper can only liquidate less than it may, which
leaves the position still liquidatable and costs the keeper, never the owner.

## What Custom Rings would change

From the Privacy Rings documentation we read (`docs/plan-lend.md`, "Facts checked"): in the
Default Ring owners are public signers, output owner tags and counts are public, and ciphertexts
are unchecked; custom rings were in audit until December 2026. With a custom ring:

- **Hidden owners.** Wallets would stop being public on every operation, which today shows who
  supplies, borrows and is liquidated.
- **A checked auditor key.** If the ring checks that outputs are encrypted to a key, a keeper
  could no longer halt the pool by writing a reserve nobody can open, and a successor could always
  take over from the chain alone.
- **Possibly open liquidation.** Liquidation needs to see a position; with ring-level disclosure to
  a liquidator set it would not have to be the keeper.

None of this is built. The keeper set, the pinned pool key and the trust statement in
`docs/mainnet-runbook.md` are the Default Ring answer.
