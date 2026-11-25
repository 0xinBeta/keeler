#!/usr/bin/env bash
# Runs the swaps' end-to-end test on a local network at port offset
# $ZOLANA_PORT_OFFSET (default 2600, prover on 3001 + offset / 100), and stops
# the prover it started. Run scripts/setup.sh first.
# Usage: [ZOLANA_PORT_OFFSET=2600] scripts/run-swap-e2e.sh
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
main=$(git -C "$root" worktree list --porcelain | awk '/^worktree /{print $2; exit}')
# shellcheck disable=SC1091
source "$main/.tools/env"
export ZOLANA_PORT_OFFSET=${ZOLANA_PORT_OFFSET:-2600}
prover_port=$((3001 + ZOLANA_PORT_OFFSET / 100))
export ZOLANA_PROVER_URL=http://127.0.0.1:$prover_port
stop_prover() {
    local pid
    pid=$(lsof -nP -t -iTCP:"$prover_port" -sTCP:LISTEN || true)
    # shellcheck disable=SC2086 # one pid per line
    if [[ -n $pid ]]; then kill $pid || true; fi
}
trap stop_prover EXIT
cd "$root"
cargo test --release -p keeler --test e2e -- --nocapture "$@"
