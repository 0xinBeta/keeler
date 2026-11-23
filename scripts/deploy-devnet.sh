#!/usr/bin/env bash
# Deploys Keeler's build of the swap example to devnet and sets up the dealer.
# NOT TO BE RUN until the owner approves publishing the example's code (see
# the mainnet gates in docs/plan-web.md). Safe to rerun after any failure:
# every step checks what is already on chain and resumes.
#
# 1. Creates these Keychain items if absent (Solana keypair JSON arrays, plus
#    a random HTTP key): keeler-devnet-program, keeler-devnet-buffer,
#    keeler-devnet-dealer, keeler-devnet-faucet, keeler-devnet-tusd-mint,
#    keeler-devnet-tsol-mint, keeler-devnet-dealer-http-key. The funded
#    keeler-devnet-deploy must already exist.
# 2. Rebuilds the swap example with our program ID (scripts/setup.sh; this
#    changes the shared .tools build, see the restore line it prints).
# 3. Runs the deploy tool (cli/examples/deploy.rs), which reads every key
#    from the Keychain itself, refuses any cluster whose genesis is not
#    devnet's, funds the dealer and faucet, deploys through a resumable
#    buffer, creates tUSD and tSOL and their pool interfaces, and deposits
#    the dealer's inventory.
# 4. Writes target/devnet/dealer-config.json, which names only Keychain items.
#
# Needs: security (macOS), openssl, python3, and the Helius key in the
# Keychain item keeler-helius-api-key (a full Helius URL or a bare key).
set -euo pipefail

# Nothing here should print the Helius key; this catches it if a tool does.
redact() { perl -pe 'BEGIN { $| = 1 } s/api-key=[^&"\s]+/api-key=REDACTED/g'; }
exec > >(redact) 2> >(redact >&2)

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
target=${CARGO_TARGET_DIR:-$root/target}
out=$root/target/devnet
mkdir -p "$out"
example_id=F4hjig5RJbXW6vrgYaMcuWzR55kZF6tbKyf8c36nrtvU

key() { security find-generic-password -s "$1" -w; }
have() { security find-generic-password -s "$1" >/dev/null 2>&1; }
# The secret goes to security on stdin, never on a command line or in a file.
store() { printf 'add-generic-password -s "%s" -a keeler -w "%s"\n' "$1" "$2" | security -i >/dev/null; }
build() { cargo build --release --quiet --manifest-path "$root/Cargo.toml" -p keeler --bin keeler --example deploy; }
tool=$target/release/examples/deploy

have keeler-devnet-deploy || { echo "no Keychain item keeler-devnet-deploy; fund one and rerun" >&2; exit 1; }
# shellcheck source=/dev/null
. "$root/.tools/env"
build
for item in program buffer dealer faucet tusd-mint tsol-mint; do
    if ! have "keeler-devnet-$item"; then
        json=$("$tool" keygen)
        # An empty or broken item would block every rerun.
        [[ $json =~ ^\[([0-9]{1,3},){63}[0-9]{1,3}\]$ ]] || { echo "keygen gave no keypair" >&2; exit 1; }
        store "keeler-devnet-$item" "$json"
        echo "created Keychain item keeler-devnet-$item" >&2
    fi
done
if ! have keeler-devnet-dealer-http-key; then
    http_key=$(openssl rand -hex 24)
    [[ $http_key =~ ^[0-9a-f]{48}$ ]] || { echo "openssl gave no HTTP key" >&2; exit 1; }
    store keeler-devnet-dealer-http-key "$http_key"
fi

