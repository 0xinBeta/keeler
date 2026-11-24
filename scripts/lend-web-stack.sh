#!/usr/bin/env bash
# The local stack for the lend page's real browser test (web/tests/e2e/lend-real.spec.ts):
#   - lend/sdk/examples/web_stack.rs: a local network with the lend program and the user registry,
#     at port offset 2400 (RPC 11299, Photon 11184, prover 3025), with a helper on 127.0.0.1:5303
#     for what a keeper does not do (funding the test wallet, seeding a lender);
#   - the real keeper, `keeler keeper`, on 127.0.0.1:47810, after `keeler lend-init-reserve` and
#     `keeler lend-set-price` (10 tUSD per tSOL); it keeps that test price fresh;
#   - the real Worker (wrangler dev) on 127.0.0.1:5301 (inspector 5302) in front of the keeper.
# The test's Vite dev server takes 5300 (tests/playwright.lend.config.ts).
# Run scripts/setup-lend.sh first. Test keys only.
#
#   scripts/lend-web-stack.sh start|stop|keeper-stop|keeper-start
# keeper-stop and keeper-start stop and restart only the keeper (the test's keeper-offline case).
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
main=$(git -C "$root" worktree list --porcelain | awk '/^worktree /{print $2; exit}')
# shellcheck disable=SC1091
source "$main/.tools/env"
offset=2400
helper_port=5303
worker_port=5301
keeper_port=47810
dir=$root/target/localnet/lend-web-$offset
prover_port=$((3001 + offset / 100))
keeler=$root/target/release/keeler
export ZOLANA_PORT_OFFSET=$offset
export ZOLANA_PROVER_URL=http://127.0.0.1:$prover_port

# Only this stack uses these ports, so their listeners are ours.
kill_port() {
    local pids
    pids=$(lsof -nP -t -iTCP:"$1" -sTCP:LISTEN || true)
    # shellcheck disable=SC2086
    if [[ -n $pids ]]; then kill $pids || true; fi
}

wait_port_free() {
    for _ in $(seq 30); do
        lsof -nP -t -iTCP:"$1" -sTCP:LISTEN >/dev/null 2>&1 || return 0
        sleep 1
    done
}

keeper_stop() {
    kill_port "$keeper_port"
    wait_port_free "$keeper_port"
}

keeper_start() {
    # exec and no inherited stdio: the caller (a test's execFileSync) gets its pipes back when this script exits.
    (cd "$dir" && exec nohup "$keeler" keeper "$dir/keeper.json" >>"$dir/keeper.log" 2>&1 </dev/null) &
    for _ in $(seq 120); do
        if curl -sf -H "x-keeler-key: $(cat "$dir/http-key")" "http://127.0.0.1:$keeper_port/health" >/dev/null 2>&1; then return 0; fi
        sleep 1
    done
    tail -20 "$dir/keeper.log" >&2
    echo "the keeper did not come up in 120 s" >&2
    return 1
}

stop_stack() {
    keeper_stop
    # The helper stops the validator and Photon as it exits.
    curl -sf -X POST "http://127.0.0.1:$helper_port/test/stop" >/dev/null 2>&1 || true
    wait_port_free "$helper_port"
    # wrangler stops its workerd as it exits; its command line names this stack's env file.
    pkill -f -- "--env-file $dir/worker.env" 2>/dev/null || true
    sleep 2
    for port in $worker_port $((worker_port + 1)) $helper_port $keeper_port $prover_port $((8899 + offset)) $((8784 + offset)); do kill_port "$port"; done
}

case ${1:-} in
stop)
    stop_stack
    echo "stopped the lend web stack" >&2
    exit 0
    ;;
keeper-stop)
    keeper_stop
    exit 0
    ;;
keeper-start)
    keeper_start
    exit 0
    ;;
start) ;;
*)
    echo "usage: $0 start|stop|keeper-stop|keeper-start" >&2
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
(cd "$root" && env -u CARGO_TARGET_DIR cargo build --release --quiet -p keeler)
rm -f "$dir/stack.json" "$dir/keeper-state.json" "$dir/keeper-state.price.json" "$dir/lend.pause" "$dir/keeper.log"
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

