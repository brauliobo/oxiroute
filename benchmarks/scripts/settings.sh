#!/usr/bin/env bash

load_benchmark_settings() {
  local output
  local -a values
  output=$(python3 "${BENCHMARK_ROOT}/scripts/tool.py" benchmark-settings "${BENCHMARK_ROOT}/lanes.json") || \
    die 'canonical benchmark settings are invalid'
  mapfile -t values <<<"${output}"
  ((${#values[@]} == 12)) || die 'canonical benchmark settings loader returned an incomplete contract'

  origin_port=${BENCH_ORIGIN_PORT:-${values[0]}}
  proxy_port=${BENCH_PROXY_PORT:-${values[1]}}
  connections=${BENCH_CONNECTIONS:-${values[2]}}
  warmup_seconds=${BENCH_WARMUP_SECONDS:-${values[3]}}
  duration_seconds=${BENCH_DURATION_SECONDS:-${values[4]}}
  BENCH_STOP_TIMEOUT_SECONDS=${BENCH_STOP_TIMEOUT_SECONDS:-${values[5]}}
  proxy_cpu=${BENCH_PROXY_CPU:-${values[6]}}
  origin_cpu=${BENCH_ORIGIN_CPU:-${values[7]}}
  load_cpu=${BENCH_LOAD_CPU:-${values[8]}}
  oxiroute_bin=${OXIROUTE_BIN:-"${REPOSITORY_ROOT}/${values[9]}"}
  loadgen_bin=${BENCH_LOADGEN_BIN:-"${REPOSITORY_ROOT}/${values[10]}"}
  read -r -a BENCHMARK_IMPLEMENTATIONS <<<"${values[11]}"
}

select_benchmark_implementations() {
  local implementation=$1
  local candidate
  if [[ "${implementation}" == all ]]; then
    implementations=("${BENCHMARK_IMPLEMENTATIONS[@]}")
    return
  fi
  if [[ "${implementation}" == origin ]]; then
    implementations=(origin)
    return
  fi
  for candidate in "${BENCHMARK_IMPLEMENTATIONS[@]}"; do
    if [[ "${implementation}" == "${candidate}" ]]; then
      implementations=("${implementation}")
      return
    fi
  done
  die "unknown implementation: ${implementation}"
}
