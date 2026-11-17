#!/usr/bin/env bash
# Fails when a test of a pull request lane ran longer than the budget, so that a slow test cannot creep into the fast
# tiers. Reads the JUnit reports nextest wrote (scripts/ci/nextest.toml).
#
#   test_budget.sh REPORT...       OXIROUTE_TEST_BUDGET seconds per test (default 30; `none` turns the check off)
set -euo pipefail

budget=${OXIROUTE_TEST_BUDGET:-30}
[[ ${budget} != none ]] || exit 0
(($# > 0)) || { printf 'usage: test_budget.sh REPORT...\n' >&2; exit 2; }

over_budget() {
  python3 - "${budget}" "$@" <<'PY'
import sys
import xml.etree.ElementTree as ET

budget = float(sys.argv[1])
for report in sys.argv[2:]:
    for case in ET.parse(report).iter('testcase'):
        seconds = float(case.get('time'))
        if seconds > budget:
            print(f"{seconds:8.1f}s  {case.get('classname')} {case.get('name')}")
PY
}

slow=$(over_budget "$@" | sort -rn)
[[ -z ${slow} ]] || {
  printf 'tests over the %ss budget of the pull request lanes:\n%s\n' "${budget}" "${slow}" >&2
  printf '%s\n' 'Make each test fast at its root (smaller inputs that keep the property, independent cases run side by' \
    'side, waits on events instead of fixed sleeps), or, when the duration is a real wait of a default timeout, move' \
    'it to the slow tier in scripts/ci/tiers.txt. Do not raise the budget.' >&2
  exit 1
}
printf 'every test ran within the %ss budget\n' "${budget}"
