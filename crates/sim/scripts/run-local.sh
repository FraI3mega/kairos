#!/usr/bin/env bash
#   crates/sim/scripts/run-local.sh check
#   crates/sim/scripts/run-local.sh load --users 500 --duration 5m --concurrency 64
# Needs KAIROS_TEST_DATABASE_URL. SIM_PROFILE=debug builds without optimisations (default release).
set -euo pipefail
cd "$(dirname "$0")/../../.."
: "${KAIROS_TEST_DATABASE_URL:?set KAIROS_TEST_DATABASE_URL}"
profile=${SIM_PROFILE:-release}
flag=()
[ "$profile" = release ] && flag=(--release)
cargo build -q "${flag[@]}" -p kairos-server -p kairos-sim
bin=${CARGO_TARGET_DIR:-target}/$profile

port=${SIM_PORT:-$((20000 + RANDOM % 20000))}
while ss -ltn | grep -q ":$port "; do port=$((20000 + RANDOM % 20000)); done
schema=sim_$(date +%s)_$RANDOM
sep='?'
case $KAIROS_TEST_DATABASE_URL in *\?*) sep='&' ;; esac

pid=
cleanup() {
  if [ -n "$pid" ]; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
  "$bin/kairos-sim" db drop --url "$KAIROS_TEST_DATABASE_URL" --schema "$schema" >/dev/null || true
}
trap cleanup EXIT

"$bin/kairos-sim" db create --url "$KAIROS_TEST_DATABASE_URL" --schema "$schema" >/dev/null
export DATABASE_URL="${KAIROS_TEST_DATABASE_URL}${sep}options=-c%20search_path%3D$schema"
export PUBLIC_URL="http://127.0.0.1:$port" BIND="127.0.0.1:$port" KAIROS_AUTH=mock KAIROS_TRUSTED_PROXY=loopback
export KAIROS_LOG="${KAIROS_LOG:-warn}"
"$bin/kairos-server" serve &
pid=$!

for _ in $(seq 100); do
  if exec 3<>"/dev/tcp/127.0.0.1/$port" 2>/dev/null; then
    exec 3>&-
    break
  fi
  kill -0 "$pid" 2>/dev/null || { echo "kairos-server exited during start" >&2; exit 1; }
  sleep 0.2
done
echo "server pid $pid on 127.0.0.1:$port, schema $schema"

extra=()
[ "${1:-}" = load ] && extra=(--pid "$pid")
"$bin/kairos-sim" "$@" --server "http://127.0.0.1:$port" "${extra[@]}"
