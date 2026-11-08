#!/bin/sh
# R2 data-path harness entry point. Usage: run.sh [--lanes LIST] [--reps N] [--sys] [--alloc] [--mem] [--profile] ...
# Environment: OXIROUTE_BIN (proxy binary), R2TOOL_BIN (upstream/load tool), R2_OUT, R2_PROXY_CPUS, R2_UPSTREAM_CPUS,
# R2_LOAD_CPUS. Builds the tool on first use when R2TOOL_BIN is not set.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
if [ -z "${R2TOOL_BIN:-}" ] && [ ! -x "$here/tool/target/release/r2tool" ]; then
  (cd "$here/tool" && nice cargo build --release --offline -j2 >&2)
fi
exec python3 "$here/run.py" "$@"
