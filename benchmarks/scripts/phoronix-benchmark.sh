#!/usr/bin/env bash

set -euo pipefail
IFS=$'\n\t'

: "${OXIROUTE_BENCHMARK_ROOT:?OXIROUTE_BENCHMARK_ROOT is required}"
: "${LOG_FILE:?LOG_FILE is required}"

implementation=${1:-oxiroute}
case $implementation in
  oxiroute | nginx | haproxy) ;;
  *)
    printf 'unknown implementation: %s\n' "$implementation" >&2
    printf '2\n' > "$HOME/test-exit-status"
    exit 2
    ;;
esac

run_id="pts-${implementation}-$(date -u +%Y%m%dT%H%M%SZ)-$$"
output="$OXIROUTE_BENCHMARK_ROOT/generated/runs/$run_id"
if "$OXIROUTE_BENCHMARK_ROOT/scripts/run.sh" \
  --implementation "$implementation" --output "$output"; then
  value=$(python3 "$OXIROUTE_BENCHMARK_ROOT/scripts/tool.py" \
    result-value "$output/summary-$implementation.json")
  printf 'Requests/sec: %s\n' "$value" > "$LOG_FILE"
  printf '0\n' > "$HOME/test-exit-status"
else
  status=$?
  printf '%s\n' "$status" > "$HOME/test-exit-status"
  exit "$status"
fi
