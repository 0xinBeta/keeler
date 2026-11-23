#!/usr/bin/env bash
# Hands a deployed program's upgrade authority to another account (a
# multisig, say). NOT TO BE RUN without the owner: the deploy key cannot take
# it back.
#
#   scripts/set-upgrade-authority.sh <program> <new-authority> [plan]
#
# The plan (default target/devnet/deploy-plan.json, from deploy-devnet.sh)
# names the network and the current authority's Keychain item. The tool
# refuses a new authority that does not exist or is not a system account, and
# asks you to type its address again.
set -euo pipefail

redact() { perl -pe 'BEGIN { $| = 1 } s/api-key=[^&"\s]+/api-key=REDACTED/g'; }
exec > >(redact) 2> >(redact >&2)

[[ $# -eq 2 || $# -eq 3 ]] || { echo "usage: $0 <program> <new-authority> [plan]" >&2; exit 1; }
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
target=${CARGO_TARGET_DIR:-$root/target}
plan=${3:-$root/target/devnet/deploy-plan.json}
[[ -f $plan ]] || { echo "no plan at $plan; run scripts/deploy-devnet.sh first" >&2; exit 1; }

# shellcheck source=/dev/null
. "$root/.tools/env"
cargo build --release --quiet --manifest-path "$root/Cargo.toml" -p keeler --example deploy
# stdin stays the terminal, for the typed confirmation.
"$target/release/examples/deploy" set-authority "$plan" "$1" "$2"
