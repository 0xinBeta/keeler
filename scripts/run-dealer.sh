#!/usr/bin/env bash
# Runs the devnet dealer behind a cloudflared quick tunnel, restarts either
# one with backoff when it exits, and writes the tunnel's URL to the Worker
# as DEALERS whenever it changes. NOT TO BE RUN until the owner approves
# publishing (it changes the deployed Worker). Ctrl-C stops both.
#
#   scripts/run-dealer.sh [dealer-config]   (default target/devnet/dealer-config.json)
#
# Needs cloudflared, wrangler logged in, and the config from deploy-devnet.sh.
# Pause quoting without stopping settlement: touch the config's pause_file.
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
bin=$target/release/keeler

dealer_pid="" tunnel_pid=""
trap 'kill $dealer_pid $tunnel_pid 2>/dev/null || true' EXIT

start_dealer() {
    "$bin" dealer "$cfg" >>"$logs/dealer.log" 2>&1 &
    dealer_pid=$! dealer_since=$SECONDS
}
start_tunnel() {
    # Fresh log: the URL grepped from it must be this tunnel's.
    : >"$logs/cloudflared.log"
    cloudflared tunnel --no-autoupdate --url "http://$http" >"$logs/cloudflared.log" 2>&1 &
    tunnel_pid=$! tunnel_since=$SECONDS
}
# A quick tunnel's URL changes on every start, so DEALERS goes in as a
# secret, which takes effect without redeploying the Worker.
publish() {
    printf '[{"url":"%s","pubkey":"%s"}]' "$1" "$dealer" |
        (cd "$root/worker" && npx wrangler secret put DEALERS)
}
# Doubles up to 60 s; a process that stayed up a minute starts again at 1 s.
next_delay() { if (($2 >= 60)); then echo 1; else echo $(($1 * 2 > 60 ? 60 : $1 * 2)); fi; }

dealer_delay=1 tunnel_delay=1 url=""
start_dealer
start_tunnel
while true; do
    if ! kill -0 "$dealer_pid" 2>/dev/null; then
        wait "$dealer_pid" || true
        dealer_delay=$(next_delay "$dealer_delay" $((SECONDS - dealer_since)))
        echo "dealer exited; restart in ${dealer_delay}s" >&2
        sleep "$dealer_delay"
        start_dealer
    fi
    if ! kill -0 "$tunnel_pid" 2>/dev/null; then
        wait "$tunnel_pid" || true
        tunnel_delay=$(next_delay "$tunnel_delay" $((SECONDS - tunnel_since)))
        echo "tunnel exited; restart in ${tunnel_delay}s" >&2
        sleep "$tunnel_delay"
        start_tunnel
    fi
    new=$(grep -oE 'https://[a-z0-9-]+\.trycloudflare\.com' "$logs/cloudflared.log" | head -1 || true)
    if [[ -n $new && $new != "$url" ]]; then
        # On failure url stays old, so the next pass tries again.
        if publish "$new"; then
            url=$new
            echo "dealer $dealer at $url" >&2
        fi
    fi
    sleep 2
done
