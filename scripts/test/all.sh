#!/usr/bin/env bash
# Every lane of the pull request gate, one after the other, and a verdict for each: what CI runs in its test jobs.
set -uo pipefail

lanes=(core.sh "planes.sh all" "tls.sh all" doc.sh)
status=0
cd "$(dirname -- "${BASH_SOURCE[0]}")" || exit

for lane in "${lanes[@]}"; do
  # shellcheck disable=SC2086
  if ./${lane}; then
    printf 'lane %s: passed\n' "${lane}"
  else
    printf 'lane %s: FAILED\n' "${lane}"
    status=1
  fi
done
exit "${status}"
