#!/usr/bin/env bash
# Deploys Keeler Lend to devnet as deployment <name> (default lend). It
# spends devnet SOL from keeler-devnet-deploy: run with --estimate first,
# which sends nothing and creates no Keychain item. Safe to rerun
# after any failure: the deploy resumes from its buffer and a finished one
# sends nothing.
#
#   scripts/deploy-lend-devnet.sh [--estimate] [name]
#
# Keychain items (Solana keypair JSON arrays):
#   keeler-devnet-deploy          must exist, funded: pays, upgrade authority, admin
#   keeler-devnet-dealer          must exist: the swap dealer, the first keeper
#   keeler-devnet-<name>-standby  optional: a second keeper (a standby)
#   keeler-devnet-<name>-pool     the pool key: the pool viewing key derives from
#                                 it; must be no keeper's key
#   keeler-devnet-<name>-program, keeler-devnet-<name>-buffer
# A real run creates the pool, program and buffer items if absent.
# --estimate never creates one: with any of them missing it builds with
# stand-in values in target/devnet-<name>/estimate/, prints the cost and
# records nothing.
#
# 1. Builds the program once with those addresses compiled in and the
#    test-price feature (the keeper sets the price; Pyth's devnet feeds are
#    not used), and keeps that .so: reruns deploy the same bytes.
# 2. Runs the deploy tool (cli/examples/deploy.rs `program`), which refuses
#    any cluster but devnet and stops before sending if the deploy wallet
#    holds less than the deploy still needs.
# 3. Prints the site build and Worker variables for this deployment.
#
# Writes only target/devnet-<name>/: plan.json, summary.json, program-id,
# keeler_lend.so, build-params.json ({program_id, admin, keepers, pool_viewing,
# collateral_mint, debt_mint, price_account, feed_id}, read by
# scripts/run-dealer.sh), keys/ (the proving keys the keeper reads),
# deploy.log, and the build directories build/ and cargo/.
# Needs: security (macOS), python3, the main checkout's .tools/env, and the
# Helius key in the Keychain item keeler-helius-api-key.
set -euo pipefail

estimate=false
[[ ${1:-} == --estimate ]] && { estimate=true; shift; }
name=${1:-lend}
[[ $name =~ ^[a-z0-9-]{1,32}$ ]] || { echo "deployment name '$name': lowercase letters, digits and -" >&2; exit 1; }

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
out=$root/target/devnet-$name
mkdir -p "$out"
# Nothing here should print the Helius key; this catches it if a tool does.
redact() { perl -pe 'BEGIN { $| = 1 } s/api-key=[^&"\s]+/api-key=REDACTED/g'; }
exec > >(redact) 2> >(redact | tee -a "$out/deploy.log" >&2)

have() { security find-generic-password -s "$1" >/dev/null 2>&1; }
# The secret goes to security on stdin, never on a command line or in a file.
store() { printf 'add-generic-password -s "%s" -a keeler -w "%s"\n' "$1" "$2" | security -i >/dev/null; }
# The public key is bytes 32..64 of the keypair array, read from a pipe.
address_of() {
    security find-generic-password -s "$1" -w | python3 -c '
import json, sys
b = bytes(json.load(sys.stdin))[32:64]
n = int.from_bytes(b, "big")
abc = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
s = ""
while n:
    n, r = divmod(n, 58)
    s = abc[r] + s
print("1" * (len(b) - len(b.lstrip(b"\0"))) + s)'
}
is_address() { [[ $1 =~ ^[1-9A-HJ-NP-Za-km-z]{32,44}$ ]]; }

swap=$root/target/devnet/deploy-summary.json
[[ -f $swap ]] || { echo "no $swap: the swap deploy (scripts/deploy-devnet.sh) gives the dealer and the mints" >&2; exit 1; }
summary_field() { python3 -c 'import json, sys; s = json.load(open(sys.argv[1])); print(eval(sys.argv[2], {}, {"s": s}))' "$swap" "$1"; }
dealer=$(summary_field 's["dealer"]')
collateral=$(summary_field '{m: a for m, a, _ in s["mints"]}["tSOL"]')
debt=$(summary_field '{m: a for m, a, _ in s["mints"]}["tUSD"]')
for item in deploy dealer; do
    have "keeler-devnet-$item" || { echo "no Keychain item keeler-devnet-$item" >&2; exit 1; }
done
admin=$(address_of keeler-devnet-deploy)
# The first keeper must be the key the dealer runs with, or its keeper
# instructions fail on chain.
[[ $(address_of keeler-devnet-dealer) == "$dealer" ]] || { echo "keeler-devnet-dealer is not the dealer $dealer in $swap" >&2; exit 1; }
keepers=$dealer
if have "keeler-devnet-$name-standby"; then
    standby=$(address_of "keeler-devnet-$name-standby")
    [[ $standby != "$dealer" ]] || { echo "keeler-devnet-$name-standby is the dealer's key" >&2; exit 1; }
    keepers=$dealer,$standby
fi

