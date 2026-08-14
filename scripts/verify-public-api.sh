#!/usr/bin/env bash
set -euo pipefail

workspace=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "${workspace}"

mode=${1:-verify}
toolchain=1.97.1
target=x86_64-unknown-linux-gnu
baseline_commit=2d9c5fe66cd096d7a1d8e3bada8d5784b5f97f6c
if [[ "${mode}" != verify && "${mode}" != --update ]]; then
  printf 'usage: %s [--update]\n' "${0}" >&2
  exit 2
fi
rustc_version=$(rustc +"${toolchain}" --version)
[[ "${rustc_version}" == rustc\ "${toolchain}"\ * ]] || {
  printf 'expected Rust %s, got %s\n' "${toolchain}" "${rustc_version}" >&2
  exit 1
}
if ! rustup target list --installed --toolchain "${toolchain}" | rg -qx "${target}"; then
  printf 'canonical public API target %s is unavailable for Rust %s; install is intentionally not attempted\n' "${target}" "${toolchain}" >&2
  exit 1
fi

export OXIROUTE_API_TOOLCHAIN=${toolchain}
export OXIROUTE_API_TARGET=${target}

node scripts/rtmp-public-api-inventory.mjs --self-test

packages=(oxiroute-config oxiroute-config-source oxiroute-import oxiroute oxiroute-rtmp)
crates=(oxiroute-config oxiroute-config-source oxiroute-import oxiroute-server oxiroute-rtmp)
json_names=(oxiroute_config oxiroute_config_source oxiroute_import oxiroute_server oxiroute_rtmp)
snapshot_names=(config config-source import server rtmp)

for index in "${!packages[@]}"; do
  package=${packages[index]}
  RUSTC_BOOTSTRAP=1 nice cargo +"${toolchain}" rustdoc \
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
  json="${workspace}/target/${target}/doc/${json_names[index]}.json"
  baseline="${workspace}/docs/developer/fixtures/${snapshot_names[index]}-public-api-2d9c5fe.snapshot"
  delta="${workspace}/docs/developer/fixtures/${snapshot_names[index]}-public-api-0.5.delta"

  for metadata in \
    "toolchain=${toolchain}" \
    "target=${target}" \
    "features=all" \
    "schema=4" \
    "commit=${baseline_commit}"; do
    if ! IFS= read -r present < <(grep -F -m1 "${metadata}" "${baseline}") || [[ "${present}" != "${metadata}" ]]; then
      printf '%s baseline metadata mismatch: expected %s\n' "${crate}" "${metadata}" >&2
      exit 1
    fi
  done

  if [[ "${mode}" == --update ]]; then
    node scripts/rtmp-public-api-inventory.mjs \
      --diff-write "${baseline}" "${json}" "${crate}" "${delta}" "worktree" \
      "${workspace}/target/${target}/doc"
  else
    node scripts/rtmp-public-api-inventory.mjs \
      --diff-check "${baseline}" "${json}" "${crate}" "${delta}" "worktree" \
      "${workspace}/target/${target}/doc"
  fi
done
