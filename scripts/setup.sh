#!/usr/bin/env bash
# Fetch and build the pinned Helius Privacy Rings swap example (Xark) into
# .tools/, then write .tools/env with the variables the cli crate and its tests
# need, and the swap web package into web/public/swap. Safe to run again: the
# upstream scripts keep files that already match their pinned SHA-256 digests.
# The example uses forgeable test keys: local network only.
# KEELER_SWAP_PROGRAM_ID sets the program's declare_id! (default: the
# example's own ID).
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
repo_url=https://github.com/helius-labs/privacy-ring-examples
commit=9e5d69ade7e0c107a2929306d3b758a8969e1089
# The real path, so a worktree whose .tools links to the main checkout's
# writes paths that outlive the worktree.
mkdir -p "$root/.tools"
tools=$(cd "$root/.tools" && pwd -P)
examples=$tools/privacy-ring-examples
xark=$examples/swap/xark
env_file=$tools/env
program_id=${KEELER_SWAP_PROGRAM_ID:-F4hjig5RJbXW6vrgYaMcuWzR55kZF6tbKyf8c36nrtvU}
web_swap=$root/web/public/swap
[[ $program_id =~ ^[1-9A-HJ-NP-Za-km-z]{32,44}$ ]] || { echo "KEELER_SWAP_PROGRAM_ID is not base58" >&2; exit 1; }

if [[ ! -d $examples/.git ]]; then
    git clone --quiet "$repo_url" "$examples"
fi
if [[ $(git -C "$examples" rev-parse HEAD) != "$commit" ]]; then
    git -C "$examples" fetch --quiet origin "$commit"
    git -C "$examples" checkout --quiet --detach "$commit"
fi

# Our patch: the `wasm` feature for the web package and its Cargo.lock lines.
if ! git -C "$examples" apply --reverse --check "$root/patches/swap.patch" 2>/dev/null; then
    git -C "$examples" apply "$root/patches/swap.patch"
fi
# declare_id! is the only place the program ID is written. Rewrite only on a
# change, so an unchanged ID does not force a rebuild.
lib=$xark/program/src/lib.rs
sed -E "s/(declare_id!\(\")[^\"]*(\"\))/\1$program_id\2/" "$lib" >"$lib.tmp"
grep -q "declare_id!(\"$program_id\")" "$lib.tmp" || { rm "$lib.tmp"; echo "no declare_id! in $lib" >&2; exit 1; }
if cmp -s "$lib" "$lib.tmp"; then rm "$lib.tmp"; else mv "$lib.tmp" "$lib"; fi

# The upstream pin check needs Python 3.11+ (tomllib); macOS /usr/bin/python3
# is 3.9 and comes first on PATH, so put a newer python3 ahead of it.
if ! python3 -c 'import tomllib' 2>/dev/null; then
    for py in python3.14 python3.13 python3.12 python3.11; do
        if py_bin=$(command -v "$py") && "$py_bin" -c 'import tomllib' 2>/dev/null; then
            mkdir -p "$root/.tools/shim"
            ln -sf "$py_bin" "$root/.tools/shim/python3"
            export PATH=$root/.tools/shim:$PATH
            break
        fi
    done
    python3 -c 'import tomllib' || { echo "Need Python 3.11 or newer" >&2; exit 1; }
fi

# The upstream steps run from swap/xark/, which pins its own Rust toolchain,
# and read their outputs from its own target/.
unset CARGO_TARGET_DIR
cd "$xark"
# Upstream scripts print NAME=value lines; export them without eval.
export_vars() {
    local name value
    while IFS='=' read -r name value; do
        [[ $name =~ ^[A-Z_][A-Z0-9_]*$ ]] || { echo "unexpected upstream line: $name" >&2; exit 1; }
        export "$name=$value"
    done <<<"$1"
}
vars=$(scripts/setup-zk-tools.sh)
export_vars "$vars"
# One proving thread in the browser keeps wallet popups working (no
# cross-origin isolation).
"$ZOLANA_CLI_BIN" zk compile --test --single-threaded --wasm-out "$web_swap" --manifest-path program/Cargo.toml
scripts/check-keys.sh
# The browser proves make and cancel, so it needs their checked artifacts.
for circuit in make cancel; do
    mkdir -p "$web_swap/$circuit"
    cp "$xark/target/zk/swap-program/$circuit/"{pk.bin,vk.bin,circuit.xbc} "$web_swap/$circuit/"
done
cargo build-sbf --manifest-path program/Cargo.toml --tools-version v1.54 -- --locked --features bpf-entrypoint
localnet_vars=$(scripts/build-localnet.sh)
export_vars "$localnet_vars"
vars+=$'\n'$localnet_vars
mkdir -p ~/.config/zolana/proving-keys

so=$xark/target/deploy/swap_program.so
[[ -f $so ]] || { echo "build-sbf left no $so" >&2; exit 1; }
{
    # build-localnet.sh prints ZOLANA_CLI_BIN again; keep each name once.
    # %q quotes each value so a path with spaces still sources.
    awk -F= '!seen[$1]++' <<<"$vars" | while IFS='=' read -r name value; do
        printf 'export %s=%q\n' "$name" "$value"
    done
    printf 'export %s=%q\n' \
        SWAP_KEYS_DIR "$xark/target/zk/swap-program" \
        KEELER_SWAP_PROGRAM_SO "$so"
} >"$env_file.tmp"
mv "$env_file.tmp" "$env_file"
echo "Wrote $env_file" >&2
