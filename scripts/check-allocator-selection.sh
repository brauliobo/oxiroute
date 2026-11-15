#!/usr/bin/env bash
set -euo pipefail

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf -- "${work}"' EXIT
cd "${repo}"
command -v nm > /dev/null

check_graph() {
  local backend=$1
  shift
  cargo tree --locked -p oxiroute -e normal,build --prefix none --format '{p}' "$@" > "${work}/tree"
  python3 - "${backend}" "${work}/tree" <<'PY'
import sys
from pathlib import Path

packages = {line.split()[0] for line in Path(sys.argv[2]).read_text().splitlines()}
jemalloc = packages & {'tikv-jemallocator', 'tikv-jemalloc-sys'}
expected = {'tikv-jemallocator', 'tikv-jemalloc-sys'} if sys.argv[1] == 'jemalloc' else set()
assert jemalloc == expected, f'allocator dependency mismatch: {jemalloc}, expected {expected}'
PY
}

check_allocator() {
  local backend=$1
  shift
  if ! nice cargo test --release --locked -p oxiroute-allocator --jobs 4 --message-format=json "$@" \
    -- --test-threads=1 > "${work}/tests.jsonl"; then
    cat "${work}/tests.jsonl" >&2
    return 1
  fi
  awk '/^test result:/ {print}' "${work}/tests.jsonl"
  python3 - "${work}/tests.jsonl" > "${work}/executable" <<'PY'
import json
import sys

for line in open(sys.argv[1]):
    if not line.startswith('{'):
        continue
    artifact = json.loads(line)
    if (artifact.get('reason') == 'compiler-artifact' and artifact['target']['name'] == 'selection'
            and artifact.get('executable')):
        print(artifact['executable'])
PY
  [[ $(wc -l < "${work}/executable") == 1 ]]
  check_linkage "${backend}" "$(cat "${work}/executable")"
  printf 'Allocator tests and actual executable linkage: %s\n' "${backend}"
}

check_linkage() {
  local backend=$1 binary=$2
  nm --defined-only "${binary}" > "${work}/symbols"
  python3 - "${backend}" "${work}/symbols" <<'PY'
import sys
from pathlib import Path

symbols = {line.split()[-1] for line in Path(sys.argv[2]).read_text().splitlines() if line.split()}
assert symbols, 'allocator linkage is unverifiable: executable has no defined symbols'
linked = any(symbol.startswith('_rjem_') for symbol in symbols)
assert linked == (sys.argv[1] == 'jemalloc'), f'unexpected jemalloc linkage: {linked}'
PY
}

[[ $(uname -s) == Linux ]] || { echo 'Allocator linkage checks require Linux' >&2; exit 1; }
[[ ! -v _RJEM_MALLOC_CONF ]] || { echo 'Unset _RJEM_MALLOC_CONF to test embedded defaults' >&2; exit 1; }
check_graph jemalloc
check_graph jemalloc --features alloc-count
check_graph system --no-default-features
check_graph system --no-default-features --features alloc-count
check_graph jemalloc --no-default-features --features heap-profile
check_allocator jemalloc
check_allocator system --no-default-features
check_allocator jemalloc --features count
check_allocator system --no-default-features --features count
if [[ -v SYSTEM_ALLOCATOR_BINARY ]]; then
  check_linkage system "${SYSTEM_ALLOCATOR_BINARY}"
fi
