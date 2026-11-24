#!/usr/bin/env bash
# Runs the SDK's local-network tests on port offset $ZOLANA_PORT_OFFSET
# (default 2100, prover on 3001 + offset / 100) and stops the prover it
# started. Run scripts/setup-lend.sh first.
# Usage: [ZOLANA_PORT_OFFSET=2200] [LEND_CHAIN_TESTS="earn_chain"] lend/scripts/run-chain-test.sh
set -euo pipefail
lend=$(cd "$(dirname "$0")/.." && pwd)
main=$(git -C "$lend" worktree list --porcelain | awk '/^worktree /{print $2; exit}')
# Read-only use of the main checkout's tool paths.
source "$main/.tools/env"
export ZOLANA_PORT_OFFSET=${ZOLANA_PORT_OFFSET:-2100}
prover_port=$((3001 + ZOLANA_PORT_OFFSET / 100))
export ZOLANA_PROVER_URL=http://127.0.0.1:$prover_port
unset CARGO_TARGET_DIR
stop_prover() {
    local pid
    pid=$(lsof -nP -t -iTCP:"$prover_port" -sTCP:LISTEN || true)
    [[ -n $pid ]] && kill $pid || true
}
trap stop_prover EXIT
mkdir -p "$lend/target/proving-keys"
cd "$lend"
tests=()
for t in ${LEND_CHAIN_TESTS:-chain earn_chain vault_chain}; do tests+=(--test "$t"); done
cargo test --release -p keeler-lend-sdk "${tests[@]}" -- --ignored --nocapture "$@"
