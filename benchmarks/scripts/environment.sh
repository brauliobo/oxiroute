#!/usr/bin/env bash

set -euo pipefail
IFS=$'\n\t'

source "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
source "${BENCHMARK_ROOT}/scripts/settings.sh"

output=${1:-"$GENERATED_ROOT/environment.json"}
load_benchmark_settings
mkdir -p -- "$(dirname -- "$output")"
python3 "$BENCHMARK_ROOT/scripts/tool.py" environment \
  "$output" "$REPOSITORY_ROOT" "$oxiroute_bin" "$loadgen_bin"
printf 'environment captured: %s\n' "$output"
