#!/usr/bin/env bash
# Runs the lend keeper's end-to-end test on a local network at port offset
# $ZOLANA_PORT_OFFSET (default 2500, prover on 3001 + offset / 100; keeper
# HTTP on 47800), and stops the prover it started. Its last part runs the
# second fixture keeper (the payer) beside the first, so the .so must have the
# default keeper set. Run scripts/setup-lend.sh first.
# Usage: [ZOLANA_PORT_OFFSET=2500] scripts/run-lend-e2e.sh
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
main=$(git -C "$root" worktree list --porcelain | awk '/^worktree /{print $2; exit}')
# Read-only use of the main checkout's tool paths.
# shellcheck disable=SC1091
source "$main/.tools/env"
export ZOLANA_PORT_OFFSET=${ZOLANA_PORT_OFFSET:-2500}
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
cargo test --release -p keeler --test lend_e2e -- --ignored --nocapture "$@"
