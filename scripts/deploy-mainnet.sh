#!/usr/bin/env bash
# Builds and deploys the swap or the lend program to mainnet. NOT TO BE RUN
# without the owner: it spends real SOL and hands the upgrade authority to a
# multisig, which the deploy key cannot take back. It cannot run before the
# key ceremony (docs/mainnet-runbook.md); --estimate works today and sends
# nothing.
#
#   scripts/deploy-mainnet.sh swap|lend [--estimate] --program-id <a> --payer <a>
#       [--multisig <a> [--vault-index <n>]] [--manifest <file>] [--rpc-url <url>]
#     lend also: --admin <a> --keepers <a,b> --pool-viewing <66 hex>
#       --collateral-mint <a> --debt-mint <a> --price-account <a> --feed-id <64 hex>
#       and, without --estimate, --keeper-config <file> --keys-dir <dir>
#   scripts/deploy-mainnet.sh swap|lend --check-so <file.so> --manifest <file>
#     only the marker checks, on any .so (for example one from
#     `solana program dump`); reads no chain
#
# It refuses unless: the git tree is clean; the RPC's genesis hash is
# mainnet's; every build parameter is given; the .so's embedded verifying-key
# markers are all from a real setup and its own keys are exactly the ones in
# the ceremony manifest (sha256sum lines of the ceremony's pk.bin files); the
# .so has no test-price marker; the new upgrade authority is vault
# <vault-index> of an autonomous Squads v4 multisig with threshold at least 2,
# read from chain. In order: build, checks, cost, deploy (typed
# confirmation), the same marker checks on the deployed program data, hand
# the upgrade authority to the vault (typed confirmation) and read it back,
# then (lend) create the reserve with `keeler lend-init-reserve`. It creates
# no token, runs no faucet and moves no inventory.
#
# Keychain items (Solana keypair JSON arrays), made by the owner before a real
# run: keeler-mainnet-deploy (pays, the first upgrade authority, and the lend
# admin: --admin and --payer are its address), keeler-mainnet-<program>-program
# (its address is --program-id), keeler-mainnet-<program>-buffer; and the
# Helius key in keeler-helius-api-key.
#
# Writes only target/mainnet-<program>/: plan.json, build.json (commit and
# .so sha256), the .so, deployed.so, deployment.json, deploy.log, and the
# build directories.
set -euo pipefail

usage() {
    sed -n '/^#   scripts/,/^#     `solana/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
    exit 1
}
die() { echo "refusing: $*" >&2; exit 1; }

