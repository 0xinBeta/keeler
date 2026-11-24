#!/usr/bin/env bash
# Runs the spike's localnet test on port offset 2100 (prover on 3022) and
# stops the prover it started. Usage: lend/scripts/run-spike.sh
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
cargo test --release -p lend-spike --features circuits --test spike -- --nocapture "$@"
