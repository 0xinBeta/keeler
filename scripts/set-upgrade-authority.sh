#!/usr/bin/env bash
# Hands a deployed program's upgrade authority to another account. NOT TO BE
# RUN without the owner: the deploy key cannot take it back.
#
#   scripts/set-upgrade-authority.sh <program> <new-authority> [plan]
#   scripts/set-upgrade-authority.sh --squads <multisig> <vault-index> <program> [plan]
#
# The plan (default target/devnet/deploy-plan.json, from deploy-devnet.sh;
# with --squads also a lend or mainnet plan.json) names the network, its
# genesis hash and the current authority's Keychain item. You type the new
# authority's address again to confirm.
#
# Without --squads the new authority must be an existing system account.
# With --squads it is vault <vault-index> of that Squads v4 multisig, which
# must be autonomous with threshold at least 2 (read from chain;
# cli/src/deploy.rs `check_squads_vault`); the vault need not be funded.
# scripts/deploy-mainnet.sh does this step itself on mainnet.
set -euo pipefail

redact() { perl -pe 'BEGIN { $| = 1 } s/api-key=[^&"\s]+/api-key=REDACTED/g'; }
exec > >(redact) 2> >(redact >&2)

usage() { echo "usage: $0 <program> <new-authority> [plan] | $0 --squads <multisig> <vault-index> <program> [plan]" >&2; exit 1; }
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
target=${CARGO_TARGET_DIR:-$root/target}
# shellcheck source=/dev/null
. "$root/.tools/env"

if [[ ${1:-} == --squads ]]; then
    [[ $# -eq 4 || $# -eq 5 ]] || usage
    multisig=$2 index=$3 program=$4
    plan=${5:-$root/target/devnet/deploy-plan.json}
    [[ -f $plan ]] || { echo "no plan at $plan" >&2; exit 1; }
    cargo build --release --quiet --manifest-path "$root/Cargo.toml" -p keeler --bin keeler
    echo "This hands the upgrade authority of $program to vault $index of Squads multisig $multisig. The deploy key cannot take it back." >&2
    echo "Type $multisig to confirm:" >&2
    read -r typed
    [[ $typed == "$multisig" ]] || { echo "not confirmed; nothing sent" >&2; exit 1; }
    "$target/release/keeler" deploy hand-over "$plan" "$program" "$multisig" "$index"
    exit 0
fi

[[ $# -eq 2 || $# -eq 3 ]] || usage
plan=${3:-$root/target/devnet/deploy-plan.json}
[[ -f $plan ]] || { echo "no plan at $plan; run scripts/deploy-devnet.sh first" >&2; exit 1; }
cargo build --release --quiet --manifest-path "$root/Cargo.toml" -p keeler --example deploy
# stdin stays the terminal, for the typed confirmation.
"$target/release/examples/deploy" set-authority "$plan" "$1" "$2"
