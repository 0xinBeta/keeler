#!/usr/bin/env bash
# Runs the devnet dealer behind a cloudflared quick tunnel and writes the
# tunnel's URL to the Worker as DEALERS. NOT TO BE RUN until the owner
# approves publishing (it changes the deployed Worker). Ctrl-C stops both.
#
#   scripts/run-dealer.sh [dealer-config]   (default target/devnet/dealer-config.json)
#
# Needs cloudflared, wrangler logged in, and the config from deploy-devnet.sh.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cfg=${1:-$root/target/devnet/dealer-config.json}
target=${CARGO_TARGET_DIR:-$root/target}
logs=$root/target/devnet
mkdir -p "$logs"
# shellcheck source=/dev/null
. "$root/.tools/env"

http=$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["http"]["listen"])' "$cfg" || true)
[[ -n $http ]] || { echo "$cfg has no http.listen" >&2; exit 1; }
dealer=$(solana-keygen pubkey <(security find-generic-password -s keeler-devnet-dealer -w))

cargo build --release --quiet --manifest-path "$root/Cargo.toml" -p keeler --bin keeler
"$target/release/keeler" dealer "$cfg" 2>&1 | tee -a "$logs/dealer.log" &
dealer_pid=$!
cloudflared tunnel --no-autoupdate --url "http://$http" >"$logs/cloudflared.log" 2>&1 &
tunnel_pid=$!
trap 'kill "$dealer_pid" "$tunnel_pid" 2>/dev/null || true' EXIT

url=""
for _ in $(seq 60); do
    url=$(grep -oE 'https://[a-z0-9-]+\.trycloudflare\.com' "$logs/cloudflared.log" | head -1 || true)
    [[ -n $url ]] && break
    sleep 1
done
[[ -n $url ]] || { echo "no tunnel URL; see $logs/cloudflared.log" >&2; exit 1; }

# A quick tunnel's URL changes on every run, so DEALERS goes in as a secret,
# which takes effect without redeploying the Worker.
printf '[{"url":"%s","pubkey":"%s"}]' "$url" "$dealer" |
    (cd "$root/worker" && npx wrangler secret put DEALERS)
echo "dealer $dealer at $url" >&2
wait "$dealer_pid"
