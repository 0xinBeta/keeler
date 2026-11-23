#!/usr/bin/env bash
# Hands a deployed program's upgrade authority to another account (a
# multisig, say). NOT TO BE RUN without the owner: it cannot be undone by
# this key once done.
#
#   scripts/set-upgrade-authority.sh <program> <new-authority>
#
# KEELER_CLUSTER=devnet (default) or mainnet picks the Helius RPC; the current
# authority is the Keychain item keeler-<cluster>-deploy.
set -euo pipefail

[[ $# -eq 2 ]] || { echo "usage: $0 <program> <new-authority>" >&2; exit 1; }
program=$1 new=$2
cluster=${KEELER_CLUSTER:-devnet}
case $cluster in
    devnet) host=devnet.helius-rpc.com ;;
    mainnet) host=mainnet.helius-rpc.com ;;
    *) echo "KEELER_CLUSTER is devnet or mainnet" >&2; exit 1 ;;
esac
for a in "$program" "$new"; do
    [[ $a =~ ^[1-9A-HJ-NP-Za-km-z]{32,44}$ ]] || { echo "$a is not a base58 address" >&2; exit 1; }
done

key() { security find-generic-password -s "$1" -w; }
helius=$(key keeler-helius-api-key)
if [[ $helius == https://* ]]; then
    helius=$(sed -nE 's/.*[?&]api-key=([A-Za-z0-9-]+).*/\1/p' <<<"$helius")
fi
[[ $helius =~ ^[A-Za-z0-9-]+$ ]] || { echo "keeler-helius-api-key holds no usable key" >&2; exit 1; }
# Holds the key: passed as an argument only, never echoed.
rpc="https://$host/?api-key=$helius"
unset helius
sol() { solana -u "$rpc" "$@"; }

# A typo'd authority would lock the program for good.
sol account "$new" >/dev/null 2>&1 || { echo "$new does not exist on $cluster; refusing" >&2; exit 1; }
sol program set-upgrade-authority "$program" \
    --upgrade-authority <(key "keeler-$cluster-deploy") \
    --new-upgrade-authority "$new" --skip-new-upgrade-authority-signer-check
got=$(sol program show "$program" | awk '/^Authority:/ {print $2}')
[[ $got == "$new" ]] || { echo "upgrade authority reads back as '$got', not $new" >&2; exit 1; }
echo "$program upgrade authority is now $new on $cluster" >&2
