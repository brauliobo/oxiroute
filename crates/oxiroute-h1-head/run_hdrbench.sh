#!/bin/sh
# Instructions per iteration of the head work, as the difference of two iteration counts under `perf stat`.
# Usage: run_hdrbench.sh MODE   (req3 req12 resp3 resp12 scan3 scan12 all3 all12); environment: CARGO_TARGET_DIR.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
export PATH=${PATH}
nice cargo build --release --offline --locked -j4 -p oxiroute-h1-head --example hdrbench --manifest-path "$here/Cargo.toml" 2>&1 | tail -1
bin=${CARGO_TARGET_DIR:-$here/../../target}/release/examples/hdrbench
count() {
  perf stat -x, -e instructions:u "$bin" "$1" "$2" 2>&1 >/dev/null | awk -F, '/instructions/ {print $1}'
}
low=100000
high=1100000
a=$(count "$1" $low)
b=$(count "$1" $high)
echo "$1: $(( (b - a) / (high - low) )) instructions per iteration"
