#!/usr/bin/env bash
# The one owner of the test tiers (scripts/ci/tiers.txt). Every test binary of the workspace is listed once with the
# tiers it runs in, and the lanes (scripts/test/*.sh) select binaries by tier, so a binary cannot be skipped by one lane
# and unrun by another. A line `BINARY TIER[,TIER...] TEST` moves the tests of BINARY whose name contains TEST to those tiers.
#
#   tiers.sh check          fail when a test binary is in no tier, a tier is unknown, or an entry names no binary
#   tiers.sh filter TIER... print the nextest filterset that selects the binaries and the moved tests of the tiers
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
table="${repo_dir}/scripts/ci/tiers.txt"
known_tiers=(core planes engine epoll uring tls slow)

entries() {
  grep -Ev '^[[:space:]]*(#|$)' "${table}"
}

binary_entries() {
  entries | awk 'NF == 2'
}

workspace_binaries() {
  cargo metadata --format-version 1 --no-deps --locked --manifest-path "${repo_dir}/Cargo.toml" | jq -r '
    .packages[] | .name as $package | .targets[] | select(.test)
    | if (.kind | index("test")) then "\($package)::\(.name)"
      elif (.kind | index("bin")) then "\($package)::bin/\(.name)"
      else $package end'
}

report() {
  local heading=$1 lines=$2
  [[ -z ${lines} ]] || printf '%s\n%s\n' "${heading}" "${lines}" >&2
}

check() {
  local unknown duplicates missing stale orphans listed
  listed=$(binary_entries | awk '{ print $1 }' | sort)
  unknown=$(entries | awk -v known="${known_tiers[*]}" '
    BEGIN { n = split(known, k, " "); for (i = 1; i <= n; i++) ok[k[i]] = 1 }
    { n = split($2, tiers, ","); for (i = 1; i <= n; i++) if (!(tiers[i] in ok)) print $1 " " tiers[i] }')
  duplicates=$(uniq -d <<<"${listed}")
  missing=$(comm -23 <(workspace_binaries | sort -u) <(sort -u <<<"${listed}"))
  stale=$(comm -13 <(workspace_binaries | sort -u) <(sort -u <<<"${listed}"))
  orphans=$(entries | awk 'NF == 3 { print $1 }' | sort -u | comm -23 - <(sort -u <<<"${listed}"))
  report 'entries naming an unknown tier (known: '"${known_tiers[*]}"'):' "${unknown}"
  report 'test binaries listed more than once:' "${duplicates}"
  report 'test binaries in no tier (add each to scripts/ci/tiers.txt with the tiers of what it exercises):' "${missing}"
  report 'entries that name no test binary (remove them):' "${stale}"
  report 'slow test entries whose binary has no line of its own:' "${orphans}"
  [[ -z ${unknown}${duplicates}${missing}${stale}${orphans} ]] || exit 1
  printf 'verified every test binary is in a tier (%s binaries)\n' "$(sort -u <<<"${listed}" | wc -l)"
}

filter() {
  (($# > 0)) || { printf 'usage: tiers.sh filter TIER...\n' >&2; exit 2; }
  local tier
  for tier in "$@"; do
    [[ " ${known_tiers[*]} " == *" ${tier} "* ]] || { printf 'unknown tier: %s\n' "${tier}" >&2; exit 2; }
  done
  entries | awk -v wanted="$*" '
    BEGIN { n = split(wanted, w, " "); for (i = 1; i <= n; i++) pick[w[i]] = 1 }
    function add(list, item) { return list == "" ? item : list " | " item }
    function picked(tiers,   n, t, i) {
      n = split(tiers, t, ",")
      for (i = 1; i <= n; i++) if (t[i] in pick) return 1
      return 0
    }
    NF == 3 {
      test = "(binary_id(=" $1 ") & test(~" $3 "))"
      moved = add(moved, test)
      if (picked($2)) rows = add(rows, test)
      next
    }
    picked($2) { binaries = add(binaries, "binary_id(=" $1 ")") }
    END {
      if (binaries == "" && rows == "") exit 3
      selected = binaries
      if (binaries != "" && moved != "") selected = "(" binaries ") & !(" moved ")"
      if (rows != "") selected = add(selected, rows)
      print selected
    }'
}

case ${1:-} in
  check) check ;;
  filter) shift; filter "$@" ;;
  *) printf 'usage: tiers.sh check | filter TIER...\n' >&2; exit 2 ;;
esac
