# Keeler Lend: measurements

Measured on 2026-11-25 from the commit that adds this file. Every figure comes from one of the
commands below, run once. Nothing is estimated or copied from another document.

Machine: `sysctl -n machdep.cpu.brand_string` prints `Apple M4` (10 cores, 16 GB). The proving
times were taken while the local validator, indexer and prover of the same test were running, so
they are a loaded machine's figures, not a quiet one's.

## Per instruction

```sh
scripts/setup-lend.sh
ZOLANA_PORT_OFFSET=3600 LEND_CHAIN_TESTS="chain earn_chain vault_chain" lend/scripts/run-chain-test.sh
```

The tests print one `PROOF` and one `TX` line per transaction they land (`lend/sdk/tests/measure.rs`):

- **Lend proof**: the Groth16 proof of the lend circuit as the instruction carries it (the
  serialized `CompressedProof` in the instruction's arguments). The shielded pool's own proof is
  part of the instruction data, not of this figure.
- **Native proving**: the lend circuit proved once more on this machine, natively, timed
  (`keeler_lend_sdk::proof::prove`). It does not include the shielded pool proof, which the local
  zolana prover makes.
- **CU**: `computeUnitsConsumed` of the confirmed transaction (`getTransaction`).
- **Tx bytes**: the v1 transaction, signatures included, compiled with a compute budget
  instruction (`spr_client::transaction_size`). The limit that applies to a v1 transaction is
  4,096 bytes (`solana_message::v1::MAX_TRANSACTION_SIZE`, solana-message 5.0.0). Every Lend
  transaction is v1.
- **Accounts**: the account metas of the lend instruction, then the distinct addresses of the
  whole transaction (fewer when one account, such as the keeper as fee payer and SPP payer,
  appears twice in the instruction).

| Instruction | Lend proof (bytes) | Native proving (s) | CU | Tx bytes (limit 4,096) | Ix data bytes | Accounts (ix / tx) | Test |
| --- | --- | --- | --- | --- | --- | --- | --- |
| init_reserve | 128 | 0.90 | 249,865 | 1,236 | 789 | 9 / 8 | earn_chain |
| intent | 128 | 0.91, 0.92 | 245,202, 245,350 | 1,244 | 893 | 9 / 7 | earn_chain |
| cancel_intent | 128 | 0.77 | 244,561 | 977 | 627 | 8 / 7 | chain |
| supply | 128 | 2.00, 1.93, 2.13 | 268,542, 267,174, 268,913 | 1,553 | 1,137 | 10 / 9 | earn_chain, vault_chain |
| redeem (partial) | 128 | 2.08 | 269,435 | 1,767 | 1,254 | 11 / 10 | earn_chain |
| redeem (full) | 128 | 2.09 | 272,070 | 1,767 | 1,254 | 11 / 10 | earn_chain |
| deposit open | 128 | 0.94, 0.95 | 245,220, 245,220 | 1,148 | 798 | 8 / 7 | vault_chain |
| deposit add | 128 | 1.26 | 249,409 | 1,214 | 831 | 9 / 8 | vault_chain |
| draw (borrow) | 128 | 2.43, 2.39 | 280,570, 276,718 | 1,949 | 1,403 | 12 / 11 | vault_chain |
| repay (partial) | 128 | 2.48 | 274,617 | 1,760 | 1,311 | 11 / 10 | vault_chain |
| repay (all, with change) | 128 | 2.51 | 276,943 | 1,760 | 1,311 | 11 / 10 | vault_chain |
| liquidate | 128 | 2.56, 3.35 | 278,637, 277,177 | 1,889 | 1,406 | 13 / 11 | vault_chain |
| close | 128 | 0.84, 1.06 | 246,335, 246,339 | 977 | 627 | 8 / 7 | vault_chain |

Where an instruction ran more than once, every run is listed. Not measured: a draw that only
withdraws collateral (the chain tests draw to borrow only); the SPP proving time alone.

## Constraints per circuit

```sh
scripts/setup-lend.sh
```

`zolana zk compile` prints `xark <vars> vars, <constraints> constraints, <ops> witness-gen ops` for
each circuit it builds:

| Circuit | Constraints | Variables | Witness-gen ops |
| --- | --- | --- | --- |
| init_reserve | 20,569 | 20,408 | 20,297 |
| intent | 14,097 | 14,027 | 13,878 |
| cancel_intent | 7,179 | 7,135 | 6,953 |
| supply | 33,652 | 33,272 | 32,927 |
| redeem | 35,302 | 34,922 | 34,510 |
| deposit_open | 14,546 | 14,449 | 14,265 |
| deposit_add | 23,859 | 23,610 | 23,316 |
| draw | 45,657 | 45,141 | 44,689 |
| repay | 46,929 | 46,364 | 45,839 |
| liquidate | 48,878 | 48,348 | 47,920 |
| close | 13,510 | 13,419 | 13,236 |
