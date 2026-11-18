#!/usr/bin/env bash
# Exercise the real scanners on both baseline and native builds; QEMU covers CPUs lacking optional instructions.
# Under QEMU runs only what proves the runtime dispatch (every oxiroute-cpu test and the h1-head dispatch binary); the
# exhaustive comparisons of the other test binaries run natively in the core tier.
set -euo pipefail
ulimit -c 0

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf -- "${work}"' EXIT
cd "${repo}"
target=${CARGO_TARGET_DIR:-${repo}/target}/cpu-targets
qemu=${QEMU_X86_64:-qemu-x86_64}
command -v "${qemu}" > /dev/null || { echo "${qemu} is required for CPU dispatch tests" >&2; exit 1; }

read -ra emulated <<< "${CPU_TEST_BINARIES:-oxiroute-cpu oxiroute-h1-head:dispatch}"

RUSTFLAGS='-C target-cpu=x86-64' CARGO_TARGET_DIR=${target}/generic \
  nice cargo test --release --locked -p oxiroute-cpu -p oxiroute-h1-head --no-run --jobs 4 \
    --message-format=json > "${work}/build.jsonl"
python3 - "${work}/build.jsonl" "${emulated[@]}" > "${work}/tests" <<'PY'
import json
import sys
from pathlib import Path

wanted = {entry.partition(':')[0]: entry.partition(':')[2] for entry in sys.argv[2:]}
found = set()
with open(sys.argv[1]) as output:
    for line in output:
        artifact = json.loads(line)
        if artifact.get('reason') != 'compiler-artifact' or not artifact.get('executable') or not artifact['profile']['test']:
            continue
        package = Path(artifact['manifest_path']).parent.name
        target = artifact['target']['name']
        if package in wanted and wanted[package] in ('', target):
            found.add(package + (':' + target if wanted[package] else ''))
            print(package, artifact['executable'], sep='\t')
missing = [entry for entry in sys.argv[2:] if entry not in found]
if missing:
    sys.exit('no test executable was built for: ' + ' '.join(missing))
PY
default_cpus='qemu64=sse2 Nehalem=sse42 Haswell=avx2'
default_masks='Haswell,-popcnt,-bmi1,-bmi2=avx2'
default_masks+=' Haswell,-sse3,-ssse3,-sse4.1,-sse4.2=sse2 Haswell,-sse4.1,-sse4.2=sse42'
read -ra cpus <<< "${CPU_TEST_CPUS:-${default_cpus}}"
read -ra masks <<< "${CPU_TEST_MASKED_CPUS-${default_masks}}"
loader_hint='the host loader or libc needs a newer instruction set; set QEMU_LD_PREFIX to a baseline sysroot'
loader_hint+=' (docs/developer/RELEASING.md)'
while IFS=$'\t' read -r package binary; do
  selected_cpus=("${cpus[@]}")
  if [[ ${package} == oxiroute-cpu ]]; then
    selected_cpus+=("${masks[@]}")
  fi
  for spec in "${selected_cpus[@]}"; do
    cpu=${spec%=*} expected=${spec##*=}
    [[ ${cpu} != "${expected}" ]] || { echo "CPU_TEST_CPUS entries must be CPU=LEVEL" >&2; exit 1; }
    printf 'CPU dispatch tests: %s on %s\n' "$(basename "${binary}")" "${cpu}"
    "${qemu}" -cpu "${cpu}" "${binary}" --list > /dev/null 2>&1 || { echo "${package} cannot start on ${cpu}: ${loader_hint}" >&2; exit 1; }
    OXIROUTE_CPU_LEVEL=avx512 OXIROUTE_EXPECT_CPU_LEVEL=${expected} OXIROUTE_EXPECT_SELECTED_LEVEL=${expected} \
      "${qemu}" -cpu "${cpu}" "${binary}" --test-threads=1 --nocapture
  done
done < "${work}/tests"
native=${target}/native-$("${repo}/scripts/lib/cpu_id.sh")
printf 'native build in %s on: %s\n' "${native}" "$(grep -m1 '^model name' /proc/cpuinfo)"
OXIROUTE_CPU_LEVEL=avx512 RUSTFLAGS='-C target-cpu=native' CARGO_TARGET_DIR=${native} \
  nice cargo test --release --locked -p oxiroute-cpu -p oxiroute-h1-head --jobs 4 -- --test-threads=4
