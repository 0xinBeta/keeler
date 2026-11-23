#!/usr/bin/env bash
# The local stack for the web app: validator, Photon and prover with the swap
# program, tUSD and tSOL, and a funded, registered dealer serving HTTP with
# the faucet on. Writes the settings to target/localnet/keeler-web.json (and
# keeler-web-<offset>.json) and prints them.
#
#   scripts/localnet.sh start <offset>
#   scripts/localnet.sh stop <offset>
#
# <offset> is a multiple of 100 from 100 to 4900, so stacks can run side by
# side. Ports: RPC 8899+offset, Photon 8784+offset, prover 3001+offset/100,
# dealer TCP 47000+offset, dealer HTTP 47500+offset. Test keys only.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
usage() { echo "usage: $0 start|stop <offset: 100..4900, a multiple of 100>" >&2; exit 2; }
cmd=${1:-}
offset=${2:-}
[[ $cmd == start || $cmd == stop ]] || usage
if ! [[ $offset =~ ^[0-9]+$ ]] || ((offset < 100 || offset > 4900 || offset % 100 != 0)); then usage; fi

# shellcheck source=/dev/null
. "$root/.tools/env"
target=${CARGO_TARGET_DIR:-$root/target}
dir=$root/target/localnet/keeler-web-$offset
prover_port=$((3001 + offset / 100))
http_port=$((47500 + offset))
export ZOLANA_PROVER_URL=http://127.0.0.1:$prover_port
# The prover resolves proof data against this stack's Photon.
export ZOLANA_PORT_OFFSET=$offset
pidfile=$dir/dealer.pid

stop_stack() {
    if [[ -f $pidfile ]]; then
        kill "$(cat "$pidfile")" 2>/dev/null || true
        rm -f "$pidfile"
    fi
    if [[ -x $target/release/examples/localnet ]]; then
        mkdir -p "$dir"
        (cd "$dir" && "$target/release/examples/localnet" stop "$offset" "$dir") || true
    fi
    # Only this stack uses its prover port, so the listener there is ours.
    local pids
    pids=$(lsof -ti "tcp:$prover_port" -sTCP:LISTEN || true)
    if [[ -n $pids ]]; then
        # shellcheck disable=SC2086
        kill $pids || true
    fi
}

if [[ $cmd == stop ]]; then
    stop_stack
    echo "stopped the stack at offset $offset" >&2
    exit 0
fi

if [[ -f $pidfile ]] && kill -0 "$(cat "$pidfile")" 2>/dev/null; then
    echo "a stack is already running at offset $offset; run: $0 stop $offset" >&2
    exit 1
fi
cargo build --release --quiet --manifest-path "$root/Cargo.toml" -p keeler --bin keeler --example localnet
mkdir -p "$dir"
# The services write logs relative to the working directory.
cd "$dir"
"$target/release/examples/localnet" start "$offset" "$dir" || { stop_stack; exit 1; }
nohup "$target/release/keeler" dealer "$dir/dealer-config.json" >"$dir/dealer.log" 2>&1 &
echo $! >"$pidfile"

key=$(cat "$dir/http-key")
for _ in $(seq 120); do
    if curl -sf -H "x-keeler-key: $key" "http://127.0.0.1:$http_port/health" >/dev/null; then
        cp "$dir/keeler-web.json" "$root/target/localnet/keeler-web.json"
        cp "$dir/keeler-web.json" "$root/target/localnet/keeler-web-$offset.json"
        cat "$dir/keeler-web.json"
        echo "dealer log: $dir/dealer.log; stop with: $0 stop $offset" >&2
        exit 0
    fi
    if ! kill -0 "$(cat "$pidfile")" 2>/dev/null; then
        tail -20 "$dir/dealer.log" >&2
        echo "the dealer exited; stopping the stack" >&2
        stop_stack
        exit 1
    fi
    sleep 1
done
echo "the dealer did not answer /health in 120 s; see $dir/dealer.log" >&2
exit 1
