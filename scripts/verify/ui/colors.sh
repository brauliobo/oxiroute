#!/usr/bin/env bash
# Fails when a console stylesheet gains a hard-coded colour (hex, rgb(), hsl() and friends). Existing ones are recorded in
# scripts/verify/ui/baselines/colors.txt ("COUNT PATH" per line); the baseline only shrinks. The design system and the
# stats page hold no colour outside ui/src/design/tokens: stylelint enforces that without a baseline.
#
#   colors.sh [REPO_DIR]
#   colors.sh --print       print the current counts in baseline format
set -euo pipefail

lib=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../lib" && pwd)
root=$(cd -- "${lib}/../.." && pwd)
[[ ${1:-} == --print || -z ${1:-} ]] || root=$1
source "${lib}/ui_css.sh"
source "${lib}/ratchet.sh"

ratchet_unit=colours
ratchet_rule='are hard-coded in a stylesheet'
ratchet_ok='verified no console stylesheet gains a hard-coded colour'
ratchet_guidance='Use a colour token (var(--ox-fg), var(--ox-surface), var(--ox-ok), ...) from ui/src/design/tokens/color.css,
or a design primitive that already carries the colour. Do not add a new colour or raise the baseline: the baseline only
shrinks.'

load_colors() {
  local path count
  while IFS= read -r -d '' path; do
    count=$(declaration_text "${root}" "${path}" | { grep -oE '#[0-9a-fA-F]{3,8}\b|\b(rgba?|hsla?|hwb|lab|lch|oklab|oklch)\(' || true; } | wc -l)
    (( count == 0 )) || current[${path}]=${count}
  done < <(console_styles "${root}")
}

load_colors
if [[ ${1:-} == --print ]]; then
  for path in $(ratchet_keys current); do printf '%s %s\n' "${current[${path}]}" "${path}"; done
  exit 0
fi
ratchet_run "$(dirname -- "${BASH_SOURCE[0]}")/baselines/colors.txt"
