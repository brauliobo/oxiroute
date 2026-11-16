#!/usr/bin/env bash
# Fails when a git-tracked source file exceeds the line limit. Oversized files that predate the limit are recorded in
# scripts/verify/line_limit_baseline.txt ("LINES PATH" per line); the baseline only shrinks.
#
#   line_limit.sh [REPO_DIR]
set -euo pipefail

max_lines=1000

lib=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../lib" && pwd)
root=${1:-$(cd -- "${lib}/../.." && pwd)}
source "${lib}/sources.sh"
source "${lib}/ratchet.sh"

ratchet_unit=lines
ratchet_rule="exceeds the ${max_lines} line limit"
ratchet_ok="verified every tracked source file is within ${max_lines} lines"
ratchet_guidance='Split each oversized file into modules or subfolders grouped by context and responsibility.
Do not compress lines, strip comments or raise the baseline: the baseline only shrinks.'

load_oversized() {
  local path count
  while IFS= read -r -d '' path; do
    count=$(wc -l <"${root}/${path}")
    (( count <= max_lines )) || current[${path}]=${count}
  done < <(tracked_sources "${root}")
}

load_oversized
ratchet_run "${root}/scripts/verify/line_limit_baseline.txt"
