#!/usr/bin/env bash
# Exercise the real scanners on both baseline and native builds; QEMU covers CPUs lacking optional instructions.
set -euo pipefail
ulimit -c 0

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf -- "${work}"' EXIT
cd "${repo}"
target=${CARGO_TARGET_DIR:-${repo}/target}/cpu-targets
qemu=${QEMU_X86_64:-qemu-x86_64}
command -v "${qemu}" > /dev/null || { echo "${qemu} is required for CPU dispatch tests" >&2; exit 1; }

RUSTFLAGS='-C target-cpu=x86-64' CARGO_TARGET_DIR=${target}/generic \
  nice cargo test --release --locked -p oxiroute-cpu -p oxiroute-h1-head --no-run --jobs 4 \
    --message-format=json > "${work}/build.jsonl"
python3 - "${work}/build.jsonl" > "${work}/tests" <<'PY'
import json
import sys
from pathlib import Path

with open(sys.argv[1]) as output:
    for line in output:
        artifact = json.loads(line)
        if artifact.get('reason') == 'compiler-artifact' and artifact.get('executable') and artifact['profile']['test']:
            print(Path(artifact['manifest_path']).parent.name, artifact['executable'], sep='\t')
PY
[[ -s ${work}/tests ]] || { echo 'no CPU test executables were built' >&2; exit 1; }
default_cpus='qemu64=sse2 Nehalem=sse42 Haswell=avx2'
default_masks='Haswell,-popcnt,-bmi1,-bmi2=avx2'
default_masks+=' Haswell,-sse3,-ssse3,-sse4.1,-sse4.2=sse2 Haswell,-sse4.1,-sse4.2=sse42'
read -ra cpus <<< "${CPU_TEST_CPUS:-${default_cpus}}"
read -ra masks <<< "${CPU_TEST_MASKED_CPUS-${default_masks}}"
while IFS=$'\t' read -r package binary; do
  selected_cpus=("${cpus[@]}")
  if [[ ${package} == oxiroute-cpu ]]; then
    selected_cpus+=("${masks[@]}")
  fi
  for spec in "${selected_cpus[@]}"; do
    cpu=${spec%=*} expected=${spec##*=}
    [[ ${cpu} != "${expected}" ]] || { echo "CPU_TEST_CPUS entries must be CPU=LEVEL" >&2; exit 1; }
    printf 'CPU dispatch tests: %s on %s\n' "$(basename "${binary}")" "${cpu}"
    OXIROUTE_CPU_LEVEL=avx512 OXIROUTE_EXPECT_CPU_LEVEL=${expected} OXIROUTE_EXPECT_SELECTED_LEVEL=${expected} \
      "${qemu}" -cpu "${cpu}" "${binary}" --test-threads=1 --nocapture
  done
done < "${work}/tests"
OXIROUTE_CPU_LEVEL=avx512 RUSTFLAGS='-C target-cpu=native' CARGO_TARGET_DIR=${target}/native \
  nice cargo test --release --locked -p oxiroute-cpu -p oxiroute-h1-head --jobs 4 -- --test-threads=4
