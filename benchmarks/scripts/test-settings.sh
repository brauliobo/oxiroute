#!/usr/bin/env bash

set -euo pipefail
IFS=$'\n\t'

source "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
source "${BENCHMARK_ROOT}/scripts/settings.sh"

load_benchmark_settings
[[ "$origin_port:$proxy_port:$connections:$warmup_seconds:$duration_seconds" == '19080:19081:128:10:30' ]] || \
  die 'canonical benchmark defaults were not loaded'
[[ "${BENCHMARK_IMPLEMENTATIONS[*]}" == 'oxiroute nginx haproxy' ]] || \
  die 'runnable lane implementations were not loaded'

BENCH_ORIGIN_PORT=29080 BENCH_CONNECTIONS=16 load_benchmark_settings
[[ "$origin_port:$connections" == '29080:16' ]] || die 'benchmark environment overrides were not retained'

select_benchmark_implementations all
[[ "${implementations[*]}" == "${BENCHMARK_IMPLEMENTATIONS[*]}" ]] || \
  die 'all implementation selection drifted from the runnable lane'
if (select_benchmark_implementations unknown) >/dev/null 2>&1; then
  die 'unknown benchmark implementation was accepted'
fi

invalid=$(mktemp)
trap 'rm -f -- "$invalid"' EXIT
printf '%s\n' '{"schema":"oxiroute.local-v1.lanes.v1","settings":{},"lanes":[]}' >"$invalid"
if python3 "$BENCHMARK_ROOT/scripts/tool.py" benchmark-settings "$invalid" >/dev/null 2>&1; then
  die 'invalid benchmark manifest was accepted'
fi

printf 'benchmark settings tests passed\n'
