#!/usr/bin/env bash
# Creates or updates the devnet CVM "keeler-devnet" on Phala Cloud from
# deploy/phala/docker-compose.yml: the dealer (with the primary keeper) and
# the standby keeper, each with only its own secrets. Then prints the app id
# and the two public URLs, checks that both answer /health and that the
# container logs are not public.
#
#   scripts/deploy-phala.sh [--publish] [--registry-user <github user>]
#                           [--instance-type <type>]
#
#   --publish        also write the Worker secrets DEALERS and LEND_KEEPERS
#                    with the CVM's URLs (the cut-over; wrangler logged in).
#   --registry-user  pull a private ghcr.io image as this GitHub user, with
#                    the token in the Keychain item keeler-phala-ghcr-token
#                    (read:packages).
#   --instance-type  the CVM size, on create and on update (default tdx.medium).
#
# The cut-over, in order: pause the Worker, wait until the open tickets and
# quotes have run out, stop the Mac's `keeler dealer` and `keeler keeper`
# (the script refuses to run while either does), deploy, check, then publish
# (--publish). The CVM starts from empty volumes: the Mac's tickets, builds
# in flight and quote book do not carry over.
#
# Handed to the CVM, by Keychain item: the dealer key (also the primary
# keeper's), the standby keeper key, the pool key, the HTTP key, the faucet
# key and the Helius key keeler-helius-api-key, the same item the mainnet
# runbook names. Never handed over: the admin and deploy key, the program
# and buffer keys. Every secret is read from the Keychain here and reaches
# the phala CLI through a pipe (-e /dev/fd/3): never an argument, never a
# file. The Phala API key (Keychain item keeler-phala-api-key) goes into the
# environment of each phala call only. No Terraform: its state would hold the secrets in
# clear. Needs Node and `npm ci --ignore-scripts` in deploy/phala (the pinned CLI).
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
phala_dir=$root/deploy/phala
compose=$phala_dir/docker-compose.yml
phala=$phala_dir/node_modules/.bin/phala
name=keeler-devnet
instance_type=tdx.medium
disk_size=20G
os_image=dstack-0.6.5
publish=false
registry_user=""
while [[ $# -gt 0 ]]; do
    case $1 in
        --publish) publish=true; shift ;;
        --registry-user) registry_user=${2:?--registry-user needs a GitHub user}; shift 2 ;;
        --instance-type) instance_type=${2:?--instance-type needs a type}; shift 2 ;;
        *) echo "unknown argument $1" >&2; exit 1 ;;
    esac
done

if pgrep -f '(^|/)keeler (dealer|keeper) ' >/dev/null; then
    echo "a keeler dealer or keeper runs on this machine: stop it first (two keepers on one key race)" >&2
    exit 1
fi
[[ -x $phala ]] || { echo "no $phala: run (cd deploy/phala && npm ci --ignore-scripts)" >&2; exit 1; }
image_lines=$(grep -E '^[[:space:]]+image:' "$compose")
if grep -vqE 'image: ghcr\.io/0xinbeta/keeler-keeper@sha256:[0-9a-f]{64}$' <<<"$image_lines" ||
    grep -q 'sha256:0\{64\}' <<<"$image_lines"; then
    echo "$compose: set both images to the digest image.yml printed" >&2
    exit 1
fi

keychain() {
    security find-generic-password -s "$1" -w 2>/dev/null ||
        { echo "no Keychain item named $1" >&2; return 1; }
}

secrets=(
    KEELER_DEALER_KEY=keeler-devnet-dealer
    KEELER_STANDBY_KEY=keeler-devnet-lend2-standby
    KEELER_POOL_KEY=keeler-devnet-lend2-pool
    KEELER_HTTP_KEY=keeler-devnet-dealer-http-key
    KEELER_FAUCET_KEY=keeler-devnet-faucet
    KEELER_HELIUS_KEY=keeler-helius-api-key
)
if [[ -n $registry_user ]]; then
    secrets+=(DSTACK_DOCKER_PASSWORD=keeler-phala-ghcr-token)
fi
env_text="" http_key=""
for entry in "${secrets[@]}"; do
    var=${entry%%=*} item=${entry#*=}
    secret_value=$(keychain "$item")
    [[ -n $secret_value ]] || { echo "Keychain item $item is empty" >&2; exit 1; }
    case $secret_value in
        *$'\n'* | *$'\r'* | *"'"* | *'"'* | *'$'* | *\\*)
            echo "Keychain item $item holds a newline, a quote, \$ or a backslash, which the env file does not carry as is" >&2
            exit 1
            ;;
    esac
    env_text+=$(printf "%s='%s'" "$var" "$secret_value")$'\n'
    [[ $var != KEELER_HTTP_KEY ]] || http_key=$secret_value
