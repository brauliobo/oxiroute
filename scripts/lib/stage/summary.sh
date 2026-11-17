#!/usr/bin/env bash
# Prints the "stage-time NAME SECONDS" lines of the files given (scripts/lib/stage/time.sh) as a Markdown table, the form the
# release workflow appends to the job summary. A file that does not exist (the job failed before its first stage) is skipped.
#
#   stage-summary.sh FILE...
set -euo pipefail

printf '### Stage times\n\n| stage | seconds |\n| --- | ---: |\n'
for file in "$@"; do
  [[ -f ${file} ]] && awk '$1 == "stage-time" { printf "| %s | %d |\n", $2, $3 }' "${file}"
done
exit 0
