#!/usr/bin/env bash
# Runs the devnet dealer behind a cloudflared quick tunnel, restarts either
# one with backoff when it exits, and writes the tunnel's URL to the Worker
# as DEALERS whenever it changes. NOT TO BE RUN until the owner approves
# publishing (it changes the deployed Worker). Ctrl-C stops both.
#
#   scripts/run-dealer.sh [dealer-config]   (default target/devnet/dealer-config.json)
#
# Needs cloudflared, wrangler logged in, the config from deploy-devnet.sh and
# the .tools build for the config's program_id. A config with a `lend`
# section (docs/lend-keeper.md) also runs the keeper on the same port and
# tunnel; it needs the lend deploy (target/devnet-<name>/program-id) and the
# Keychain item keeler-devnet-deploy. The Worker still gets only DEALERS.
# Pause quoting without stopping settlement: touch the config's pause_file;
# pause new lending: touch lend.pause_file.
set -euo pipefail

redact() { perl -pe 'BEGIN { $| = 1 } s/api-key=[^&"\s]+/api-key=REDACTED/g'; }
exec > >(redact) 2> >(redact >&2)

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cfg=${1:-$root/target/devnet/dealer-config.json}
target=${CARGO_TARGET_DIR:-$root/target}
logs=$root/target/devnet
mkdir -p "$logs"
# shellcheck source=/dev/null
. "$root/.tools/env"

field() { python3 -c 'import json, sys; c = json.load(open(sys.argv[1])); print(eval(sys.argv[2], {}, {"c": c}))' "$cfg" "$1"; }
http=$(field 'c["http"]["listen"]') || { echo "$cfg has no http.listen" >&2; exit 1; }
dealer=$(field 'c["pubkey"]') || { echo "$cfg has no pubkey" >&2; exit 1; }
program_id=$(field 'c["program_id"]') || { echo "$cfg has no program_id" >&2; exit 1; }
for a in "$dealer" "$program_id"; do
    [[ $a =~ ^[1-9A-HJ-NP-Za-km-z]{32,44}$ ]] || { echo "$cfg: '$a' is not a base58 address" >&2; exit 1; }
done

# Build only against the program the config is for, never whatever .tools
# happens to hold.
lib=$(cd "$root/.tools" && pwd -P)/privacy-ring-examples/swap/xark/program/src/lib.rs
built=$(sed -nE 's/.*declare_id!\("([^"]+)"\).*/\1/p' "$lib")
if [[ $built != "$program_id" ]]; then
    echo ".tools is built for '$built', the config for $program_id; run:" >&2
    echo "  KEELER_SWAP_PROGRAM_ID=$program_id scripts/setup.sh" >&2
    exit 1
fi

# A config with a `lend` section also runs the keeper, which needs the lend
# program's build parameters compiled in exactly as scripts/deploy-lend-devnet.sh
# compiled them into the deployed program: the program ID, the admin (the
# deploy key), the keeper (the dealer) and vault 1's mints (the swap deploy's
# tSOL and tUSD). That binary gets its own cargo target, so the swap-only
# build in target/release is left alone.
lend_program=$(field 'c.get("lend", {}).get("program_id", "")')
if [[ -n $lend_program ]]; then
    [[ $lend_program =~ ^[1-9A-HJ-NP-Za-km-z]{32,44}$ ]] || { echo "$cfg: lend.program_id '$lend_program' is not a base58 address" >&2; exit 1; }
    [[ $(field 'c["lend"].get("key", "")') == "" ]] || { echo "$cfg: the devnet keeper is the dealer; drop lend.key" >&2; exit 1; }
    deployment=""
    for d in "$root"/target/devnet-*/; do
        if [[ -f $d/program-id && $(cat "$d/program-id") == "$lend_program" ]]; then deployment=${d%/}; fi
    done
    [[ -n $deployment ]] || { echo "no target/devnet-*/program-id holds $lend_program: deploy it with scripts/deploy-lend-devnet.sh first" >&2; exit 1; }
    swap=$root/target/devnet/deploy-summary.json
    summary_field() { python3 -c 'import json, sys; s = json.load(open(sys.argv[1])); print(eval(sys.argv[2], {}, {"s": s}))' "$swap" "$1"; }
    [[ $(summary_field 's["dealer"]') == "$dealer" ]] || { echo "$swap's dealer is not $dealer, the keeper the lend program was built for" >&2; exit 1; }
    # The admin's public key: bytes 32..64 of the deploy keypair, read from a
    # pipe, never a file or the command line.
    admin=$(security find-generic-password -s keeler-devnet-deploy -w | python3 -c '
import json, sys
b = bytes(json.load(sys.stdin))[32:64]
n = int.from_bytes(b, "big")
abc = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
s = ""
while n:
    n, r = divmod(n, 58)
    s = abc[r] + s
print("1" * (len(b) - len(b.lstrip(b"\0"))) + s)')
    KEELER_LEND_PROGRAM_ID=$lend_program KEELER_LEND_ADMIN=$admin KEELER_LEND_KEEPER=$dealer \
        KEELER_LEND_VAULT1_COLLATERAL_MINT=$(summary_field '{m: a for m, a, _ in s["mints"]}["tSOL"]') \
        KEELER_LEND_VAULT1_DEBT_MINT=$(summary_field '{m: a for m, a, _ in s["mints"]}["tUSD"]') \
        CARGO_TARGET_DIR=$deployment/dealer-cargo \
        cargo build --release --quiet --manifest-path "$root/Cargo.toml" -p keeler --bin keeler
    bin=$deployment/dealer-cargo/release/keeler
else
    cargo build --release --quiet --manifest-path "$root/Cargo.toml" -p keeler --bin keeler
    bin=$target/release/keeler
fi

dealer_pid="" tunnel_pid=""
trap 'kill $dealer_pid $tunnel_pid 2>/dev/null || true' EXIT

start_dealer() {
    # The dealer redacts its own logs; this catches a dependency that does not.
    "$bin" dealer "$cfg" > >(redact >>"$logs/dealer.log") 2>&1 &
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

dealer_delay=1 tunnel_delay=1 url="" publish_delay=2 publish_at=0
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
    # Quick tunnel hosts are hyphenated words; anything else in the log is
    # not this tunnel's URL.
    new=$(grep -oE 'https://[a-z0-9]+(-[a-z0-9]+)+\.trycloudflare\.com' "$logs/cloudflared.log" | head -1 || true)
    if [[ -n $new && $new != "$url" ]] && ((SECONDS >= publish_at)); then
        if publish "$new"; then
            url=$new publish_delay=2
            echo "dealer $dealer at $url" >&2
        else
            # url stays old, so a later pass tries again, backing off to 5 min.
            publish_at=$((SECONDS + publish_delay))
            echo "publishing $new failed; retry in ${publish_delay}s" >&2
            publish_delay=$((publish_delay * 2 > 300 ? 300 : publish_delay * 2))
        fi
    fi
    sleep 2
done
