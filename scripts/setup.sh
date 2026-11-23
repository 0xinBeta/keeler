#!/usr/bin/env bash
# Fetch and build the pinned Helius Privacy Rings swap example (Xark) into
# .tools/, then write .tools/env with the variables the cli crate and its tests
# need. Safe to run again: the upstream scripts keep files that already match
# their pinned SHA-256 digests. The example uses forgeable test keys: local
# network only.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
repo_url=https://github.com/helius-labs/privacy-ring-examples
commit=9e5d69ade7e0c107a2929306d3b758a8969e1089
examples=$root/.tools/privacy-ring-examples
xark=$examples/swap/xark
env_file=$root/.tools/env

if [[ ! -d $examples/.git ]]; then
    git clone --quiet "$repo_url" "$examples"
fi
if [[ $(git -C "$examples" rev-parse HEAD) != "$commit" ]]; then
    git -C "$examples" fetch --quiet origin "$commit"
    git -C "$examples" checkout --quiet --detach "$commit"
fi

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

# The upstream steps run from swap/xark/, which pins its own Rust toolchain.
cd "$xark"
vars=$(scripts/setup-zk-tools.sh)
eval "$(sed 's/^/export /' <<<"$vars")"
"$ZOLANA_CLI_BIN" zk compile --test --skip-wasm --manifest-path program/Cargo.toml
scripts/check-keys.sh
cargo build-sbf --manifest-path program/Cargo.toml --tools-version v1.54 -- --locked --features bpf-entrypoint
vars+=$'\n'$(scripts/build-localnet.sh)
mkdir -p ~/.config/zolana/proving-keys

so=$xark/target/deploy/swap_program.so
[[ -f $so ]] || { echo "build-sbf left no $so" >&2; exit 1; }
{
    # build-localnet.sh prints ZOLANA_CLI_BIN again; keep each name once.
    awk -F= '!seen[$1]++ { print "export " $0 }' <<<"$vars"
    echo "export SWAP_KEYS_DIR=$xark/target/zk/swap-program"
    echo "export KEELER_SWAP_PROGRAM_SO=$so"
} >"$env_file.tmp"
mv "$env_file.tmp" "$env_file"
echo "Wrote $env_file" >&2
