#!/usr/bin/env bash
# Deploys Keeler's build of the swap example to devnet and sets up the dealer.
# NOT TO BE RUN until the owner approves publishing the example's code (see
# the mainnet gates in docs/plan-web.md). Idempotent: every step checks what
# is already there.
#
# 1. Creates these Keychain items if absent (Solana keypair JSON arrays, plus
#    a random HTTP key): keeler-devnet-deploy, keeler-devnet-program,
#    keeler-devnet-dealer, keeler-devnet-faucet, keeler-devnet-tusd-mint,
#    keeler-devnet-tsol-mint, keeler-devnet-dealer-http-key. Keys are read
#    by process substitution, never written to a file.
# 2. Funds the deploy key by airdrop, the dealer and faucet keys from it.
# 3. Builds with our program ID (scripts/setup.sh) and deploys the program.
# 4. Creates tUSD and tSOL (faucet key is the mint authority) and their pool
#    interfaces, and deposits the dealer's inventory, once.
# 5. Writes target/devnet/dealer-config.json, which names only Keychain items.
#
# Needs: solana, spl-token, security (macOS), and the Helius key in the
# Keychain item keeler-helius-api-key (a full Helius URL or a bare key).
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
target=${CARGO_TARGET_DIR:-$root/target}
out=$root/target/devnet
mkdir -p "$out"

key() { security find-generic-password -s "$1" -w; }
have() { security find-generic-password -s "$1" >/dev/null 2>&1; }
# The secret goes to security on stdin, never on a command line.
store() { printf 'add-generic-password -s "%s" -a keeler -w "%s"\n' "$1" "$2" | security -i >/dev/null; }
ensure_keypair() {
    have "$1" && return
    local json
    json=$(solana-keygen new --no-bip39-passphrase --silent --force --outfile /dev/fd/3 3>&1 >/dev/null)
    store "$1" "$json"
    echo "created Keychain item $1" >&2
}
pubkey() { solana-keygen pubkey <(key "$1"); }

