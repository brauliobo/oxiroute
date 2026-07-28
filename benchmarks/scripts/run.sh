#!/usr/bin/env bash

set -euo pipefail
IFS=$'\n\t'

source "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"

implementation=all
output=
while (($#)); do
  case $1 in
    --implementation)
      (($# >= 2)) || die "--implementation requires a value"
      implementation=$2
      shift 2
      ;;
    --output)
      (($# >= 2)) || die "--output requires a value"
      output=$2
      shift 2
      ;;
    *) die "unknown argument: $1" ;;
  esac
done

case $implementation in
  all) implementations=(oxiroute nginx haproxy) ;;
  origin | oxiroute | nginx | haproxy) implementations=("$implementation") ;;
  *) die "unknown implementation: $implementation" ;;
esac

origin_port=${BENCH_ORIGIN_PORT:-19080}
proxy_port=${BENCH_PROXY_PORT:-19081}
connections=${BENCH_CONNECTIONS:-128}
warmup_seconds=${BENCH_WARMUP_SECONDS:-10}
duration_seconds=${BENCH_DURATION_SECONDS:-30}
BENCH_STOP_TIMEOUT_SECONDS=${BENCH_STOP_TIMEOUT_SECONDS:-10}
proxy_cpu=${BENCH_PROXY_CPU:-2}
origin_cpu=${BENCH_ORIGIN_CPU:-3}
load_cpu=${BENCH_LOAD_CPU:-4}
oxiroute_bin=${OXIROUTE_BIN:-"$REPOSITORY_ROOT/target/release/oxiroute"}
loadgen_bin=${BENCH_LOADGEN_BIN:-"$BENCHMARK_ROOT/loadgen/target/release/oxiroute-loadgen"}
export BENCH_STOP_TIMEOUT_SECONDS

for pair in \
  "BENCH_ORIGIN_PORT:$origin_port" \
  "BENCH_PROXY_PORT:$proxy_port" \
  "BENCH_CONNECTIONS:$connections" \
  "BENCH_WARMUP_SECONDS:$warmup_seconds" \
  "BENCH_DURATION_SECONDS:$duration_seconds" \
  "BENCH_STOP_TIMEOUT_SECONDS:$BENCH_STOP_TIMEOUT_SECONDS"; do
  require_positive_integer "${pair%%:*}" "${pair#*:}"
done
for pair in \
  "BENCH_PROXY_CPU:$proxy_cpu" \
  "BENCH_ORIGIN_CPU:$origin_cpu" \
  "BENCH_LOAD_CPU:$load_cpu"; do
  require_nonnegative_integer "${pair%%:*}" "${pair#*:}"
done
[[ $proxy_cpu != "$origin_cpu" && $proxy_cpu != "$load_cpu" && $origin_cpu != "$load_cpu" ]] || \
  die "proxy, origin, and load-generator CPUs must be distinct"
[[ $origin_port != "$proxy_port" ]] || die "origin and proxy ports must differ"

if [[ -z $output ]]; then
  run_id=$(date -u +%Y%m%dT%H%M%SZ)-$$
  output="$GENERATED_ROOT/runs/$run_id"
fi
output=$(python3 -c 'import pathlib, sys; print(pathlib.Path(sys.argv[1]).resolve())' "$output")
case "$output/" in
  "$GENERATED_ROOT/"*) ;;
  *) die "output must be below $GENERATED_ROOT" ;;
esac
config_root="$output/config"
runtime_root="$output/runtime"
log_root="$output/logs"
raw_root="$output/raw"
mkdir -p -- "$config_root" "$runtime_root" "$log_root" "$raw_root"

python3 "$BENCHMARK_ROOT/scripts/tool.py" skipped-lanes \
  "$BENCHMARK_ROOT/lanes.json" "$output/skips.json"
python3 "$BENCHMARK_ROOT/scripts/tool.py" run-metadata \
  "$output/run.json" "$implementation" "$origin_port" "$proxy_port" \
  "$connections" "$warmup_seconds" "$duration_seconds" \
  "$proxy_cpu" "$origin_cpu" "$load_cpu"
