#!/usr/bin/env bash

set -euo pipefail
IFS=$'\n\t'

source "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
source "${BENCHMARK_ROOT}/scripts/settings.sh"
load_benchmark_settings

validation_root="$GENERATED_ROOT/validation"
config_root="$validation_root/config"
runtime_root="$validation_root/runtime"
log_root="$validation_root/logs"
mkdir -p -- "$config_root" "$runtime_root" "$log_root"

render_config "$BENCHMARK_ROOT/config/oxiroute-reverse-h1.lua.in" \
  "$config_root/oxiroute.lua" ORIGIN_PORT "$origin_port" PROXY_PORT "$proxy_port"
render_config "$BENCHMARK_ROOT/config/nginx-origin.conf.in" \
  "$config_root/nginx-origin.conf" ORIGIN_PORT "$origin_port" RUNTIME_DIR "$runtime_root" LOG_DIR "$log_root"
render_config "$BENCHMARK_ROOT/config/nginx-reverse-h1.conf.in" \
  "$config_root/nginx-proxy.conf" ORIGIN_PORT "$origin_port" PROXY_PORT "$proxy_port" \
  RUNTIME_DIR "$runtime_root" LOG_DIR "$log_root"
render_config "$BENCHMARK_ROOT/config/haproxy-reverse-h1.cfg.in" \
  "$config_root/haproxy.cfg" ORIGIN_PORT "$origin_port" PROXY_PORT "$proxy_port"

if command -v luac >/dev/null 2>&1; then
  luac -p "$config_root/oxiroute.lua"
else
  printf 'SKIP luac syntax check: luac is unavailable\n'
fi

nginx -t -p "$runtime_root/" -c "$config_root/nginx-origin.conf"
nginx -t -p "$runtime_root/" -c "$config_root/nginx-proxy.conf"
haproxy -c -f "$config_root/haproxy.cfg"
python3 -c 'import pathlib, sys, xml.etree.ElementTree as ET; [ET.parse(path) for path in map(pathlib.Path, sys.argv[1:])]' \
  "$BENCHMARK_ROOT/phoronix/local/oxiroute-local-v1/test-definition.xml" \
  "$BENCHMARK_ROOT/phoronix/local/oxiroute-local-v1/results-definition.xml"
python3 -c 'import ast, pathlib, sys; ast.parse(pathlib.Path(sys.argv[1]).read_text())' \
  "$BENCHMARK_ROOT/scripts/tool.py"
"$BENCHMARK_ROOT/scripts/test-settings.sh"
python3 "$BENCHMARK_ROOT/scripts/tool.py" validate-reports "$BENCHMARK_ROOT/reports"
"$BENCHMARK_ROOT/scripts/test-evidence.sh"
cargo +1.97.1 test --manifest-path "$BENCHMARK_ROOT/loadgen/Cargo.toml" --locked --jobs 4
printf 'validation passed: %s\n' "$validation_root"