helius=$(key keeler-helius-api-key)
if [[ $helius == https://* ]]; then
    helius=$(sed -nE 's/.*[?&]api-key=([A-Za-z0-9-]+).*/\1/p' <<<"$helius")
fi
[[ $helius =~ ^[A-Za-z0-9-]+$ ]] || { echo "keeler-helius-api-key holds no usable key" >&2; exit 1; }
# These hold the key: only ever passed as arguments or in the environment
# (so they show in `ps` while a command runs), never echoed or written.
rpc="https://devnet.helius-rpc.com/?api-key=$helius"
spr="https://devnet.helius-rpc.com/v1/spr?api-key=$helius"
unset helius
# Devnet only: refuse any cluster whose genesis is not devnet's.
devnet_genesis=EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG
[[ $(solana -u "$rpc" genesis-hash) == "$devnet_genesis" ]] || { echo "the RPC is not devnet; refusing" >&2; exit 1; }

for item in deploy program dealer faucet tusd-mint tsol-mint; do
    ensure_keypair "keeler-devnet-$item"
done
have keeler-devnet-dealer-http-key || store keeler-devnet-dealer-http-key "$(openssl rand -hex 24)"

deploy=$(pubkey keeler-devnet-deploy)
program_id=$(pubkey keeler-devnet-program)
dealer=$(pubkey keeler-devnet-dealer)
faucet=$(pubkey keeler-devnet-faucet)
echo "deploy $deploy, program $program_id, dealer $dealer, faucet $faucet" >&2

sol() { solana -u "$rpc" "$@"; }
lamports() { sol balance --lamports "$1" | awk '{print $1}'; }
# Deploying the program costs about 0.65 SOL; devnet airdrops cap at 2 SOL.
for _ in 1 2 3; do
    (($(lamports "$deploy") >= 3000000000)) && break
    sol airdrop 2 "$deploy" || { echo "airdrop refused; fund $deploy by hand and rerun" >&2; exit 1; }
done
(($(lamports "$deploy") >= 3000000000)) || { echo "fund $deploy to 3 SOL and rerun" >&2; exit 1; }
for who in "$dealer" "$faucet"; do
    if (($(lamports "$who") < 200000000)); then
        sol transfer --from <(key keeler-devnet-deploy) --fee-payer <(key keeler-devnet-deploy) \
            --allow-unfunded-recipient "$who" 0.5
    fi
done

KEELER_SWAP_PROGRAM_ID=$program_id "$root/scripts/setup.sh"
# shellcheck source=/dev/null
. "$root/.tools/env"
if sol program show "$program_id" >/dev/null 2>&1; then
    echo "program $program_id is already deployed" >&2
else
    sol program deploy --keypair <(key keeler-devnet-deploy) \
        --program-id <(key keeler-devnet-program) "$KEELER_SWAP_PROGRAM_SO"
fi

cargo build --release --quiet --manifest-path "$root/Cargo.toml" -p keeler --bin keeler --example spl_interface
spl() { spl-token -u "$rpc" --fee-payer <(key keeler-devnet-deploy) "$@"; }
mint_tusd="" mint_tsol=""
for t in tusd:6:1000000000000 tsol:9:10000000000000; do
    IFS=: read -r name decimals inventory <<<"$t"
    m=$(pubkey "keeler-devnet-$name-mint")
    printf -v "mint_$name" %s "$m"
    if ! sol account "$m" >/dev/null 2>&1; then
        spl create-token --decimals "$decimals" --mint-authority "$faucet" <(key "keeler-devnet-$name-mint")
    fi
    KEELER_RPC_URL=$rpc "$target/release/examples/spl_interface" <(key keeler-devnet-deploy) "$m"
    # A dealer token account means its inventory was already deposited.
    ata=$(spl address --verbose --token "$m" --owner "$dealer" | awk '/Associated token address/ {print $4}')
    if ! sol account "$ata" >/dev/null 2>&1; then
        spl create-account "$m" --owner "$dealer"
        spl mint "$m" "$inventory" --mint-authority <(key keeler-devnet-faucet) -- "$ata"
        echo "depositing $inventory base units of $name into the dealer's private balance" >&2
        "$ZOLANA_CLI_BIN" wallet register --keypair <(key keeler-devnet-dealer) --rpc-url "$rpc" || true
        "$ZOLANA_CLI_BIN" deposit --keypair <(key keeler-devnet-dealer) \
            --rpc-url "$rpc" --indexer-url "$spr" --prover-url "$spr" \
            --prover-tee-policy "$root/.tools/privacy-ring-examples/devnet-e-tee-policy.json" \
            --mint "$m" --amount "$inventory"
    fi
done
unset rpc spr

cat >"$out/dealer-config.json" <<EOF
{
  "network": {
    "rpc_url": "https://devnet.helius-rpc.com",
    "indexer_url": "https://devnet.helius-rpc.com/v1/spr",
    "prover": {"helius": {"keychain": "keeler-helius-api-key"}}
  },
  "key": {"keychain": "keeler-devnet-dealer"},
  "assets": [
    {"mint": "$mint_tusd", "price": {"price": 1, "expo": 0}},
    {"mint": "$mint_tsol", "price": {"price": 150, "expo": 0}}
  ],
  "listen": "127.0.0.1:47001",
  "spread_bps": 10,
  "quote_ttl_secs": 300,
  "settle_margin_secs": 60,
  "http": {"listen": "127.0.0.1:47500", "key": {"keychain": "keeler-devnet-dealer-http-key"}},
  "faucet": {
    "key": {"keychain": "keeler-devnet-faucet"},
    "tokens": [
      {"mint": "$mint_tusd", "amount": 1000000000},
      {"mint": "$mint_tsol", "amount": 10000000000}
    ],
    "lamports": 20000000,
    "per_wallet_daily": 1,
    "daily_cap": 200
  },
  "pause_file": "$out/PAUSE",
  "settle_tolerance_bps": 0,
  "limits": [
    {"mint": "$mint_tusd", "max_sell_amount": 10000000000, "hourly_sell_cap": 100000000000},
    {"mint": "$mint_tsol", "max_sell_amount": 100000000000, "hourly_sell_cap": 1000000000000}
  ]
}
EOF
echo "programId $program_id; tUSD $mint_tusd; tSOL $mint_tsol; dealer $dealer" >&2
echo "wrote $out/dealer-config.json; next: scripts/run-dealer.sh" >&2