"$BENCHMARK_ROOT/scripts/preflight.sh" "$output/preflight.json" "$implementation"
"$BENCHMARK_ROOT/scripts/environment.sh" "$output/environment.json"

render_config "$BENCHMARK_ROOT/config/nginx-origin.conf.in" \
  "$config_root/nginx-origin.conf" ORIGIN_PORT "$origin_port" \
  RUNTIME_DIR "$runtime_root" LOG_DIR "$log_root"
render_config "$BENCHMARK_ROOT/config/oxiroute-reverse-h1.lua.in" \
  "$config_root/oxiroute.lua" ORIGIN_PORT "$origin_port" PROXY_PORT "$proxy_port"
render_config "$BENCHMARK_ROOT/config/nginx-reverse-h1.conf.in" \
  "$config_root/nginx-proxy.conf" ORIGIN_PORT "$origin_port" PROXY_PORT "$proxy_port" \
  RUNTIME_DIR "$runtime_root" LOG_DIR "$log_root"
render_config "$BENCHMARK_ROOT/config/haproxy-reverse-h1.cfg.in" \
  "$config_root/haproxy.cfg" ORIGIN_PORT "$origin_port" PROXY_PORT "$proxy_port"

install_cleanup_traps
start_process origin "$log_root/origin-stdout.log" "$log_root/origin-stderr.log" \
  taskset -c "$origin_cpu" nginx -p "$runtime_root/" -c "$config_root/nginx-origin.conf"
wait_for_http "http://127.0.0.1:$origin_port/healthz" origin
check_http_payload "http://127.0.0.1:$origin_port/payload" 1024 origin

run_implementation() {
  local current=$1
  local raw="$raw_root/loadgen-$current.json"
  local summary="$output/summary-$current.json"
  local target_port=$proxy_port

  case $current in
    origin)
      target_port=$origin_port
      ;;
    oxiroute)
      start_process proxy "$log_root/oxiroute-stdout.log" "$log_root/oxiroute-stderr.log" \
        taskset -c "$proxy_cpu" env RUST_LOG=warn "$oxiroute_bin" "$config_root/oxiroute.lua"
      ;;
    nginx)
      start_process proxy "$log_root/nginx-stdout.log" "$log_root/nginx-stderr.log" \
        taskset -c "$proxy_cpu" nginx -p "$runtime_root/" -c "$config_root/nginx-proxy.conf"
      ;;
    haproxy)
      start_process proxy "$log_root/haproxy-stdout.log" "$log_root/haproxy-stderr.log" \
        taskset -c "$proxy_cpu" haproxy -db -f "$config_root/haproxy.cfg"
      ;;
  esac

  wait_for_http "http://127.0.0.1:$target_port/healthz" "$current"
  check_http_payload "http://127.0.0.1:$target_port/payload" 1024 "$current"
  taskset -c "$load_cpu" "$loadgen_bin" \
    --implementation "$current" --host 127.0.0.1 --port "$target_port" --path /payload \
    --connections "$connections" --duration-seconds "$warmup_seconds" --expected-bytes 1024 \
    >"$raw_root/warmup-$current.json" 2>"$log_root/loadgen-warmup-$current.log"
  python3 "$BENCHMARK_ROOT/scripts/tool.py" summarize-loadgen \
    "$raw_root/warmup-$current.json" "$output/warmup-$current.json"
  taskset -c "$load_cpu" "$loadgen_bin" \
    --implementation "$current" --host 127.0.0.1 --port "$target_port" --path /payload \
    --connections "$connections" --duration-seconds "$duration_seconds" --expected-bytes 1024 \
    >"$raw" 2>"$log_root/loadgen-$current.log"
  python3 "$BENCHMARK_ROOT/scripts/tool.py" summarize-loadgen "$raw" "$summary"
  if [[ $current != origin ]]; then
    stop_named_process proxy
  fi
  printf '%s result: %s\n' "$current" "$summary"
}

for current in "${implementations[@]}"; do
  run_implementation "$current"
done

printf 'benchmark run complete: %s\n' "$output"