program=${1:-}
[[ $program == swap || $program == lend ]] || usage
shift
estimate=false check_so=
# macOS bash 3.2 has no associative arrays: --some-name sets $arg_some_name,
# read back by name with opt.
opt() { local v=arg_${1//-/_}; printf '%s' "${!v:-}"; }
# Cleared first, so nothing comes in from the environment.
arg_program_id="" arg_payer="" arg_multisig="" arg_vault_index="" arg_manifest="" arg_rpc_url="" arg_admin=""
arg_keepers="" arg_pool_viewing="" arg_collateral_mint="" arg_debt_mint="" arg_price_account=""
arg_feed_id="" arg_keeper_config="" arg_keys_dir=""
while (($#)); do
    case $1 in
    --estimate) estimate=true; shift ;;
    --check-so) [[ $# -ge 2 ]] || usage; check_so=$2; shift 2 ;;
    --program-id | --payer | --multisig | --vault-index | --manifest | --rpc-url | --admin | \
        --keepers | --pool-viewing | --collateral-mint | --debt-mint | --price-account | \
        --feed-id | --keeper-config | --keys-dir)
        [[ $# -ge 2 && -n $2 ]] || die "$1 needs a value"
        n=${1#--}
        printf -v "arg_${n//-/_}" '%s' "$2"
        shift 2
        ;;
    *) usage ;;
    esac
done

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
main=$(git -C "$root" worktree list --porcelain | awk '/^worktree /{print $2; exit}')
[[ -f $main/.tools/env ]] || die "no $main/.tools/env: run scripts/setup.sh in $main first"
# shellcheck disable=SC1091
source "$main/.tools/env"
# The swap .so is the only program with three keys of its own, lend eleven.
case $program in swap) circuits=3 ;; lend) circuits=11 ;; esac

is_address() { [[ $1 =~ ^[1-9A-HJ-NP-Za-km-z]{32,44}$ ]]; }

# The manifest: one "<sha256>  <circuit>/pk.bin" line per circuit of the
# program, as `zolana vks check --expect` reads it.
check_manifest() {
    [[ -f $1 ]] || die "no manifest $1"
    python3 - "$1" "$circuits" <<'EOF' || exit 1
import re, sys
path, want = sys.argv[1], int(sys.argv[2])
lines = [l for l in open(path).read().splitlines() if l.strip()]
pk = [l for l in lines if l.endswith("pk.bin")]
if not all(re.fullmatch(r"[0-9a-f]{64}  \S+", l) for l in lines) or len(pk) != want or len({l[:64] for l in pk}) != want:
    sys.exit(f"refusing: {path} is not a ceremony manifest: want {want} distinct '<sha256>  <circuit>/pk.bin' lines")
EOF
}

# Every verifying key in the .so is from a real setup, its own keys (those
# zolana does not know: the shielded pool's are known) are exactly the
# manifest's, and it has no test price.
check_markers() {
    local so=$1 manifest=$2 report
    [[ -f $so ]] || die "no $so"
    check_manifest "$manifest"
    if LC_ALL=C grep -q -a KEELER_LEND_TEST_PRICE_BUILD "$so"; then
        die "$so has the test price (KEELER_LEND_TEST_PRICE_BUILD): a local or devnet build"
    fi
    # zolana's own check: no test setup anywhere, every manifest key present.
    report=$("$ZOLANA_CLI_BIN" vks check --so "$so" --expect "$manifest" 2>&1) || {
        printf '%s\n' "$report" | grep -v '^  VERIFYINGKEY: [a-z_0-9]*\.key ' >&2
        die "$so: the verifying-key markers fail zolana vks check against $manifest"
    }
    "$ZOLANA_CLI_BIN" vks list --so "$so" | python3 -c '
import sys
so, manifest = sys.argv[1], sys.argv[2]
want = {l[:64] for l in open(manifest) if l.rstrip().endswith("pk.bin")}
own, bad = set(), 0
for line in sys.stdin:
    parts = line.split()
    if len(parts) == 3 and parts[0] == "unknown":
        own.add(parts[2].removeprefix("proving_key_sha256="))
    bad += "insecure_test_setup=true" in line
if bad or own != want:
    sys.exit(f"refusing: {so}: its own verifying keys {sorted(own)} are not the manifest keys {sorted(want)}" + (" and some are test setups" if bad else ""))
' "$so" "$manifest" || exit 1
    echo "markers: $so: real setups, its $circuits keys match $manifest, no test price" >&2
}

if [[ -n $check_so ]]; then
    [[ -n ${arg_manifest:-} ]] || die "--check-so needs --manifest"
    check_markers "$check_so" "${arg_manifest}"
    exit 0
fi

# Every build parameter explicitly; nothing defaults on mainnet.
need=(program-id payer)
[[ $program == lend ]] && need+=(admin keepers pool-viewing collateral-mint debt-mint price-account feed-id)
$estimate || need+=(multisig manifest)
$estimate || [[ $program == swap ]] || need+=(keeper-config keys-dir)
missing=()
for n in "${need[@]}"; do [[ -n $(opt "$n") ]] || missing+=("--$n"); done
((${#missing[@]} == 0)) || die "missing ${missing[*]}; every parameter is given explicitly"
for n in program-id payer multisig admin collateral-mint debt-mint price-account; do
    v=$(opt "$n")
    [[ -z $v ]] || is_address "$v" || die "--$n $v is not a base58 address"
done
if [[ $program == lend ]]; then
    IFS=, read -ra keepers <<<"${arg_keepers}"
    for k in "${keepers[@]}"; do is_address "$k" || die "--keepers: '$k' is not a base58 address"; done
    [[ ${arg_pool_viewing} =~ ^04[0-9a-f]{64}$ ]] || die "--pool-viewing is not 66 hex starting 04"
    [[ ${arg_feed_id} =~ ^[0-9a-f]{64}$ ]] || die "--feed-id is not 64 hex"
    # The admin only co-signs the reserve's creation, below, with the deploy key.
    [[ ${arg_admin} == "${arg_payer}" ]] || die "--admin must be the deploy key, --payer"
fi
vault_index=${arg_vault_index:-0}
if ! [[ $vault_index =~ ^[0-9]{1,3}$ ]] || ((vault_index > 255)); then die "--vault-index is 0 to 255"; fi
rpc_url=${arg_rpc_url:-https://mainnet.helius-rpc.com}
[[ $rpc_url =~ ^https://[a-z0-9.-]+\.helius-rpc\.com/?$ ]] || die "--rpc-url is a Helius https host (the key comes from the Keychain)"
rpc_url=${rpc_url%/}

# A worktree's .tools is a link to the main checkout's, untracked: the one
# untracked path allowed.
dirty=$(git -C "$root" status --porcelain | grep -v '^?? \.tools$' || true)
[[ -z $dirty ]] || die "the git tree is not clean:
$dirty"
commit=$(git -C "$root" rev-parse HEAD)

out=$root/target/mainnet-$program
$estimate && out=$out/estimate
mkdir -p "$out"
# Nothing here should print the Helius key; this catches it if a tool does.
redact() { perl -pe 'BEGIN { $| = 1 } s/api-key=[^&"\s]+/api-key=REDACTED/g'; }
exec > >(redact) 2> >(redact | tee -a "$out/deploy.log" >&2)
echo "deploy-mainnet $program at $commit$($estimate && echo ' (estimate)')" >&2

# The real build needs the ceremony's verifying keys committed: the test
# ones refuse to compile without insecure-test-setup.
no_ceremony() {
    die "no key ceremony yet: $1. The mainnet deploy cannot run before the ceremony's verifying keys are committed (docs/mainnet-runbook.md, step 3). --estimate works without them."
}
if ! $estimate; then
    check_manifest "${arg_manifest}"
    for n in deploy "$program-program" "$program-buffer"; do
        security find-generic-password -s "keeler-mainnet-$n" >/dev/null 2>&1 ||
            die "no Keychain item keeler-mainnet-$n (docs/mainnet-runbook.md)"
    done
    if [[ $program == lend ]]; then
        grep -l 'insecure_test_setup\\x00true' "$root"/lend/program/src/verifier/*.vk.rs >/dev/null &&
            no_ceremony "lend/program/src/verifier/*.vk.rs are test setups"
        [[ -f ${arg_keeper_config} ]] || die "no keeper config ${arg_keeper_config}"
        # The keeper proves with the ceremony's proving keys.
        (cd "${arg_keys_dir}" && shasum -a 256 -c --quiet "${arg_manifest}") ||
            die "${arg_keys_dir} does not hold the manifest's proving keys"
    else
        for c in make take cancel; do
            [[ -f $root/ceremony/swap/$c.vk.rs ]] || no_ceremony "no ceremony/swap/$c.vk.rs"
        done
        grep -l 'insecure_test_setup\\x00true' "$root"/ceremony/swap/*.vk.rs >/dev/null &&
            no_ceremony "ceremony/swap/*.vk.rs are test setups"
    fi
fi

# The deploy tool: `keeler deploy` (cli/src/deploy.rs), in its own target.
CARGO_TARGET_DIR=$root/target/mainnet-$program/cargo-tool cargo build --release --quiet \
    --manifest-path "$root/Cargo.toml" -p keeler --bin keeler
keeler=$root/target/mainnet-$program/cargo-tool/release/keeler

# The build. --estimate builds the same code with the committed test keys
# (and without the mainnet feature, which refuses them): the size, and so
# the cost, is the same within the verifying keys' few bytes.
case $program in
lend)
    so=$out/deploy/keeler_lend.so
    # --estimate keeps the default insecure-test-setup, which the test keys need.
    # shellcheck disable=SC2054 # one cargo argument
    features=(--no-default-features --features bpf-entrypoint,mainnet)
    $estimate && features=(--features bpf-entrypoint)
    rm -f "$so"
    (
        cd "$root/lend"
        unset KEELER_LEND_KEEPER
        KEELER_LEND_PROGRAM_ID=${arg_program_id} KEELER_LEND_ADMIN=${arg_admin} \
            KEELER_LEND_KEEPERS=${arg_keepers} KEELER_LEND_POOL_VIEWING=${arg_pool_viewing} \
            KEELER_LEND_VAULT1_COLLATERAL_MINT=${arg_collateral_mint} \
            KEELER_LEND_VAULT1_DEBT_MINT=${arg_debt_mint} \
            KEELER_LEND_VAULT1_PRICE_ACCOUNT=${arg_price_account} \
            KEELER_LEND_VAULT1_FEED_ID=${arg_feed_id} \
            CARGO_TARGET_DIR=$out/cargo cargo build-sbf --manifest-path program/Cargo.toml \
            --tools-version v1.54 --sbf-out-dir "$out/deploy" -- --locked "${features[@]}"
    )
    ;;
swap)
    so=$out/swap_program.so
    if $estimate; then
        # The local build's size: only the program ID differs.
        cp "$KEELER_SWAP_PROGRAM_SO" "$so"
    else
        # The pinned upstream example, our patch, the ID and the ceremony's keys.
        repo_commit=$(sed -n 's/^commit=//p' "$root/scripts/setup.sh")
        src=$out/src
        rm -rf "$src"
        git clone --quiet https://github.com/helius-labs/privacy-ring-examples "$src"
        git -C "$src" checkout --quiet --detach "$repo_commit"
        git -C "$src" apply "$root/patches/swap.patch"
        sed -E -i '' "s/(declare_id!\(\")[^\"]*(\"\))/\1${arg_program_id}\2/" "$src/swap/xark/program/src/lib.rs"
        grep -q "declare_id!(\"${arg_program_id}\")" "$src/swap/xark/program/src/lib.rs" || die "no declare_id! in the swap program"
        cp "$root"/ceremony/swap/{make,take,cancel}.vk.rs "$src/swap/xark/program/src/verifier/"
        (cd "$src/swap/xark" && unset CARGO_TARGET_DIR && cargo build-sbf --manifest-path program/Cargo.toml \
            --tools-version v1.54 -- --locked --no-default-features --features bpf-entrypoint)
        cp "$src/swap/xark/target/deploy/swap_program.so" "$so"
    fi
    ;;
esac
[[ -f $so ]] || die "the build left no $so"
so_sha256=$(shasum -a 256 "$so" | awk '{print $1}')
printf '{"program": "%s", "commit": "%s", "so_sha256": "%s", "estimate": %s}\n' \
    "$program" "$commit" "$so_sha256" "$estimate" >"$out/build.json"
echo "built $so: sha256 $so_sha256, commit $commit" >&2

if $estimate; then
    if [[ -n ${arg_manifest:-} ]]; then check_markers "$so" "${arg_manifest}"; fi
    if LC_ALL=C grep -q -a KEELER_LEND_TEST_PRICE_BUILD "$so"; then die "$so has the test price"; fi
else
    check_markers "$so" "${arg_manifest}"
fi

python3 - "$out/plan.json" "$program" "$rpc_url" "$so" "${arg_payer}" "${arg_program_id}" \
    "${arg_multisig:-}" "$vault_index" <<'EOF'
import json, sys
path, name, rpc, so, payer, program_id, multisig, index = sys.argv[1:]
plan = {
    "name": name,
    "network": {
        "rpc_url": rpc,
        "indexer_url": rpc + "/v1/spr",
        "prover": {"helius": {"keychain": "keeler-helius-api-key"}},
    },
    "genesis_hash": "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d",
    "deploy_key": {"keychain": "keeler-mainnet-deploy"},
    "program_key": {"keychain": f"keeler-mainnet-{name}-program"},
    "buffer_key": {"keychain": f"keeler-mainnet-{name}-buffer"},
    "program_so": so,
    "payer": payer,
    "program_id": program_id,
    "multisig": multisig or None,
    "vault_index": int(index),
}
open(path, "w").write(json.dumps(plan, indent=2) + "\n")
EOF

# Genesis hash, the Squads multisig (when given) and the cost; sends nothing.
"$keeler" deploy check "$out/plan.json" >"$out/estimate.json"
if $estimate; then
    cat "$out/estimate.json"
    echo "estimate only: nothing sent. A real run also needs --multisig, --manifest$([[ $program == lend ]] && echo ', --keeper-config, --keys-dir') and the ceremony's keys." >&2
    exit 0
fi

confirm() {
    echo "$1" >&2
    echo "Type $2 to confirm:" >&2
    local typed
    read -r typed
    [[ $typed == "$2" ]] || die "not confirmed; stopped before this step"
}
confirm "Deploy $so ($so_sha256) to mainnet as ${arg_program_id}, paid by ${arg_payer}." "${arg_program_id}"
"$keeler" deploy program "$out/plan.json" "$out/deployed.so" >"$out/summary.json"
cmp -s "$so" "$out/deployed.so" || die "the deployed program data differs from $so"
check_markers "$out/deployed.so" "${arg_manifest}"

confirm "Hand the upgrade authority of ${arg_program_id} to vault $vault_index of Squads multisig ${arg_multisig}. The deploy key cannot take it back." "${arg_multisig}"
"$keeler" deploy hand-over "$out/plan.json" "${arg_program_id}" "${arg_multisig}" "$vault_index" >"$out/authority.json"
cat "$out/authority.json" >&2

if [[ $program == lend ]]; then
    KEELER_LEND_KEYS_DIR=${arg_keys_dir} "$keeler" lend-init-reserve "${arg_keeper_config}" >&2
fi

python3 - "$out" "$commit" "$so_sha256" <<'EOF'
import json, sys, time
out, commit, sha = sys.argv[1:]
d = json.load(open(f"{out}/authority.json"))
d.update(network="mainnet", commit=commit, so_sha256=sha, deployed_at=int(time.time()))
open(f"{out}/deployment.json", "w").write(json.dumps(d, indent=2) + "\n")
EOF
echo "wrote $out/deployment.json; next: docs/mainnet-runbook.md, after the deploy" >&2