# The keeper's config: fixture keys from the helper, a test HTTP key, short tickets so the refund case waits 2 minutes.
printf 'keeler-lend-web-test-key-%s\n' "$offset" >"$dir/http-key"
node - "$dir" "$keeper_port" <<'EOF' >"$dir/keeper.json"
const [dir, port] = process.argv.slice(2);
const s = require(`${dir}/stack.json`);
console.log(JSON.stringify({
  network: { rpc_url: s.rpcUrl, indexer_url: s.indexerUrl, prover: "local" },
  key: { file: `${dir}/keeper-key.json` },
  http: { listen: `127.0.0.1:${port}`, key: { file: `${dir}/http-key` } },
  lend: {
    program_id: s.programId,
    network: "local",
    admin_key: { file: `${dir}/admin-key.json` },
    state_file: `${dir}/keeper-state.json`,
    pause_file: `${dir}/lend.pause`,
    price: "test",
    limits: { ticket_secs: 120, tickets_per_wallet_hour: 200 },
    poll_secs: 1,
  },
}, null, 2));
EOF
"$keeler" lend-init-reserve "$dir/keeper.json" >>"$dir/keeper.log" 2>&1 || { tail -20 "$dir/keeper.log" >&2; stop_stack; exit 1; }
# 10 tUSD per tSOL, +- 0.01: debt base units per collateral base unit x 1e12.
"$keeler" lend-set-price "$dir/keeper.json" 1 10000000000 10000000 >>"$dir/keeper.log" 2>&1 || { tail -20 "$dir/keeper.log" >&2; stop_stack; exit 1; }
keeper_start || { stop_stack; exit 1; }
# A lender's 20 tUSD, so there is something to borrow.
curl -sf -X POST "http://127.0.0.1:$helper_port/test/seed" -H 'content-type: application/json' \
    -d "{\"keeperUrl\":\"http://127.0.0.1:$keeper_port\",\"key\":\"$(cat "$dir/http-key")\",\"amount\":\"20000000\"}" >/dev/null ||
    { tail -20 "$dir/helper.log" >&2; tail -20 "$dir/keeper.log" >&2; echo "seeding the lender failed" >&2; stop_stack; exit 1; }

# The Worker's settings: lend on, the keeper as the first DEALERS entry, no swap dealer.
# REFERENCE is the page's reference price; it matches the test price set above.
node - "$dir/stack.json" "$keeper_port" "$(cat "$dir/http-key")" >"$dir/worker.env" <<'EOF'
const [file, port, key] = process.argv.slice(2);
const s = require(file);
const SOL = "11111111111111111111111111111111";
const vars = {
  NETWORK: "local",
  PROGRAM_ID: s.programId,
  DEALER_KEY: key,
  DEALERS: JSON.stringify([{ url: `http://127.0.0.1:${port}`, pubkey: s.keeper }]),
  ASSETS: JSON.stringify([{ mint: SOL, symbol: "SOL", decimals: 9 }, { mint: s.tusd, symbol: "tUSD", decimals: 6 }]),
  BAND_BPS: "100",
  MIN_TTL_SECS: "120",
  MAX_TTL_SECS: "600",
  REFERENCE: JSON.stringify({ [SOL]: { price: 10, expo: 0 }, [s.tusd]: { price: 1, expo: 0 } }),
  FAUCET: "0",
  UPSTREAM_RPC: s.rpcUrl,
  UPSTREAM_INDEXER: s.indexerUrl,
  UPSTREAM_PROVER: s.proverUrl,
  LEND_PROGRAM_ID: s.programId,
  LEND_POOLS: JSON.stringify([{ mint: s.tusd, curve: [{ utilisationBps: 0, rateBps: 0 }, { utilisationBps: 8800, rateBps: 500 }, { utilisationBps: 9300, rateBps: 700 }, { utilisationBps: 10000, rateBps: 1500 }], feeBps: 1000, maxUtilisationBps: 9500, minSupply: "1000000" }]),
  LEND_VAULTS: JSON.stringify([{ id: 1, collateralMint: SOL, debtMint: s.tusd, cfBps: 8000, ltBps: 8500, lmlBps: 9000, penaltyBps: 100, minCollateral: "10000000", minDebt: "1000000" }]),
};
for (const [k, v] of Object.entries(vars)) console.log(`${k}='${v}'`);
EOF

# wrangler wants the site's assets directory to exist; the test serves the page from Vite.
mkdir -p "$root/web/dist"
(cd "$root/worker" && WRANGLER_SEND_METRICS=false nohup npx wrangler dev --ip 127.0.0.1 --port "$worker_port" --inspector-port $((worker_port + 1)) --env-file "$dir/worker.env" --show-interactive-dev-session=false >"$dir/worker.log" 2>&1 &)
for _ in $(seq 120); do
    if curl -sf "http://127.0.0.1:$worker_port/api/config" | grep -q '"lend"'; then
        # What the test needs to drive the keeper: its config, the binary, the pause file.
        node - "$dir" "$keeper_port" "$keeler" "$root/scripts/lend-web-stack.sh" <<'EOF'
const fs = require("fs");
const [dir, port, keeler, script] = process.argv.slice(2);
const file = `${dir}/stack.json`;
const s = JSON.parse(fs.readFileSync(file, "utf8"));
Object.assign(s, { keeperUrl: `http://127.0.0.1:${port}`, keeperConfig: `${dir}/keeper.json`, keeler, script, pauseFile: `${dir}/lend.pause`, keeperState: `${dir}/keeper-state.json` });
fs.writeFileSync(file, JSON.stringify(s, null, 2) + "\n");
EOF
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
