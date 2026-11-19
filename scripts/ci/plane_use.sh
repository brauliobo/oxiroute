#!/usr/bin/env bash
# Fails when a test of the planes tier never chooses an HTTP data plane. A test that does not read the plane (the test
# harness records the tests that do in the file named by OXIROUTE_TEST_PLANE_USE_FILE) runs identically on every plane,
# so running it on all of them only repeats it. Reads the JUnit reports nextest wrote for the lane and scripts/ci/tiers.txt.
#
#   plane_use.sh check REPORT JUNIT...
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
if (($# < 2)) || [[ $1 != check ]]; then
  printf 'usage: plane_use.sh check REPORT JUNIT...\n' >&2
  exit 2
fi
shift

python3 - "${repo_dir}/scripts/ci/tiers.txt" "$@" <<'PY'
import sys
import xml.etree.ElementTree as ET

table, report, *junit = sys.argv[1:]
binaries, moved = {}, []
for line in open(table):
    fields = line.split()
    if not fields or fields[0].startswith('#'):
        continue
    if len(fields) == 2:
        binaries[fields[0]] = set(fields[1].split(','))
    else:
        moved.append((fields[0], set(fields[1].split(',')), fields[2]))

chose = {line.strip() for line in open(report)}
ran = [(case.get('classname'), case.get('name')) for path in junit for case in ET.parse(path).iter('testcase')]


def tiers(binary, name):
    rows = [row for row in moved if row[0] == binary and row[2] in name]
    return set().union(*(row[1] for row in rows)) if rows else binaries.get(binary, set())


planes = [(binary, name) for binary, name in ran if 'planes' in tiers(binary, name)]
idle = sorted(f'{binary} {name}' for binary, name in planes if f'{binary} {name}' not in chose)
unmatched = sorted(
    f'{binary} {",".join(sorted(tier_set))} {test}'
    for binary, tier_set, test in moved
    if 'planes' in tier_set and not any(b == binary and test in name for b, name in ran)
)
if idle:
    print('tests of the planes tier that never choose a data plane:', file=sys.stderr)
    print('\n'.join(idle), file=sys.stderr)
    print('Each of them is plane-independent: it runs the same on every plane, so tier it core (or tls) in\n'
          'scripts/ci/tiers.txt with a `BINARY core TEST` line, which moves that test and leaves the others of its binary.\n'
          'Do not make a test read the plane only to stay here.', file=sys.stderr)
if unmatched:
    print('moved test lines of the planes tier that match no test:', file=sys.stderr)
    print('\n'.join(unmatched), file=sys.stderr)
if idle or unmatched:
    sys.exit(1)
print(f'every one of the {len(planes)} tests of the planes tier chooses a data plane')
PY