# The public key is bytes 32..64 of the keypair array, read from a pipe.
program_id=$(key keeler-devnet-program | python3 -c '
import json, sys
b = bytes(json.load(sys.stdin))[32:64]
n = int.from_bytes(b, "big")
abc = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
s = ""
while n:
    n, r = divmod(n, 58)
    s = abc[r] + s
print("1" * (len(b) - len(b.lstrip(b"\0"))) + s)')
[[ $program_id =~ ^[1-9A-HJ-NP-Za-km-z]{32,44}$ ]] || { echo "keeler-devnet-program is not a keypair" >&2; exit 1; }

KEELER_SWAP_PROGRAM_ID=$program_id "$root/scripts/setup.sh"
# shellcheck source=/dev/null
. "$root/.tools/env"
build

# Deploy the first build's bytes on every rerun: a rebuild that is not
# byte-identical would otherwise fail the deployed-bytes check.
[[ -f $out/swap_program.so ]] || cp "$KEELER_SWAP_PROGRAM_SO" "$out/swap_program.so"
python3 - "$out/deploy-plan.json" "$out/swap_program.so" <<'EOF'
import json, sys
k = lambda name: {"keychain": f"keeler-devnet-{name}"}
plan = {
    "network": {
        "rpc_url": "https://devnet.helius-rpc.com",
        "indexer_url": "https://devnet.helius-rpc.com/v1/spr",
        "prover": {"helius": {"keychain": "keeler-helius-api-key"}},
    },
    "genesis_hash": "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG",
    "deploy_key": k("deploy"),
    "program_key": k("program"),
    "buffer_key": k("buffer"),
    "program_so": sys.argv[2],
    "dealer_key": k("dealer"),
    "faucet_key": k("faucet"),
    "dealer_lamports": 500_000_000,
    "faucet_lamports": 2_500_000_000,
    "tokens": [
        {"symbol": "tUSD", "decimals": 6, "key": k("tusd-mint"), "inventory": 1_000_000_000_000},
        {"symbol": "tSOL", "decimals": 9, "key": k("tsol-mint"), "inventory": 10_000_000_000_000},
    ],
}
open(sys.argv[1], "w").write(json.dumps(plan, indent=2) + "\n")
EOF

echo "running the deploy tool; about 100 buffer writes, then mints and deposits" >&2
"$tool" setup "$out/deploy-plan.json" >"$out/deploy-summary.json.tmp"
mv "$out/deploy-summary.json.tmp" "$out/deploy-summary.json"

python3 - "$out/deploy-summary.json" "$out" <<'EOF'
import json, sys
s = json.load(open(sys.argv[1]))
out = sys.argv[2]
mint = {sym: m for sym, m, _ in s["mints"]}
tusd, tsol = mint["tUSD"], mint["tSOL"]
cfg = {
    "network": {
        "rpc_url": "https://devnet.helius-rpc.com",
        "indexer_url": "https://devnet.helius-rpc.com/v1/spr",
        "prover": {"helius": {"keychain": "keeler-helius-api-key"}},
    },
    "key": {"keychain": "keeler-devnet-dealer"},
    "program_id": s["program_id"],
    "pubkey": s["dealer"],
    "assets": [
        {"mint": tusd, "price": {"price": 1, "expo": 0}},
        {"mint": tsol, "price": {"price": 150, "expo": 0}},
    ],
    "listen": "127.0.0.1:47001",
    "spread_bps": 10,
    "quote_ttl_secs": 300,
    "settle_margin_secs": 60,
    "http": {"listen": "127.0.0.1:47500", "key": {"keychain": "keeler-devnet-dealer-http-key"}},
    "faucet": {
        "key": {"keychain": "keeler-devnet-faucet"},
        "tokens": [{"mint": tusd, "amount": 1_000_000_000}, {"mint": tsol, "amount": 10_000_000_000}],
        "lamports": 10_000_000,
        "per_wallet_daily": 1,
        "daily_cap": 60,
    },
    "pause_file": f"{out}/PAUSE",
    "settle_tolerance_bps": 0,
    "limits": [
        {"mint": tusd, "max_sell_amount": 10_000_000_000, "hourly_sell_cap": 100_000_000_000},
        {"mint": tsol, "max_sell_amount": 100_000_000_000, "hourly_sell_cap": 1_000_000_000_000},
    ],
}
open(f"{out}/dealer-config.json", "w").write(json.dumps(cfg, indent=2) + "\n")
print(f"programId {s['program_id']}; tUSD {tusd}; tSOL {tsol}; dealer {s['dealer']}; faucet {s['faucet']}", file=sys.stderr)
EOF
echo "wrote $out/dealer-config.json; next: scripts/run-dealer.sh" >&2
echo "the shared .tools build is now for $program_id; local tests and fixtures want the example's:" >&2
echo "  KEELER_SWAP_PROGRAM_ID=$example_id scripts/setup.sh" >&2
