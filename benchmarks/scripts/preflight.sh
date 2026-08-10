#!/usr/bin/env bash

set -euo pipefail
IFS=$'\n\t'

source "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
source "${BENCHMARK_ROOT}/scripts/settings.sh"

output=${1:-"$GENERATED_ROOT/preflight.json"}
implementation=${2:-all}
load_benchmark_settings

require_positive_integer BENCH_ORIGIN_PORT "$origin_port"
require_positive_integer BENCH_PROXY_PORT "$proxy_port"
require_nonnegative_integer BENCH_PROXY_CPU "$proxy_cpu"
require_nonnegative_integer BENCH_ORIGIN_CPU "$origin_cpu"
require_nonnegative_integer BENCH_LOAD_CPU "$load_cpu"
[[ $origin_port != "$proxy_port" ]] || die "origin and proxy ports must differ"

select_benchmark_implementations "$implementation"

mkdir -p -- "$(dirname -- "$output")"
if ! python3 "$BENCHMARK_ROOT/scripts/tool.py" preflight \
  "$output" "$oxiroute_bin" "$loadgen_bin" "$origin_port" "$proxy_port" \
  "$proxy_cpu" "$origin_cpu" "$load_cpu" "${implementations[@]}"; then
  printf 'preflight failed: %s\n' "$output" >&2
  exit 1
fi
printf 'preflight passed: %s\n' "$output"
