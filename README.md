# Keeler

A private quote router on a local network. See [`docs/plan.md`](docs/plan.md).

## Setup

> **Local network only.** Keeler settles through the Helius Privacy Rings
> swap example (Xark). That program uses insecure test proving keys from a
> fixed public seed, so anyone can forge its proofs. Never deploy it or use it
> with real funds.

Needs macOS on Apple silicon or Linux x86-64, git, curl, Python 3.11+, rustup
and the Solana/Agave CLI (`cargo build-sbf`).

```bash
scripts/setup.sh      # idempotent; the first run takes a while
source .tools/env
cargo check -p keeler
```

`scripts/setup.sh`:

- clones `helius-labs/privacy-ring-examples` at the pinned commit into
  `.tools/privacy-ring-examples` (git-ignored; that repository has no licence,
  so none of its code is copied into this one);
- runs the example's own `swap/xark` steps: installs the `zolana` CLI
  (`cli-v0.10.0`) into `swap/xark/.tools/`, compiles the circuits and test
  keys into `swap/xark/target/zk/`, checks them against the pinned checksums,
  builds `swap_program.so` with SBF platform tools v1.54, and fetches the
  localnet artifacts (shielded-pool program, prover, Photon, surfpool) into
  `~/.config/zolana/cache` and the user-registry program into
  `swap/xark/.tools/`. Every download is the upstream scripts' own and is
  checked against a pinned SHA-256;
- writes `.tools/env` with `ZOLANA_CLI_BIN`, `SHIELDED_POOL_PROGRAM_PATH`,
  `PROVER_BIN`, `ZOLANA_PHOTON_BIN`, `SURFPOOL_BIN`,
  `USER_REGISTRY_PROGRAM_PATH`, `SWAP_KEYS_DIR` and `KEELER_SWAP_PROGRAM_SO`.

Outside the repository it also uses rustup (Rust 1.98.1), the Solana platform
tools cache (`~/.cache/solana`) and `~/.config/zolana/`. Expect several GB in
`.tools/` and `target/`.

The `cli` crate depends on `swap-sdk` and `swap-program` by path from
`.tools/`, so run `scripts/setup.sh` before any cargo command, and keep
`.tools/` in place: the binary reads the proving keys from it at run time.
Format with `cargo fmt -p keeler-core -p keeler`, never `cargo fmt --all`.
