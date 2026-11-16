#!/usr/bin/env bash
# Fails when two or more git-tracked source files in one directory share a leading name token (the stem before its first
# "_" or "-"). Existing groups are recorded in scripts/verify/name_prefixes_baseline.txt ("FILES DIR/TOKEN" per line);
# the baseline only shrinks.
#
#   name_prefixes.sh [REPO_DIR]
set -euo pipefail

lib=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../lib" && pwd)
root=${1:-$(cd -- "${lib}/../.." && pwd)}
source "${lib}/sources.sh"
source "${lib}/ratchet.sh"

ratchet_unit=files
ratchet_rule='share a leading name prefix'
ratchet_ok='verified no directory holds source files sharing a name prefix beyond the baseline'
ratchet_guidance='Move the files into a subfolder named by the shared prefix and drop the prefix from each file name
(process_management.rs becomes process/management.rs). Do not rename around the check or raise the baseline: the
baseline only shrinks.'

prefix_group() {
  local dir base stem token
  dir=$(dirname -- "$1")
  base=$(basename -- "$1")
  stem=${base%.*}
  token=${stem%%[_-]*}
  [[ -n ${token} && ${token} != "${stem}" ]] && printf '%s/%s\n' "${dir}" "${token}"
  return 0
}

load_groups() {
  local path group
  local -A members
  while IFS= read -r -d '' path; do
    group=$(prefix_group "${path}")
    [[ -z ${group} ]] || members[${group}]=$(( ${members[${group}]:-0} + 1 ))
  done < <(tracked_sources "${root}")
  for group in "${!members[@]}"; do
    (( members[${group}] < 2 )) || current[${group}]=${members[${group}]}
  done
}

load_groups
ratchet_run "${root}/scripts/verify/name_prefixes_baseline.txt"
