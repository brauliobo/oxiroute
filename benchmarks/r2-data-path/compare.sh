#!/bin/sh
# Instructions per request of an older release against the new binary (and the new binary with the fast lane off).
# Usage: compare.sh --new BIN [--base BIN] --out DIR LANE...
#   --new BIN   the binary under test (OXIROUTE_BIN)
#   --base BIN  an older release (6.3.x): measured with --baseline, so no 6.4-only option is generated for it
#   --out DIR   one directory per variant and lane: <variant>_<lane>/results.json (variants base, classic, new)
# Environment: R2TOOL_BIN (the load generator, default tool/target/release/r2tool), REPS (default 3).
# Each lane runs once per variant in turn, so a drift of the host load reaches the columns of one lane alike; every
# `run.py` call stays under five minutes (timeout). Print the table with `table.py --markdown DIR LANE...`.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
new= base= out=
while [ $# -gt 0 ]; do
  case $1 in
    --new) new=$2; shift 2 ;;
    --base) base=$2; shift 2 ;;
    --out) out=$2; shift 2 ;;
    --) shift; break ;;
    -*) echo "unknown option $1" >&2; exit 2 ;;
    *) break ;;
  esac
done
[ -n "$new" ] && [ -n "$out" ] && [ $# -gt 0 ] || { sed -n 2,9p "$0" >&2; exit 2; }
tool=${R2TOOL_BIN:-$here/tool/target/release/r2tool}
scratch=${R2_TMP:-${XDG_STATE_HOME:-$HOME/.local/state}/oxiroute-bench/tmp}
mkdir -p "$out" "$scratch"
measure() { # variant binary lane [run.py options]
  variant=$1 bin=$2 lane=$3
  shift 3
  timeout 290 python3 "$here/run.py" --bin "$bin" --tool "$tool" --lanes "$lane" --reps "${REPS:-3}" \
    --out "$out/${variant}_${lane}" --tmp "$scratch" "$@" >"$out/${variant}_${lane}.log" 2>&1 ||
    echo "$variant $lane: failed or unsupported, see $out/${variant}_${lane}.log" >&2
}
for lane in "$@"; do
  [ -z "$base" ] || measure base "$base" "$lane" --baseline
  measure classic "$new" "$lane" --classic
  measure new "$new" "$lane"
done
