#!/usr/bin/env bash
set -euo pipefail

workspace=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
mode=${1:-classified}

if [[ "${mode}" == classified ]]; then
  exec "${workspace}/scripts/verify-public-api.sh"
fi
if [[ "${mode}" != --baseline-equality ]]; then
  printf 'usage: %s [--baseline-equality]\n' "${0}" >&2
  exit 2
fi

snapshot="${workspace}/docs/developer/fixtures/rtmp-public-api-phase0.snapshot"

cd "${workspace}"
rustc_version=$(rustc +1.97.1 --version)
[[ "${rustc_version}" == rustc\ 1.97.1\ * ]] || {
  printf 'expected Rust 1.97.1, got %s\n' "${rustc_version}" >&2
  exit 1
}
host=
while IFS=': ' read -r key value; do
  if [[ "${key}" == host ]]; then
    host=${value}
    break
  fi
done < <(rustc +1.97.1 -vV)
[[ -n "${host}" ]] || {
  printf 'could not determine the Rust host target\n' >&2
  exit 1
}
json="${workspace}/target/${host}/doc/oxiroute_rtmp.json"

RUSTC_BOOTSTRAP=1 nice cargo +1.97.1 rustdoc \
  -p oxiroute-rtmp \
  --lib \
  --all-features \
  --locked \
  --target "${host}" \
  -j 4 \
  -- \
  -Z unstable-options \
  --output-format json

node scripts/rtmp-public-api-inventory.mjs --self-test
node scripts/rtmp-public-api-inventory.mjs --check "${json}" "${snapshot}"
