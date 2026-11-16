#!/usr/bin/env bash
# Verifies that unsafe code and raw io_uring access stay where they are allowed (docs/developer/ARCHITECTURE.md,
# "Unsafe containment"): every first-party crate forbids unsafe code except the crates of the baseline below, the only
# source file allowed to opt out inside crates/oxiroute-uring is src/raw.rs, and only crates/oxiroute-uring may depend
# on the io-uring crate, enable rustix's io_uring module or call the io_uring system calls.
# The fix for a failure is never to widen this list: move the unsafe code behind the narrow safe API of oxiroute-uring.
set -euo pipefail

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${repo}"

cargo metadata --format-version 1 --no-deps --locked > "${TMPDIR:-/tmp}/oxiroute-unsafe-metadata.$$.json"
trap 'rm -f -- "${TMPDIR:-/tmp}/oxiroute-unsafe-metadata.$$.json"' EXIT

python3 -I - "${repo}" "${TMPDIR:-/tmp}/oxiroute-unsafe-metadata.$$.json" <<'PY'
import json
import re
import sys
import tomllib
from pathlib import Path

repo = Path(sys.argv[1])
metadata = json.loads(Path(sys.argv[2]).read_text())

# Crates that were already allowed to opt out before the io_uring crate (scoped `#[allow(unsafe_code)]` under a
# crate-level `deny`), and the one crate that exists for io_uring. A new entry needs a design review, not an edit here.
OPT_OUT = {'oxiroute-allocator', 'oxiroute-cpu', 'oxiroute-uring'}
URING_CRATE = 'oxiroute-uring'
URING_UNSAFE_FILES = {'crates/oxiroute-uring/src/raw.rs'}
RAW_URING = re.compile(r'io_uring_setup|io_uring_enter|io_uring_register|SYS_io_uring|rustix::io_uring|\bio_uring::')

failures = []
first_party = [p for p in metadata['packages'] if p['manifest_path'].startswith(str(repo / 'crates'))]
assert first_party, 'no first-party crates found'

def lint_level(manifest):
    lints = manifest.get('lints', {})
    if lints.get('workspace') is True:
        return 'forbid'
    return lints.get('rust', {}).get('unsafe_code')

for package in first_party:
    name = package['name']
    manifest = tomllib.loads(Path(package['manifest_path']).read_text())
    level = lint_level(manifest)
    if name in OPT_OUT:
        if level not in {'deny', 'forbid'}:
            failures.append(f'{name}: unsafe_code is {level!r}; an opt-out crate keeps it at deny')
    elif level != 'forbid':
        failures.append(f'{name}: unsafe_code is {level!r}; every crate outside the baseline forbids unsafe code')
    uses_uring = any(dep['name'] in {'io-uring', 'io_uring'} for dep in package['dependencies'])
    if uses_uring and name != URING_CRATE:
        failures.append(f'{name}: depends on the io-uring crate; use the safe API of {URING_CRATE}')

for package in first_party:
    if package['name'] == URING_CRATE:
        continue
    root = Path(package['manifest_path']).parent
    for path in sorted(root.rglob('*.rs')):
        if 'target' in path.parts:
            continue
        for number, line in enumerate(path.read_text().splitlines(), 1):
            if RAW_URING.search(line) and not line.lstrip().startswith(('//', '///')):
                failures.append(f'{path.relative_to(repo)}:{number}: raw io_uring access outside {URING_CRATE}')

workspace = tomllib.loads((repo / 'Cargo.toml').read_text())
rustix = workspace.get('workspace', {}).get('dependencies', {}).get('rustix', {})
if isinstance(rustix, dict) and 'io_uring' in rustix.get('features', []):
    failures.append('workspace rustix enables its io_uring module; only oxiroute-uring may use raw io_uring')

for package in first_party:
    if package['name'] != URING_CRATE:
        continue
    root = Path(package['manifest_path']).parent
    allowing = {
        str(path.relative_to(repo))
        for path in root.rglob('*.rs')
        if re.search(r'allow\(unsafe_code\)', path.read_text())
    }
    if allowing != URING_UNSAFE_FILES:
        failures.append(f'{URING_CRATE}: allow(unsafe_code) is in {sorted(allowing)}, expected {sorted(URING_UNSAFE_FILES)}')
    raw = (root / 'src/raw.rs').read_text()
    blocks = len(re.findall(r'\bunsafe\s*\{', raw))
    documented = len(re.findall(r'// SAFETY:', raw))
    if blocks != documented:
        failures.append(f'{URING_CRATE}: raw.rs has {blocks} unsafe blocks and {documented} SAFETY comments')

if failures:
    print('unsafe containment violations:', *failures, sep='\n  ', file=sys.stderr)
    sys.exit(1)
print(f'unsafe containment holds: {len(first_party)} crates checked, opt-outs {sorted(OPT_OUT)}')
PY
