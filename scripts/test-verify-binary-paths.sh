#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
verify=${repo_dir}/scripts/verify-binary-paths.sh
root=$(mktemp -d)
trap 'rm -rf -- "${root}"' EXIT

binary_with() {
  printf '\177ELF\0\0%s\0\0tail' "$1" >"${root}/bin"
}

expect_accepted() {
  binary_with "$1"
  "${verify}" "${root}/bin" "${@:2}" >/dev/null
}

expect_rejected() {
  binary_with "$1"
  if "${verify}" "${root}/bin" "${@:2}" >/dev/null 2>&1; then
    printf 'verify-binary-paths.sh accepted a binary embedding %s\n' "$1" >&2
    exit 1
  fi
}

expect_accepted '/usr/src/debug/oxiroute/cargo/registry/src/crate/src/lib.rs'
expect_accepted '/rustc/0123456789abcdef/library/std/src/lib.rs'
expect_rejected '/home/runner/.cargo/registry/src/index.crates.io-0/crate/src/lib.rs'
expect_rejected '/home/runner/work/_temp/pgo/target-use/release/build/out'
expect_rejected '/home/alice/src/oxiroute/src/main.rs'
expect_rejected '/srv/ci/runner/work/oxiroute/src/main.rs'
expect_rejected '/srv/checkout/src/main.rs' /srv/checkout
expect_accepted '/srv/checkout/src/main.rs' /srv/other

printf 'verify-binary-paths tests passed\n'
