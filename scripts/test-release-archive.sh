#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
test_root=$(mktemp -d)
trap 'rm -rf -- "${test_root}"' EXIT
version=policy-test
root="oxiroute-${version}"

expect_rejected() {
  local label=$1
  local archive=$2
  if "${repo_dir}/scripts/verify-release-archive.sh" "${archive}" "${version}" >/dev/null 2>&1; then
    printf 'release archive test accepted %s\n' "${label}" >&2
    exit 1
  fi
}

make_archive() {
  local archive=$1
  local path=$2
  local content=$3
  rm -rf -- "${test_root}/payload"
  mkdir -p -- "${test_root}/payload/$(dirname -- "${path}")"
  printf '%s\n' "${content}" >"${test_root}/payload/${path}"
  tar -C "${test_root}/payload" -czf "${archive}" -- "${path}"
}

make_archive "${test_root}/wrong-root.tar.gz" 'wrong-root/source.txt' safe
expect_rejected 'an entry outside the release root' "${test_root}/wrong-root.tar.gz"

make_archive "${test_root}/artifact.tar.gz" "${root}/target/binary" safe
expect_rejected 'a build artifact path' "${test_root}/artifact.tar.gz"

make_archive "${test_root}/secret-path.tar.gz" "${root}/production.env" safe
expect_rejected 'a secret-shaped path' "${test_root}/secret-path.tar.gz"

private_key_marker='-----BEGIN PRIVATE ''KEY-----'
make_archive "${test_root}/secret-content.tar.gz" "${root}/source.txt" "${private_key_marker}"
expect_rejected 'unallowlisted private-key content' "${test_root}/secret-content.tar.gz"

valid_archive="${test_root}/valid.tar.gz"
SOURCE_DATE_EPOCH=1 "${repo_dir}/scripts/create-release-archive.sh" \
  "${valid_archive}" "${version}" >/dev/null
gzip -dc -- "${valid_archive}" >"${test_root}/missing-required.tar"
tar --delete --file="${test_root}/missing-required.tar" "${root}/Cargo.lock"
gzip -n <"${test_root}/missing-required.tar" >"${test_root}/missing-required.tar.gz"
expect_rejected 'an omitted required file' "${test_root}/missing-required.tar.gz"

printf 'release archive policy tests passed\n'