done
unset secret_value
if [[ -n $registry_user ]]; then
    env_text+=$(printf "DSTACK_DOCKER_USERNAME='%s'\nDSTACK_DOCKER_REGISTRY='ghcr.io'" "$registry_user")$'\n'
fi
env_file() { printf '%s' "$env_text"; }

phala_api_key=$(keychain keeler-phala-api-key)
run_phala() { (cd "$phala_dir" && PHALA_CLOUD_API_KEY=$phala_api_key "$phala" "$@"); }

phala_json() {
    local out
    out=$(run_phala "$@" --json) || { echo "phala $* failed: $out" >&2; return 1; }
    printf '%s' "$out"
}
json_field() { python3 -c 'import json, sys; d = json.load(sys.stdin); d = d.get("data", d) if isinstance(d, dict) else d; print(eval(sys.argv[1], {}, {"d": d}))' "$1"; }

images=$(phala_json os-images)
grep -q "\"$os_image\"" <<<"$images" ||
    { echo "Phala Cloud does not list the OS image $os_image (phala os-images)" >&2; exit 1; }

cvms=$(phala_json cvms list --page-size 100)
existing=$(json_field '[c["cvmName"] for c in d["items"]].count("'"$name"'") if isinstance(d, dict) and isinstance(d.get("items"), list) and d.get("totalPages", 1) <= 1 and all(isinstance(c, dict) and isinstance(c.get("cvmName"), str) for c in d["items"]) else "unreadable"' <<<"$cvms")
if [[ $existing == 0 ]]; then
    run_phala deploy -n "$name" -c "$compose" -t "$instance_type" --disk-size "$disk_size" \
        --image "$os_image" --kms phala --no-public-logs --no-public-sysinfo --wait --json \
        -e /dev/fd/3 3< <(env_file)
elif [[ $existing == 1 ]]; then
    run_phala deploy --cvm-id "$name" -c "$compose" -t "$instance_type" \
        --no-public-logs --no-public-sysinfo --wait --json -e /dev/fd/3 3< <(env_file)
elif [[ $existing == unreadable ]]; then
    echo "phala cvms list did not give one page of items, each with a cvmName" >&2
    exit 1
else
    echo "$existing CVMs are named $name; resolve that in the dashboard first" >&2
    exit 1
fi

cvm=$(phala_json cvms get "$name")
app_id=$(json_field 'd["app_id"]' <<<"$cvm")
domain=$(json_field 'd["gateway"]["base_domain"]' <<<"$cvm")
[[ $app_id =~ ^[0-9a-f]+$ && $domain =~ ^[a-z0-9.-]+$ ]] ||
    { echo "unexpected app id or gateway domain in phala cvms get" >&2; exit 1; }
[[ $(json_field 'd.get("public_logs")' <<<"$cvm") == False ]] ||
    { echo "the CVM's public_logs is not off" >&2; exit 1; }
[[ $(json_field 'd.get("public_sysinfo")' <<<"$cvm") == False ]] ||
    { echo "the CVM's public_sysinfo is not off" >&2; exit 1; }
dealer_url=https://$app_id-47500.$domain
standby_url=https://$app_id-47501.$domain
echo "app id: $app_id"
echo "compose hash: $(json_field 'd.get("compose_hash")' <<<"$cvm")"
echo "dealer and primary keeper: $dealer_url"
echo "standby keeper: $standby_url"

for service in dealer standby; do
    status=$(curl -s -o /dev/null -w '%{http_code}' "https://$app_id-8090.$domain/logs/$service" || true)
    [[ $status != 2* ]] || { echo "the $service logs answer $status without a key" >&2; exit 1; }
done
for url in "$dealer_url" "$standby_url"; do
    for _ in $(seq 60); do
        if curl -sf -H @<(printf 'x-keeler-key: %s\n' "$http_key") "$url/health" >/dev/null; then
            echo "$url/health answers"
            continue 2
        fi
        sleep 10
    done
    echo "$url/health does not answer after 10 minutes" >&2
    exit 1
done

$publish || exit 0
dealer_pubkey=$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["pubkey"])' "$root/deploy/devnet/dealer.json")
standby_pubkey=$(python3 -c 'import json, sys; p = json.load(open(sys.argv[1])); print([k for k in p["keepers"] if k != sys.argv[2]][0])' "$root/deploy/devnet/build-params.json" "$dealer_pubkey")
secret() { (cd "$root/worker" && npx wrangler secret put "$1"); }
printf '[{"url":"%s","pubkey":"%s"}]' "$dealer_url" "$dealer_pubkey" | secret DEALERS
printf '[{"url":"%s","pubkey":"%s"},{"url":"%s","pubkey":"%s"}]' \
    "$dealer_url" "$dealer_pubkey" "$standby_url" "$standby_pubkey" | secret LEND_KEEPERS
echo "published DEALERS and LEND_KEEPERS"
