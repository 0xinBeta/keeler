#!/usr/bin/env bash
# Builds Keeler Lend in this checkout, under lend/target:
#   - the circuits and their test keys (zolana zk compile --test), checked
#     against the committed lend/lend-keys.CHECKSUM;
#   - the browser package (lend/target/wasm);
#   - the SBF program (lend/target/deploy/keeler_lend.so);
#   - lend/target/keeler-lend-env, which the lend tests and tools source.
# It reads the tool paths from the main checkout's .tools/env (run
# scripts/setup.sh there once). Outside lend/target it writes the browser
# package to web/public/lend and, with --update-checksums, lend/lend-keys.CHECKSUM.
#
# Build parameters (environment): KEELER_LEND_PROGRAM_ID, KEELER_LEND_ADMIN,
# KEELER_LEND_KEEPERS, KEELER_LEND_POOL_VIEWING and the KEELER_LEND_VAULT1_*
# (see lend/program/build_params.rs; the defaults are local test keys). The
# .so it builds has the test price (marker string KEELER_LEND_TEST_PRICE_BUILD);
# a mainnet build is scripts/deploy-mainnet.sh's. Pass --update-checksums
# after changing a circuit.
#
# KEELER_LEND_OUT (default lend/target) is where the .so and keeler-lend-env
# go, so a build for another network leaves the local test build alone. The
# circuits, keys and browser package do not depend on the build parameters
# (the checksum check below fails if they ever do) and stay in lend/target.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
lend=$root/lend
main=$(git -C "$root" worktree list --porcelain | awk '/^worktree /{print $2; exit}')
[[ -f $main/.tools/env ]] || { echo "no $main/.tools/env: run scripts/setup.sh in $main first" >&2; exit 1; }
# shellcheck disable=SC1091
source "$main/.tools/env"
update=false
[[ ${1:-} == --update-checksums ]] && update=true

unset CARGO_TARGET_DIR
cd "$lend"
keys=$lend/target/zk/keeler-lend
wasm=$lend/target/wasm
# One proving thread in the browser keeps wallet popups working (no
# cross-origin isolation), as for the swap.
"$ZOLANA_CLI_BIN" zk compile --test --single-threaded --wasm-out "$wasm" --manifest-path program/Cargo.toml

circuits=(init_reserve intent cancel_intent supply redeem deposit_open deposit_add draw repay liquidate close)
sums=$(cd "$keys" && for c in "${circuits[@]}"; do shasum -a 256 "$c"/circuit.xbc "$c"/pk.bin "$c"/vk.bin; done)
if $update; then
    printf '%s\n' "$sums" >"$lend/lend-keys.CHECKSUM"
    echo "Updated lend/lend-keys.CHECKSUM" >&2
elif [[ $sums != "$(cat "$lend/lend-keys.CHECKSUM")" ]]; then
    echo "the built circuits differ from lend/lend-keys.CHECKSUM; rerun with --update-checksums if a circuit changed" >&2
    exit 1
fi
# The browser proves intent, cancel_intent and close, so it needs their artifacts.
for c in intent cancel_intent close; do
    mkdir -p "$wasm/$c"
    cp "$keys/$c/"{pk.bin,vk.bin,circuit.xbc} "$wasm/$c/"
done
# The web app serves the package from web/public/lend (git-ignored); vite.config.ts pins its digests.
web_lend=$root/web/public/lend
rm -rf "$web_lend"
mkdir -p "$web_lend"
cp -R "$wasm/." "$web_lend/"

out=${KEELER_LEND_OUT:-$lend/target}
# Local networks and devnet: the keeper's test price account (test-price).
if [[ $out == "$lend/target" ]]; then
    cargo build-sbf --manifest-path program/Cargo.toml --tools-version v1.54 --sbf-out-dir target/deploy -- --locked --features bpf-entrypoint,test-price
else
    # Its own cargo target too: the build parameters are compiled in.
    mkdir -p "$out"
    CARGO_TARGET_DIR=$out/cargo cargo build-sbf --manifest-path program/Cargo.toml --tools-version v1.54 --sbf-out-dir "$out/deploy" -- --locked --features bpf-entrypoint,test-price
fi
so=$out/deploy/keeler_lend.so
[[ -f $so ]] || { echo "build-sbf left no $so" >&2; exit 1; }

env_file=$out/keeler-lend-env
{
    printf 'export %s=%q\n' \
        KEELER_LEND_PROGRAM_ID "${KEELER_LEND_PROGRAM_ID:-A7qaBKJ1gJRcKwMWyyRdr9YhmbBmnMaemM9Sijah3EbC}" \
        KEELER_LEND_PROGRAM_SO "$so" \
        KEELER_LEND_KEYS_DIR "$keys" \
        KEELER_LEND_WASM_DIR "$wasm" \
        KEELER_LEND_PROVING_KEYS_DIR "$lend/target/proving-keys" \
        ZOLANA_CLI_BIN "$ZOLANA_CLI_BIN" \
        SHIELDED_POOL_PROGRAM_PATH "$SHIELDED_POOL_PROGRAM_PATH" \
        PROVER_BIN "$PROVER_BIN" \
        ZOLANA_PHOTON_BIN "$ZOLANA_PHOTON_BIN" \
        SURFPOOL_BIN "$SURFPOOL_BIN"
} >"$env_file.tmp"
mv "$env_file.tmp" "$env_file"
echo "Wrote $env_file" >&2
