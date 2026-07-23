#!/usr/bin/env bash

set -euo pipefail
IFS=$'\n\t'

source "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"

output=${1:-"$GENERATED_ROOT/environment.json"}
oxiroute_bin=${OXIROUTE_BIN:-"$REPOSITORY_ROOT/target/release/oxiroute-server"}
loadgen_bin=${BENCH_LOADGEN_BIN:-"$BENCHMARK_ROOT/loadgen/target/release/oxiroute-loadgen"}
mkdir -p -- "$(dirname -- "$output")"
python3 "$BENCHMARK_ROOT/scripts/tool.py" environment \
  "$output" "$REPOSITORY_ROOT" "$oxiroute_bin" "$loadgen_bin"
printf 'environment captured: %s\n' "$output"
