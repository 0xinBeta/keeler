#!/usr/bin/env bash
# The local stack for the lend page's real browser test (web/tests/e2e/lend-real.spec.ts):
#   - lend/sdk/examples/web_stack.rs: a local network with the lend program and the user registry,
#     at port offset 2400 (RPC 11299, Photon 11184, prover 3025), which also stands in for the
#     keeper on 127.0.0.1:5303 (tickets and the account view; no sessions checked);
#   - the real Worker (wrangler dev) on 127.0.0.1:5301 (inspector 5302) in front of it.
# The test's Vite dev server takes 5300 (tests/playwright.lend.config.ts).
# Run scripts/setup-lend.sh first. Test keys only.
#
#   scripts/lend-web-stack.sh start|stop
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
main=$(git -C "$root" worktree list --porcelain | awk '/^worktree /{print $2; exit}')
# shellcheck disable=SC1091
source "$main/.tools/env"
offset=2400
helper_port=5303
worker_port=5301
dir=$root/target/localnet/lend-web-$offset
prover_port=$((3001 + offset / 100))
export ZOLANA_PORT_OFFSET=$offset
export ZOLANA_PROVER_URL=http://127.0.0.1:$prover_port

# Only this stack uses these ports, so their listeners are ours.
kill_port() {
    local pids
    pids=$(lsof -nP -t -iTCP:"$1" -sTCP:LISTEN || true)
    # shellcheck disable=SC2086
    if [[ -n $pids ]]; then kill $pids || true; fi
}

stop_stack() {
    # The helper stops the validator and Photon as it exits.
    curl -sf -X POST "http://127.0.0.1:$helper_port/test/stop" >/dev/null 2>&1 || true
    for _ in $(seq 30); do
        lsof -nP -t -iTCP:"$helper_port" -sTCP:LISTEN >/dev/null 2>&1 || break
        sleep 1
    done
    # wrangler stops its workerd as it exits; its command line names this stack's env file.
    pkill -f -- "--env-file $dir/worker.env" 2>/dev/null || true
    sleep 2
    for port in $worker_port $((worker_port + 1)) $helper_port $prover_port $((8899 + offset)) $((8784 + offset)); do kill_port "$port"; done
}

case ${1:-} in
stop)
    stop_stack
    echo "stopped the lend web stack" >&2
    exit 0
    ;;
start) ;;
*)
    echo "usage: $0 start|stop" >&2
    exit 2
    ;;
esac

if curl -sf "http://127.0.0.1:$helper_port/health" >/dev/null 2>&1; then
    echo "the lend web stack is already up; run: $0 stop" >&2
    exit 1
fi
[[ -f $root/lend/target/deploy/keeler_lend.so ]] || { echo "run scripts/setup-lend.sh first" >&2; exit 1; }
mkdir -p "$dir" "$root/lend/target/proving-keys"
(cd "$root/lend" && env -u CARGO_TARGET_DIR cargo build --release --quiet -p keeler-lend-sdk --example web_stack)
rm -f "$dir/stack.json"
# The services write logs relative to the working directory.
cd "$dir"
nohup "$root/lend/target/release/examples/web_stack" "$offset" "$dir" "$helper_port" >"$dir/helper.log" 2>&1 &
helper=$!
for _ in $(seq 300); do
    [[ -f $dir/stack.json ]] && break
    kill -0 "$helper" 2>/dev/null || { tail -20 "$dir/helper.log" >&2; echo "the helper exited" >&2; stop_stack; exit 1; }
    sleep 1
done
[[ -f $dir/stack.json ]] || { echo "the helper did not come up in 300 s; see $dir/helper.log" >&2; stop_stack; exit 1; }

# The Worker's settings: lend on, the helper as the keeper (the first DEALERS entry), no swap dealer.
node - "$dir/stack.json" >"$dir/worker.env" <<'EOF'
const s = require(process.argv[2]);
const SOL = "11111111111111111111111111111111";
const vars = {
  NETWORK: "local",
  PROGRAM_ID: s.programId,
  DEALER_KEY: "keeler-lend-web-test-key",
  DEALERS: JSON.stringify([{ url: s.keeperUrl, pubkey: s.keeper }]),
  ASSETS: JSON.stringify([{ mint: SOL, symbol: "SOL", decimals: 9 }, { mint: s.tusd, symbol: "tUSD", decimals: 6 }]),
  BAND_BPS: "100",
  MIN_TTL_SECS: "120",
  MAX_TTL_SECS: "600",
  REFERENCE: JSON.stringify({ [SOL]: { price: 150, expo: 0 }, [s.tusd]: { price: 1, expo: 0 } }),
  FAUCET: "0",
  UPSTREAM_RPC: s.rpcUrl,
  UPSTREAM_INDEXER: s.indexerUrl,
  UPSTREAM_PROVER: s.proverUrl,
  LEND_PROGRAM_ID: s.programId,
  LEND_POOLS: JSON.stringify([{ mint: s.tusd, curve: [{ utilisationBps: 0, rateBps: 0 }, { utilisationBps: 8800, rateBps: 500 }, { utilisationBps: 9300, rateBps: 700 }, { utilisationBps: 10000, rateBps: 1500 }], feeBps: 1000, maxUtilisationBps: 9300, minSupply: "1000000" }]),
  LEND_VAULTS: JSON.stringify([{ id: 1, collateralMint: SOL, debtMint: s.tusd, cfBps: 8000, ltBps: 8500, lmlBps: 9500, penaltyBps: 500, minCollateral: "10000000", minDebt: "1000000" }]),
};
for (const [k, v] of Object.entries(vars)) console.log(`${k}='${v}'`);
EOF

(cd "$root/worker" && WRANGLER_SEND_METRICS=false nohup npx wrangler dev --ip 127.0.0.1 --port "$worker_port" --inspector-port $((worker_port + 1)) --env-file "$dir/worker.env" --show-interactive-dev-session=false >"$dir/worker.log" 2>&1 &)
for _ in $(seq 120); do
    if curl -sf "http://127.0.0.1:$worker_port/api/config" | grep -q '"lend"'; then
        cat "$dir/stack.json"
        echo "lend web stack up (logs in $dir); stop with: $0 stop" >&2
        exit 0
    fi
    sleep 1
done
tail -20 "$dir/worker.log" >&2
echo "the Worker did not serve a lend config in 120 s; stopping" >&2
stop_stack
exit 1
