#!/usr/bin/env bash
# Runs the devnet dealer behind a cloudflared quick tunnel, restarts either
# one with backoff when it exits, and writes the tunnel's URL to the Worker
# as DEALERS whenever it changes. NOT TO BE RUN until the owner approves
# publishing (it changes the deployed Worker). Ctrl-C stops everything.
#
#   scripts/run-dealer.sh [--standby] [dealer-config]
#       (default target/devnet/dealer-config.json)
#
# Needs cloudflared, wrangler logged in, the config from deploy-devnet.sh and
# the .tools build for the config's program_id. A config with a `lend`
# section (docs/lend-keeper.md) also runs the keeper on the same port and
# tunnel; it needs the lend deploy (target/devnet-<name>/: program-id,
# build-params.json and keys/) and the pool key in the Keychain item
# keeler-devnet-<name>-pool, named by lend.pool_key. The keeper key is
# lend.key when set (it must be one of the deployed keepers), else the
# dealer's; it is read here only to find its address.
#
# With a lend section the script also writes LEND_KEEPERS (the dealer's
# keeper) whenever its URL changes. --standby also runs the second keeper of
# the set as a standby (`keeler keeper`, key in the Keychain item
# keeler-devnet-<name>-standby) on the next port behind its own tunnel, and
# adds it to LEND_KEEPERS, after the dealer's keeper, once its URL is known.
# DEALERS is written as soon as the dealer's URL is known.
#
# Pause quoting without stopping settlement: touch the config's pause_file;
# pause new lending (both keepers): touch lend.pause_file.
set -euo pipefail

redact() { perl -pe 'BEGIN { $| = 1 } s/api-key=[^&"\s]+/api-key=REDACTED/g'; }
exec > >(redact) 2> >(redact >&2)

