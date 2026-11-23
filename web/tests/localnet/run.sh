#!/usr/bin/env bash
# Starts the test local network (see src/main.rs) and keeps it up until this
# script's stdin closes. Ports: RPC 10899, Photon 10784, prover 3101, dealer
# 47950, so it runs beside piece A's tests.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
# shellcheck disable=SC1091
source "$root/.tools/env"
export ZOLANA_PORT_OFFSET="${ZOLANA_PORT_OFFSET:-2000}"
export ZOLANA_PROVER_URL="${ZOLANA_PROVER_URL:-http://127.0.0.1:3101}"
cargo build --release --quiet --manifest-path "$here/Cargo.toml"
exec "$here/target/release/keeler-web-localnet" "${1:-$here/../.localnet}"
