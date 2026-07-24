#!/usr/bin/env bash

set -euo pipefail
IFS=$'\n\t'

source "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"

(($# >= 2)) || die "usage: $0 REPORT_JSON RUN_DIRECTORY..."
supplied_report=$(python3 -c 'import pathlib, sys; print(pathlib.Path(sys.argv[1]).absolute())' "$1")
report=$(python3 -c 'import pathlib, sys; print(pathlib.Path(sys.argv[1]).resolve())' "$1")
shift
[[ $(dirname -- "$supplied_report") == "$BENCHMARK_ROOT/reports" && \
   $(dirname -- "$report") == "$BENCHMARK_ROOT/reports" && \
   $(basename -- "$report") == *.json ]] || \
  die "report must be a JSON file directly below $BENCHMARK_ROOT/reports"

runs=()
for run in "$@"; do
  run=$(python3 -c 'import pathlib, sys; print(pathlib.Path(sys.argv[1]).resolve())' "$run")
  [[ $(dirname -- "$run") == "$GENERATED_ROOT/runs" ]] || \
    die "run must be directly below $GENERATED_ROOT/runs"
  runs+=("$run")
done

python3 "$BENCHMARK_ROOT/scripts/tool.py" publish-evidence "$report" "${runs[@]}"
