#!/usr/bin/env bash
set -euo pipefail

workspace=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
mode=${1:-verify}
toolchain=1.97.1
target=x86_64-unknown-linux-gnu
baseline_commit=2d9c5fe66cd096d7a1d8e3bada8d5784b5f97f6c
baseline_canonicalizer=${workspace}/docs/developer/fixtures/public-api-canonicalizer-v4.mjs
candidate_canonicalizer=${workspace}/scripts/rtmp-public-api-inventory.mjs
baseline_canonicalizer_sha256=896c406527e412456f4f3a51281ced1363331def95e90b99b086f00726ac39e5
candidate_canonicalizer_sha256=2545bc342e1042f0f4986875fd7f1944f61715f93eecd61cc42e190edd9a08f1
temporary_root=/tmp/opencode/oxiroute-public-api-baseline-${baseline_commit}

if [[ "${mode}" != verify && "${mode}" != --update && "${mode}" != --adversarial-self-test ]]; then
  printf 'usage: %s [--update|--adversarial-self-test]\n' "${0}" >&2
  exit 2
fi

cleanup() {
  rm -rf -- "${temporary_root}"
}
trap cleanup EXIT

verify_digest() {
  local file=$1
  local expected=$2
  local label=$3
  local actual
  actual=$(sha256sum "${file}")
  actual=${actual%% *}
  [[ "${actual}" == "${expected}" ]] || {
    printf '%s digest mismatch: expected %s, got %s\n' "${label}" "${expected}" "${actual}" >&2
    return 1
  }
}

verify_digest "${baseline_canonicalizer}" "${baseline_canonicalizer_sha256}" "baseline canonicalizer"
verify_digest "${candidate_canonicalizer}" "${candidate_canonicalizer_sha256}" "candidate canonicalizer"

if [[ "${mode}" == --adversarial-self-test ]]; then
  cleanup
  mkdir -p "${temporary_root}/adversarial"
  baseline_mutation="${temporary_root}/adversarial/baseline.mjs"
  candidate_mutation="${temporary_root}/adversarial/candidate.mjs"
  cp "${baseline_canonicalizer}" "${baseline_mutation}"
  cp "${candidate_canonicalizer}" "${candidate_mutation}"
  printf '\n// adversarial mutation\n' >> "${baseline_mutation}"
  printf '\n// adversarial mutation\n' >> "${candidate_mutation}"
  if verify_digest "${baseline_mutation}" "${baseline_canonicalizer_sha256}" "mutated baseline canonicalizer" 2>/dev/null; then
    printf 'mutated baseline canonicalizer unexpectedly passed digest verification\n' >&2
    exit 1
  fi
  if verify_digest "${candidate_mutation}" "${candidate_canonicalizer_sha256}" "mutated candidate canonicalizer" 2>/dev/null; then
    printf 'mutated candidate canonicalizer unexpectedly passed digest verification\n' >&2
    exit 1
  fi
  printf 'adversarial canonicalizer digest checks passed\n'
  exit 0
fi

resolved=$(git -C "${workspace}" rev-parse "${baseline_commit}^{commit}")
[[ "${resolved}" == "${baseline_commit}" ]] || {
  printf 'baseline commit mismatch: expected %s, resolved %s\n' "${baseline_commit}" "${resolved}" >&2
  exit 1
}
if ! rustup target list --installed --toolchain "${toolchain}" | rg -qx "${target}"; then
  printf 'canonical public API target %s is unavailable for Rust %s; install is intentionally not attempted\n' "${target}" "${toolchain}" >&2
  exit 1
fi

cleanup
mkdir -p "${temporary_root}/source" "${temporary_root}/generated" "${temporary_root}/canonicalizer"
git -C "${workspace}" archive "${baseline_commit}" | tar -x -C "${temporary_root}/source"
cp "${baseline_canonicalizer}" "${temporary_root}/canonicalizer/public-api-canonicalizer-v4.mjs"

packages=(oxiroute-config oxiroute-config-source oxiroute-import oxiroute oxiroute-rtmp)
crates=(oxiroute-config oxiroute-config-source oxiroute-import oxiroute-server oxiroute-rtmp)
json_names=(oxiroute_config oxiroute_config_source oxiroute_import oxiroute_server oxiroute_rtmp)
snapshot_names=(config config-source import server rtmp)
export OXIROUTE_API_TOOLCHAIN=${toolchain}
export OXIROUTE_API_TARGET=${target}

node "${temporary_root}/canonicalizer/public-api-canonicalizer-v4.mjs" --self-test
for index in "${!packages[@]}"; do
  package=${packages[index]}
  RUSTC_BOOTSTRAP=1 nice cargo +"${toolchain}" rustdoc \
    --manifest-path "${temporary_root}/source/Cargo.toml" \
    -p "${package}" \
    --lib \
    --all-features \
    --locked \
    --target "${target}" \
    -j 4 \
    -- \
    -Z unstable-options \
    --output-format json
done

for index in "${!packages[@]}"; do
  crate=${crates[index]}
  json="${temporary_root}/source/target/${target}/doc/${json_names[index]}.json"
  generated="${temporary_root}/generated/${snapshot_names[index]}.snapshot"
  checked="${workspace}/docs/developer/fixtures/${snapshot_names[index]}-public-api-2d9c5fe.snapshot"

  node "${temporary_root}/canonicalizer/public-api-canonicalizer-v4.mjs" \
    --write "${json}" "${generated}" "${crate}" unused "${baseline_commit}" \
    "${temporary_root}/source/target/${target}/doc"
  if [[ "${mode}" == --update ]]; then
    cp "${generated}" "${checked}"
  else
    cmp "${generated}" "${checked}"
  fi
done