standby=false
if [[ ${1:-} == --standby ]]; then standby=true; shift; fi
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
# deploy key), the keeper set (the dealer and the standby), the pool viewing
# key and vault 1's mints (the swap deploy's tSOL and tUSD). That binary gets
# its own cargo target, so the swap-only build in target/release is left alone.
lend_program=$(field 'c.get("lend", {}).get("program_id", "")')
standby_key=""
keeper=$dealer
if [[ -n $lend_program ]]; then
    [[ $lend_program =~ ^[1-9A-HJ-NP-Za-km-z]{32,44}$ ]] || { echo "$cfg: lend.program_id '$lend_program' is not a base58 address" >&2; exit 1; }
    if [[ $(field 'c["lend"].get("key", "")') != "" ]]; then
        # The address of lend.key: the last 32 bytes of the keypair, in base58.
        # Only the address leaves Python.
        keeper=$(python3 - "$cfg" 2>/dev/null <<'EOF'
import json, subprocess, sys
src = json.load(open(sys.argv[1]))["lend"]["key"]
if "keychain" in src:
    text = subprocess.run(["security", "find-generic-password", "-s", src["keychain"], "-w"],
                          check=True, capture_output=True, text=True).stdout
else:
    text = open(src["file"]).read()
key = bytes(json.loads(text))
if len(key) != 64:
    sys.exit(1)
pub = key[32:]
n, out = int.from_bytes(pub, "big"), ""
alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
while n:
    n, r = divmod(n, 58)
    out = alphabet[r] + out
print("1" * (len(pub) - len(pub.lstrip(b"\0"))) + out)
EOF
        ) || { echo "$cfg: lend.key cannot be read as a keypair" >&2; exit 1; }
    fi
    deployment=""
    for d in "$root"/target/devnet-*/; do
        if [[ -f $d/program-id && $(cat "$d/program-id") == "$lend_program" ]]; then deployment=${d%/}; fi
    done
    [[ -n $deployment ]] || { echo "no target/devnet-*/program-id holds $lend_program: deploy it with scripts/deploy-lend-devnet.sh first" >&2; exit 1; }
    name=${deployment##*/devnet-}
    pool_item=keeler-devnet-$name-pool
    [[ $(field 'c["lend"].get("pool_key", {}).get("keychain", "")') == "$pool_item" ]] ||
        { echo "$cfg: lend.pool_key must be {\"keychain\": \"$pool_item\"}" >&2; exit 1; }
    # The values the deployed program was built with, as deploy-lend-devnet.sh
    # recorded them: re-deriving them here could pick up a Keychain item or a
    # swap deploy changed since.
    params=$deployment/build-params.json
    [[ -f $params ]] || { echo "no $params: rerun scripts/deploy-lend-devnet.sh --estimate to record it" >&2; exit 1; }
    param() { python3 -c 'import json, sys; v = json.load(open(sys.argv[1]))[sys.argv[2]]; print(",".join(v) if isinstance(v, list) else v)' "$params" "$1"; }
    [[ $(param program_id) == "$lend_program" ]] || { echo "$params is for another program than $lend_program" >&2; exit 1; }
    keepers=$(param keepers) || { echo "$params has no keepers: rerun scripts/deploy-lend-devnet.sh --estimate" >&2; exit 1; }
    pool_viewing=$(param pool_viewing) || { echo "$params has no pool_viewing" >&2; exit 1; }
    [[ ,$keepers, == *,"$keeper",* ]] || { echo "$params: the keeper $keeper is not one of the lend program's keepers ($keepers)" >&2; exit 1; }
    if $standby; then
        others=$(/usr/bin/tr ',' '\n' <<<"$keepers" | grep -vx "$keeper" || true)
        [[ -n $others && $others != *$'\n'* ]] || { echo "--standby needs exactly one keeper besides $keeper in $params ($keepers)" >&2; exit 1; }
        standby_key=$others
    fi
    # Older records lack the oracle; the build default (Pyth's SOL/USD) then holds.
    opt() { python3 -c 'import json, sys; print(json.load(open(sys.argv[1])).get(sys.argv[2], ""))' "$params" "$1"; }
    price_account=$(opt price_account)
    feed_id=$(opt feed_id)
    [[ -d $deployment/keys ]] || { echo "no $deployment/keys: rerun scripts/deploy-lend-devnet.sh --estimate" >&2; exit 1; }
    # The keeper proves with the deployment's own copy of the keys, so a
    # rebuild of lend/target under a running keeper cannot change them.
    export KEELER_LEND_KEYS_DIR=$deployment/keys
    (
        export KEELER_LEND_PROGRAM_ID=$lend_program KEELER_LEND_ADMIN
        export KEELER_LEND_KEEPERS=$keepers KEELER_LEND_POOL_VIEWING=$pool_viewing
        export KEELER_LEND_VAULT1_COLLATERAL_MINT KEELER_LEND_VAULT1_DEBT_MINT
        KEELER_LEND_ADMIN=$(param admin)
        KEELER_LEND_VAULT1_COLLATERAL_MINT=$(param collateral_mint)
        KEELER_LEND_VAULT1_DEBT_MINT=$(param debt_mint)
        if [[ -n $price_account ]]; then export KEELER_LEND_VAULT1_PRICE_ACCOUNT=$price_account; fi
        if [[ -n $feed_id ]]; then export KEELER_LEND_VAULT1_FEED_ID=$feed_id; fi
        CARGO_TARGET_DIR=$deployment/dealer-cargo \
            cargo build --release --quiet --manifest-path "$root/Cargo.toml" -p keeler --bin keeler
    )
    bin=$deployment/dealer-cargo/release/keeler
else
    $standby && { echo "--standby needs a config with a lend section" >&2; exit 1; }
    cargo build --release --quiet --manifest-path "$root/Cargo.toml" -p keeler --bin keeler
    bin=$target/release/keeler
fi

# The standby: the dealer config's network and HTTP key, its lend section as
# a standby under the standby key (its own default state file, the same pause
# file), on the next port.
if [[ -n $standby_key ]]; then
    standby_cfg=$logs/standby-keeper.json
    standby_http=$(python3 - "$cfg" "$standby_cfg" "keeler-devnet-$name-standby" <<'EOF'
import json, os, sys
c = json.load(open(sys.argv[1]))
host, port = c["http"]["listen"].rsplit(":", 1)
listen = f"{host}:{int(port) + 1}"
lend = dict(c["lend"], role="standby")
# Its own key and state; the pool key and the pause file are shared.
lend.pop("state_file", None)
lend.pop("key", None)
out = {
    "network": c["network"],
    "key": {"keychain": sys.argv[3]},
    "http": {"listen": listen, "key": c["http"]["key"]},
    "lend": lend,
}
fd = os.open(sys.argv[2], os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
with os.fdopen(fd, "w") as f:
    json.dump(out, f, indent=2)
print(listen)
EOF
    )
fi

# Each process: start_<n> sets pid_<n> and since_<n>; check <n> restarts it
# with backoff when it has exited.
procs="dealer tunnel"
[[ -n $standby_key ]] && procs="$procs standby standby_tunnel"
cleanup() {
    for n in $procs; do
        local v=pid_$n
        [[ -n ${!v:-} ]] && kill "${!v}" 2>/dev/null
    done
    true
}
trap cleanup EXIT

# shellcheck disable=SC2034 # pid_<n> and since_<n> are read by name in check.
{
start_dealer() {
    # The dealer redacts its own logs; this catches a dependency that does not.
    "$bin" dealer "$cfg" > >(redact >>"$logs/dealer.log") 2>&1 &
    pid_dealer=$! since_dealer=$SECONDS
}
start_standby() {
    "$bin" keeper "$standby_cfg" > >(redact >>"$logs/standby-keeper.log") 2>&1 &
    pid_standby=$! since_standby=$SECONDS
}
# Fresh log: the URL grepped from it must be this tunnel's.
start_tunnel() {
    : >"$logs/cloudflared.log"
    cloudflared tunnel --no-autoupdate --url "http://$http" >"$logs/cloudflared.log" 2>&1 &
    pid_tunnel=$! since_tunnel=$SECONDS
}
start_standby_tunnel() {
    : >"$logs/cloudflared-standby.log"
    cloudflared tunnel --no-autoupdate --url "http://$standby_http" >"$logs/cloudflared-standby.log" 2>&1 &
    pid_standby_tunnel=$! since_standby_tunnel=$SECONDS
}
}
# Doubles up to 60 s; a process that stayed up a minute starts again at 1 s.
next_delay() { if (($2 >= 60)); then echo 1; else echo $(($1 * 2 > 60 ? 60 : $1 * 2)); fi; }
check() {
    local pid=pid_$1 since=since_$1 delay=delay_$1 d
    kill -0 "${!pid}" 2>/dev/null && return
    wait "${!pid}" || true
    d=$(next_delay "${!delay:-1}" $((SECONDS - ${!since})))
    printf -v "$delay" %s "$d"
    echo "$1 exited; restart in ${d}s" >&2
    sleep "$d"
    "start_$1"
}
# Quick tunnel hosts are hyphenated words; anything else in the log is not
# this tunnel's URL.
url_in() { grep -oE 'https://[a-z0-9]+(-[a-z0-9]+)+\.trycloudflare\.com' "$1" | head -1 || true; }

# A quick tunnel's URL changes on every start, so the URLs go in as secrets,
# which take effect without redeploying the Worker.
secret() { (cd "$root/worker" && npx wrangler secret put "$1"); }
# DEALERS: the dealer. LEND_KEEPERS: its keeper, then the standby once its
# URL is known.
dealers_json() { printf '[{"url":"%s","pubkey":"%s"}]' "$1" "$dealer"; }
keepers_json() {
    printf '[{"url":"%s","pubkey":"%s"}' "$1" "$keeper"
    [[ -n $2 ]] && printf ',{"url":"%s","pubkey":"%s"}' "$2" "$standby_key"
    printf ']'
}
# Writes secret $1 as $2 unless it was last written so; failures back off
# to 5 min and leave the record old, so a later pass tries again.
publish_delay=2 publish_at=0
publish() {
    local was=written_$1
    [[ ${!was:-} == "$2" ]] && return 0
    ((SECONDS >= publish_at)) || return 0
    if printf %s "$2" | secret "$1"; then
        printf -v "$was" %s "$2"
        publish_delay=2
        echo "published $1: $2" >&2
    else
        publish_at=$((SECONDS + publish_delay))
        echo "publishing $1 failed; retry in ${publish_delay}s" >&2
        publish_delay=$((publish_delay * 2 > 300 ? 300 : publish_delay * 2))
    fi
}

for n in $procs; do "start_$n"; done
while true; do
    for n in $procs; do check "$n"; done
    main=$(url_in "$logs/cloudflared.log") second=""
    [[ -n $standby_key ]] && second=$(url_in "$logs/cloudflared-standby.log")
    if [[ -n $main ]]; then
        publish DEALERS "$(dealers_json "$main")"
        [[ -n $lend_program ]] && publish LEND_KEEPERS "$(keepers_json "$main" "$second")"
    fi
    sleep 2
done
