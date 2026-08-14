#!/usr/bin/env bash
set -euo pipefail

workspace=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
mode=${1:-verify}
baseline_commit=2d9c5fe66cd096d7a1d8e3bada8d5784b5f97f6c
instrumentation=${workspace}/docs/developer/fixtures/generation-blueprint-instrumentation-2d9c5fe.patch
harness=${workspace}/docs/developer/fixtures/generation-blueprint-harness-2d9c5fe.rs
baseline=${workspace}/docs/developer/fixtures/generation-blueprint-2d9c5fe.json
instrumentation_sha256=a782d567f8ceab4749099c84d16e60b2f9da57185bd7cc2f0a96115c980f7adb
harness_sha256=52fd29c357930fe8df89fcbbf85d884932a298c3ffd6863ba3f54e288fe72dd8
temporary_root=/tmp/opencode/oxiroute-generation-baseline-${baseline_commit}-${$}

if [[ "${mode}" != verify && "${mode}" != --update && "${mode}" != --adversarial-self-test ]]; then
  printf 'usage: %s [--update|--adversarial-self-test]\n' "${0}" >&2
  exit 2
fi

cleanup() { rm -rf -- "${temporary_root}"; }
trap cleanup EXIT

verify_digest() {
  local file=$1 expected=$2 label=$3 actual
  actual=$(sha256sum "${file}")
  actual=${actual%% *}
  [[ "${actual}" == "${expected}" ]] || {
    printf '%s digest mismatch: expected %s, got %s\n' "${label}" "${expected}" "${actual}" >&2
    return 1
  }
}

verify_digest "${instrumentation}" "${instrumentation_sha256}" 'baseline instrumentation'
verify_digest "${harness}" "${harness_sha256}" 'baseline harness'

if [[ "${mode}" == --adversarial-self-test ]]; then
  cleanup
  mkdir -p "${temporary_root}/adversarial"
  cp "${instrumentation}" "${temporary_root}/adversarial/instrumentation.patch"
  cp "${harness}" "${temporary_root}/adversarial/harness.rs"
  printf '\n# mutation\n' >> "${temporary_root}/adversarial/instrumentation.patch"
  printf '\n// mutation\n' >> "${temporary_root}/adversarial/harness.rs"
  ! verify_digest "${temporary_root}/adversarial/instrumentation.patch" "${instrumentation_sha256}" mutated 2>/dev/null
  ! verify_digest "${temporary_root}/adversarial/harness.rs" "${harness_sha256}" mutated 2>/dev/null
  printf 'adversarial generation-baseline digest checks passed\n'
  exit 0
fi

resolved=$(git -C "${workspace}" rev-parse "${baseline_commit}^{commit}")
[[ "${resolved}" == "${baseline_commit}" ]] || {
  printf 'baseline commit mismatch: expected %s, resolved %s\n' "${baseline_commit}" "${resolved}" >&2
  exit 1
}

cleanup
mkdir -p "${temporary_root}/source" "${temporary_root}/generated"
git -C "${workspace}" archive "${baseline_commit}" | tar -x -C "${temporary_root}/source"
git -C "${temporary_root}/source" apply --check "${instrumentation}"
git -C "${temporary_root}/source" apply "${instrumentation}"
cp "${harness}" "${temporary_root}/source/crates/oxiroute-server/src/generation_baseline.rs"
printf '\n#[cfg(test)]\nmod generation_baseline;\n' >> "${temporary_root}/source/crates/oxiroute-server/src/lib.rs"

output=${temporary_root}/generated/test-output.txt
generated=${temporary_root}/generated/generation-blueprint-2d9c5fe.json
CARGO_TARGET_DIR="${workspace}/target" nice cargo +1.97.1 test \
  --manifest-path "${temporary_root}/source/Cargo.toml" \
  -p oxiroute --lib generation_baseline::emit_authenticated_generation_baseline \
  --locked -j 4 -- --nocapture > "${output}"
node - "${output}" "${generated}" <<'NODE'
const fs = require('node:fs')
const [input, output] = process.argv.slice(2)
const text = fs.readFileSync(input, 'utf8')
const begin = text.indexOf('OXIROUTE_GENERATION_BASELINE_BEGIN\n')
const end = text.indexOf('\nOXIROUTE_GENERATION_BASELINE_END', begin)
if (begin < 0 || end < 0) throw new Error('baseline payload markers missing')
const payload = text.slice(begin + 'OXIROUTE_GENERATION_BASELINE_BEGIN\n'.length, end)
const parsed = JSON.parse(payload)
fs.writeFileSync(output, `${JSON.stringify(parsed, null, 2)}\n`)
NODE

if [[ "${mode}" == --update ]]; then
  cp "${generated}" "${baseline}"
else
  cmp "${generated}" "${baseline}"
fi

CARGO_TARGET_DIR="${workspace}/target" nice cargo +1.97.1 test \
  --manifest-path "${workspace}/Cargo.toml" \
  -p oxiroute --lib authenticated_2d9c5fe_runtime_parity \
  --locked -j 4

printf 'authenticated generation baseline verified at %s\n' "${baseline_commit}"
