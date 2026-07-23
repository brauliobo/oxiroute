#!/usr/bin/env bash

set -euo pipefail
IFS=$'\n\t'

: "${OXIROUTE_BENCHMARK_ROOT:?OXIROUTE_BENCHMARK_ROOT is required}"
launcher="$OXIROUTE_BENCHMARK_ROOT/scripts/phoronix-benchmark.sh"
[[ -x $launcher ]] || {
  printf 'benchmark launcher is not executable: %s\n' "$launcher" >&2
  printf '2\n' > "$HOME/install-exit-status"
  exit 2
}
ln -sfn -- "$launcher" "$HOME/oxiroute-local-v1"
printf '0\n' > "$HOME/install-exit-status"
