#!/usr/bin/env bash
# Fails when a git-tracked source file exceeds the line limit. Oversized files that predate the limit are recorded in
# scripts/file-size-baseline.txt ("LINES PATH" per line); the baseline only shrinks: a baselined file may not grow, and
# an entry whose file is within the limit (or gone) must be removed.
#
#   verify-file-size.sh [REPO_DIR]
set -euo pipefail

max_lines=1000
extensions='rs|ts|vue|js|mjs|py|sh'
excluded='^(vendor|fuzz|website|remotion|target)/|^ui/src/(api|stats)/generated/'
excluded+='|^crates/oxiroute-server/src/observability/stats_page/assets/table\.rs$'

root=${1:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)}
baseline=${root}/scripts/file-size-baseline.txt
declare -A lines recorded
status=0

fail() { printf '%s\n' "$1" >&2; status=1; }

keys() {
  local -n map=$1
  printf '%s\n' "${!map[@]}" | sed '/^$/d' | LC_ALL=C sort
}

tracked_sources() {
  git -C "${root}" ls-files -z | grep -zE "\.(${extensions})\$" | grep -zvE "${excluded}"
}

load_lines() {
  local path
  while IFS= read -r -d '' path; do
    lines[${path}]=$(wc -l <"${root}/${path}")
  done < <(tracked_sources)
}

load_recorded() {
  local count path
  while read -r count path; do
    [[ -z ${path} ]] || recorded[${path}]=${count}
  done <"${baseline}"
}

check_sources() {
  local path count
  while read -r path; do
    count=${lines[${path}]}
    (( count > max_lines )) || continue
    if [[ -z ${recorded[${path}]:-} ]]; then
      fail "${path}: ${count} lines exceeds the ${max_lines} line limit and is not in the baseline"
    elif (( count > recorded[${path}] )); then
      fail "${path}: grew from ${recorded[${path}]} to ${count} lines"
    fi
  done < <(keys lines)
}

check_baseline() {
  local path
  while read -r path; do
    (( ${lines[${path}]:-0} > max_lines )) || fail "${path}: stale baseline entry, within ${max_lines} lines or gone"
  done < <(keys recorded)
}

load_lines
load_recorded
check_sources
check_baseline

if (( status != 0 )); then
  printf 'Split each oversized file into modules or subfolders grouped by context and responsibility.\n' >&2
  printf 'Do not compress lines, strip comments or raise the baseline: the baseline only shrinks.\n' >&2
  exit "${status}"
fi
printf 'verified every tracked source file is within %s lines\n' "${max_lines}"
