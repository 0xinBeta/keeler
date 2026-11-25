#!/usr/bin/env bash
# The whole app on a local network, in one command:
#   1. scripts/localnet.sh start <offset>: validator, Photon, prover, dealer (HTTP + faucet);
#   2. builds web/dist with VITE_DEALERS and VITE_ASSETS taken from the stack's keeler-web.json;
#   3. writes worker/.dev.vars from the same file;
#   4. runs the real Worker under `wrangler dev`, serving the built site.
# Stops everything it started on exit (Ctrl-C, SIGTERM).
#
#   scripts/dev.sh [offset] [port]     defaults: offset 900, http://127.0.0.1:8787
#
# Test keys only; no secrets: the local dealer key is a fixed test value.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
offset=${1:-900}
port=${2:-8787}
json=$root/target/localnet/keeler-web-$offset.json
wrangler=

stop() {
    trap - EXIT INT TERM
    if [[ -n $wrangler ]]; then kill "$wrangler" 2>/dev/null || true; wait "$wrangler" 2>/dev/null || true; fi
    bash "$root/scripts/localnet.sh" stop "$offset" || true
}
trap stop EXIT INT TERM

bash "$root/scripts/localnet.sh" start "$offset" >/dev/null

# What a deploy sets at build time (plan G1): the site's own copy of the dealer keys.
VITE_DEALERS=$(node -e 'const w = require(process.argv[1]); console.log(JSON.stringify([w.dealer.pubkey]))' "$json")
export VITE_DEALERS
VITE_ASSETS=$(node -e 'const w = require(process.argv[1]); console.log(JSON.stringify(w.assets.map(({ mint, symbol, decimals }) => ({ mint, symbol, decimals }))))' "$json")
export VITE_ASSETS
# The client refuses a service on another network than the one it was built for.
export VITE_NETWORK=local
(cd "$root/web" && pnpm build)

# Values for worker/.dev.vars (git-ignored), from the stack. maxSell caps tUSD at 1,000 per order.
node - "$json" >"$root/worker/.dev.vars" <<'EOF'
const w = require(process.argv[2]);
const vars = {
  NETWORK: "local",
  DEALER_KEY: w.dealer.key,
  DEALERS: JSON.stringify([{ url: w.dealer.url, pubkey: w.dealer.pubkey }]),
  PROGRAM_ID: w.programId,
  ASSETS: JSON.stringify(w.assets.map((a) => (a.symbol === "tUSD" ? { ...a, maxSell: "1000000000" } : a))),
  BAND_BPS: "100",
  MIN_TTL_SECS: "120",
  MAX_TTL_SECS: "600",
  REFERENCE: JSON.stringify(w.reference),
  FAUCET: w.faucet ? "1" : "0",
  UPSTREAM_RPC: w.rpcUrl,
  UPSTREAM_INDEXER: w.indexerUrl,
  UPSTREAM_PROVER: w.proverUrl,
};
for (const [k, v] of Object.entries(vars)) console.log(`${k}='${v}'`);
EOF

cd "$root/worker"
WRANGLER_SEND_METRICS=false npx wrangler dev --ip 127.0.0.1 --port "$port" --show-interactive-dev-session=false &
wrangler=$!
echo "Keeler: http://127.0.0.1:$port (stack offset $offset; Ctrl-C stops it all)" >&2
wait "$wrangler"
