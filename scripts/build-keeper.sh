#!/usr/bin/env bash
# Builds the release keeler binary with a lend deployment's build parameters
# compiled in, as scripts/deploy-lend-devnet.sh compiled them into the
# deployed program: the program ID, the admin, the keeper set, the pool
# viewing key and vault 1's mints and oracle. Older records lack the oracle;
# the build default (Pyth's SOL/USD) then holds.
#
#   scripts/build-keeper.sh <build-params.json> <cargo target dir>
#
# The binary lands in <cargo target dir>/release/keeler.
set -euo pipefail

[[ $# -eq 2 ]] || { echo "usage: $0 <build-params.json> <cargo target dir>" >&2; exit 1; }
params=$1 target=$2
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
[[ -f $params ]] || { echo "no $params" >&2; exit 1; }

param() { python3 -c 'import json, sys; v = json.load(open(sys.argv[1]))[sys.argv[2]]; print(",".join(v) if isinstance(v, list) else v)' "$params" "$1"; }
opt() { python3 -c 'import json, sys; print(json.load(open(sys.argv[1])).get(sys.argv[2], ""))' "$params" "$1"; }

export KEELER_LEND_PROGRAM_ID KEELER_LEND_ADMIN KEELER_LEND_KEEPERS KEELER_LEND_POOL_VIEWING
export KEELER_LEND_VAULT1_COLLATERAL_MINT KEELER_LEND_VAULT1_DEBT_MINT
KEELER_LEND_PROGRAM_ID=$(param program_id) || { echo "$params has no program_id" >&2; exit 1; }
KEELER_LEND_ADMIN=$(param admin) || { echo "$params has no admin" >&2; exit 1; }
KEELER_LEND_KEEPERS=$(param keepers) || { echo "$params has no keepers" >&2; exit 1; }
KEELER_LEND_POOL_VIEWING=$(param pool_viewing) || { echo "$params has no pool_viewing" >&2; exit 1; }
KEELER_LEND_VAULT1_COLLATERAL_MINT=$(param collateral_mint) || { echo "$params has no collateral_mint" >&2; exit 1; }
KEELER_LEND_VAULT1_DEBT_MINT=$(param debt_mint) || { echo "$params has no debt_mint" >&2; exit 1; }
price_account=$(opt price_account)
feed_id=$(opt feed_id)
if [[ -n $price_account ]]; then export KEELER_LEND_VAULT1_PRICE_ACCOUNT=$price_account; fi
if [[ -n $feed_id ]]; then export KEELER_LEND_VAULT1_FEED_ID=$feed_id; fi

CARGO_TARGET_DIR=$target cargo build --release --quiet --manifest-path "$root/Cargo.toml" -p keeler --bin keeler
