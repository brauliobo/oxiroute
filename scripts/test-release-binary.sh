#!/usr/bin/env bash
# Runs the workspace test suite with the release artefact (a BOLT-rewritten or panic=abort binary) in place of the one
# cargo built: every integration test that starts `oxiroute` (CARGO_BIN_EXE_oxiroute: process, supervised, CLI and
# panic tests) then drives the final artefact. BOLT bugs live in the rewritten binary and are invisible to a normal
# `cargo test`.
#
#   test-release-binary.sh BIN [cargo test arguments]
#
# The tests are built without LTO (a test build does not need the shipped code generation, only the binary under test
# does). Cargo hard-links target/release/oxiroute to target/release/deps/oxiroute-HASH on every run, so BIN is written
# into that file in place; the script fails if the binary the tests ran against is not BIN.
# Environment: RELEASE_TEST_SCOPE (cargo selection, default --workspace; e.g. "-p oxiroute --test supervised"), BOLT_TEST_TARGET_DIR (default: $CARGO_TARGET_DIR or the build scratch directory), PGO_JOBS (default 4).
set -euo pipefail

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
binary=${1:-}
[[ -f ${binary} && ${binary} != -* ]] || { echo "usage: $0 BIN [cargo test arguments]" >&2; exit 2; }
shift
export CARGO_TARGET_DIR=${BOLT_TEST_TARGET_DIR:-${CARGO_TARGET_DIR:-${PGO_WORK:-/var/tmp/oxiroute-pgo}/target-test}}
export CARGO_PROFILE_RELEASE_LTO=off CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 CARGO_PROFILE_RELEASE_DEBUG=0
export CARGO_INCREMENTAL=0
jobs=${PGO_JOBS:-4}
read -ra scope <<< "${RELEASE_TEST_SCOPE:---workspace}"
expected=$(sha256sum "${binary}" | cut -d' ' -f1)
installed=${CARGO_TARGET_DIR}/release/oxiroute

cd "${repo}"
nice cargo test --release "${scope[@]}" --locked --jobs "${jobs}" --no-run
deps_file=$(find "${CARGO_TARGET_DIR}/release/deps" -samefile "${installed}")
[[ $(wc -l <<< "${deps_file}") == 1 ]] || { echo "cannot find the cargo output behind ${installed}" >&2; exit 1; }
cat -- "${binary}" > "${deps_file}"
[[ $(sha256sum "${installed}" | cut -d' ' -f1) == "${expected}" ]]
status=0
nice cargo test --release "${scope[@]}" --locked --jobs "${jobs}" --no-fail-fast "$@" || status=$?
[[ $(sha256sum "${installed}" | cut -d' ' -f1) == "${expected}" ]] || {
  echo "cargo replaced the binary under test; the results above are not for ${binary}" >&2
  exit 1
}
exit "${status}"