# shellcheck source=/dev/null
. "$root/.tools/env"
tool_target=$out/cargo
tool=$tool_target/release/examples/deploy
keeler=$tool_target/release/keeler
# Its own target, so the live dealer's target/release is left alone.
CARGO_TARGET_DIR=$tool_target cargo build --release --quiet --manifest-path "$root/Cargo.toml" -p keeler --bin keeler --example deploy

missing=()
for item in pool program buffer; do
    have "keeler-devnet-$name-$item" || missing+=("keeler-devnet-$name-$item")
done
if $estimate && ((${#missing[@]})); then
    # The size, and so the cost, does not depend on the addresses compiled in.
    echo "estimate only: ${missing[*]} absent (a real run creates them); building with stand-in addresses, recording nothing" >&2
    est=$out/estimate
    KEELER_LEND_ADMIN=$admin KEELER_LEND_KEEPERS=$keepers \
        KEELER_LEND_VAULT1_COLLATERAL_MINT=$collateral KEELER_LEND_VAULT1_DEBT_MINT=$debt \
        KEELER_LEND_OUT=$est/build "$root/scripts/setup-lend.sh"
    python3 - "$est/plan.json" "$est/build/deploy/keeler_lend.so" <<'EOF'
import json, sys
plan = {
    "name": "estimate",
    "network": {
        "rpc_url": "https://devnet.helius-rpc.com",
        "indexer_url": "https://devnet.helius-rpc.com/v1/spr",
        "prover": {"helius": {"keychain": "keeler-helius-api-key"}},
    },
    "genesis_hash": "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG",
    "deploy_key": {"keychain": "keeler-devnet-deploy"},
    "program_key": {"keychain": "unused-by-estimate"},
    "buffer_key": {"keychain": "unused-by-estimate"},
    "program_so": sys.argv[2],
}
open(sys.argv[1], "w").write(json.dumps(plan, indent=2) + "\n")
EOF
    "$tool" estimate "$est/plan.json"
    exit 0
fi

# bash 3.2 calls an empty "${missing[@]}" unbound under set -u.
for item in ${missing[@]+"${missing[@]}"}; do
    json=$("$tool" keygen)
    # An empty or broken item would block every rerun.
    [[ $json =~ ^\[([0-9]{1,3},){63}[0-9]{1,3}\]$ ]] || { echo "keygen gave no keypair" >&2; exit 1; }
    store "$item" "$json"
    echo "created Keychain item $item" >&2
done
program_id=$(address_of "keeler-devnet-$name-program")
pool=$(address_of "keeler-devnet-$name-pool")
# Whoever holds the pool key reads every amount; it is a key of its own so
# that a keeper can be removed without handing it the next pool key.
[[ ,$keepers, != *",$pool,"* ]] || { echo "keeler-devnet-$name-pool is a keeper's key; it must be its own" >&2; exit 1; }
[[ $pool != "$admin" ]] || { echo "keeler-devnet-$name-pool is the deploy key" >&2; exit 1; }
pool_viewing=$("$keeler" deploy viewing-key "keeler-devnet-$name-pool")
[[ $pool_viewing =~ ^04[0-9a-f]{64}$ ]] || { echo "no viewing key for keeler-devnet-$name-pool" >&2; exit 1; }
for a in "$program_id" "$admin" "$collateral" "$debt" ${keepers//,/ }; do
    is_address "$a" || { echo "'$a' is not a base58 address" >&2; exit 1; }
done
if [[ -f $out/program-id && $(cat "$out/program-id") != "$program_id" ]]; then
    echo "$out was built for $(cat "$out/program-id"), the Keychain holds $program_id; use another deployment name" >&2
    exit 1
fi
# The Pyth price account and feed stay at their defaults, unused on devnet
# (the test price account is passed), but recorded explicitly.
price_account=7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE
feed_id=ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d

so=$out/keeler_lend.so
if [[ ! -f $so ]]; then
    # The devnet price comes from the keeper's test price account, so the
    # build must have test-price (setup-lend.sh passes it; this catches a change).
    grep -q 'bpf-entrypoint,test-price' "$root/scripts/setup-lend.sh" ||
        { echo "scripts/setup-lend.sh no longer builds with test-price" >&2; exit 1; }
    before=$(git -C "$root" status --porcelain --untracked-files=no)
    KEELER_LEND_PROGRAM_ID=$program_id KEELER_LEND_ADMIN=$admin KEELER_LEND_KEEPERS=$keepers \
        KEELER_LEND_POOL_VIEWING=$pool_viewing \
        KEELER_LEND_VAULT1_COLLATERAL_MINT=$collateral KEELER_LEND_VAULT1_DEBT_MINT=$debt \
        KEELER_LEND_VAULT1_PRICE_ACCOUNT=$price_account KEELER_LEND_VAULT1_FEED_ID=$feed_id \
        KEELER_LEND_OUT=$out/build "$root/scripts/setup-lend.sh"
    [[ $(git -C "$root" status --porcelain --untracked-files=no) == "$before" ]] ||
        { echo "the build changed tracked files; nothing deployed" >&2; exit 1; }
    cp "$out/build/deploy/keeler_lend.so" "$so.tmp"
    mv "$so.tmp" "$so"
    printf '%s\n' "$program_id" >"$out/program-id"
fi

# The build parameters the kept .so was built with, recorded for
# scripts/run-dealer.sh (a keeper built with other values fails every
# instruction) and checked on each rerun: the Keychain or the swap deploy
# may have changed since the .so was built.
python3 - "$out/build-params.json" "$program_id" "$admin" "$keepers" "$pool_viewing" \
    "$collateral" "$debt" "$price_account" "$feed_id" <<'EOF'
import json, os, sys
path, program_id, admin, keepers, pool_viewing, collateral, debt, price, feed = sys.argv[1:]
params = {
    "program_id": program_id,
    "admin": admin,
    "keepers": keepers.split(","),
    "pool_viewing": pool_viewing,
    "collateral_mint": collateral,
    "debt_mint": debt,
    "price_account": price,
    "feed_id": feed,
}
if os.path.exists(path) and json.load(open(path)) != params:
    sys.exit(f"{path} records other build parameters than these; use another deployment name")
open(path + ".tmp", "w").write(json.dumps(params, indent=2) + "\n")
os.replace(path + ".tmp", path)
EOF

# The keeper's proving keys, kept with the deployment: setup-lend.sh rebuilds
# lend/target in place, under a running keeper.
keys=$out/keys
if [[ ! -d $keys ]]; then
    src=$root/lend/target/zk/keeler-lend
    (cd "$src" && shasum -a 256 -c --quiet "$root/lend/lend-keys.CHECKSUM") ||
        { echo "$src does not match lend/lend-keys.CHECKSUM: run scripts/setup-lend.sh" >&2; exit 1; }
    rm -rf "$keys.tmp"
    while read -r _ file; do
        mkdir -p "$keys.tmp/$(dirname "$file")"
        cp "$src/$file" "$keys.tmp/$file"
    done <"$root/lend/lend-keys.CHECKSUM"
    mv "$keys.tmp" "$keys"
fi
(cd "$keys" && shasum -a 256 -c --quiet "$root/lend/lend-keys.CHECKSUM") ||
    { echo "$keys does not match lend/lend-keys.CHECKSUM" >&2; exit 1; }

python3 - "$out/plan.json" "$name" "$so" <<'EOF'
import json, sys
plan = {
    "name": sys.argv[2],
    "network": {
        "rpc_url": "https://devnet.helius-rpc.com",
        "indexer_url": "https://devnet.helius-rpc.com/v1/spr",
        "prover": {"helius": {"keychain": "keeler-helius-api-key"}},
    },
    "genesis_hash": "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG",
    "deploy_key": {"keychain": "keeler-devnet-deploy"},
    "program_key": {"keychain": f"keeler-devnet-{sys.argv[2]}-program"},
    "buffer_key": {"keychain": f"keeler-devnet-{sys.argv[2]}-buffer"},
    "program_so": sys.argv[3],
}
open(sys.argv[1], "w").write(json.dumps(plan, indent=2) + "\n")
EOF

echo "$name: program $program_id, admin $admin, keepers $keepers, pool viewing $pool_viewing, tSOL $collateral, tUSD $debt" >&2
if $estimate; then
    "$tool" estimate "$out/plan.json"
else
    "$tool" program "$out/plan.json" >"$out/summary.json.tmp"
    mv "$out/summary.json.tmp" "$out/summary.json"
    echo "wrote $out/summary.json; next: docs/lend-deploy.md, after the deploy" >&2
fi

# What the site build and the Worker need (docs/lend-deploy.md, worker/README.md).
python3 - "$out/build-params.json" "$swap" <<'EOF' >&2
import json, sys
p = json.load(open(sys.argv[1]))
tokens = {sym: (mint, decimals) for sym, mint, decimals in json.load(open(sys.argv[2]))["mints"]}
assets = json.dumps([{"mint": tokens[sym][0], "symbol": sym, "decimals": tokens[sym][1]} for sym in ("tUSD", "tSOL")],
                    separators=(",", ":"))
keepers = json.dumps(p["keepers"], separators=(",", ":"))
listed = json.dumps([{"url": f"<keeper {i + 1} URL>", "pubkey": k} for i, k in enumerate(p["keepers"])],
                    separators=(",", ":"))
print("site build (web/):")
print(f"  VITE_NETWORK=devnet")
print(f"  VITE_LEND_PROGRAM_ID={p['program_id']}")
print(f"  VITE_LEND_KEEPERS='{keepers}'")
print(f"  VITE_LEND_POOL_VIEWING={p['pool_viewing']}")
print(f"  VITE_ASSETS='{assets}'")
print("Worker:")
print(f"  NETWORK=devnet")
print(f"  LEND_PROGRAM_ID={p['program_id']}")
print(f"  LEND_KEEPERS='{listed}'   (each keeper's tunnel URL; scripts/run-dealer.sh publishes them)")
print("  LEND_POOLS, LEND_VAULTS: as compiled into the program (worker/README.md)")
EOF
