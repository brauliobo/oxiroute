#!/usr/bin/env bash
# Fails when a console class selector is styled in more files than recorded. The same class styled in several files is a
# primitive that belongs in ui/src/design. Existing ones are recorded in scripts/verify/ui/baselines/duplicates.txt
# ("FILES CLASS" per line); the baseline only shrinks.
#
#   duplicates.sh [REPO_DIR]
#   duplicates.sh --print       print the current counts in baseline format
set -euo pipefail

lib=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../lib" && pwd)
root=$(cd -- "${lib}/../.." && pwd)
[[ ${1:-} == --print || -z ${1:-} ]] || root=$1
source "${lib}/ui_css.sh"
source "${lib}/ratchet.sh"

ratchet_unit=files
ratchet_rule='style the same class'
ratchet_ok='verified no console class is styled in more files than recorded'
ratchet_guidance='Move the shared styling into a design primitive under ui/src/design (a Pug component with its own styles.css)
and use the primitive from every page. Do not restyle a class in another file or raise the baseline: the baseline
only shrinks.'

load_duplicates() {
  local path class
  local -A files
  while IFS= read -r -d '' path; do
    while IFS= read -r class; do
      files[${class}]=$(( ${files[${class}]:-0} + 1 ))
    done < <(selector_text "${root}" "${path}" | { grep -oE '\.[A-Za-z_][A-Za-z0-9_-]*' || true; } | sort -u)
  done < <(console_styles "${root}")
  for class in "${!files[@]}"; do
    (( files[${class}] < 2 )) || current[${class}]=${files[${class}]}
  done
}

load_duplicates
if [[ ${1:-} == --print ]]; then
  for class in $(ratchet_keys current); do printf '%s %s\n' "${current[${class}]}" "${class}"; done
  exit 0
fi
ratchet_run "$(dirname -- "${BASH_SOURCE[0]}")/baselines/duplicates.txt"
