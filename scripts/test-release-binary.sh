#!/usr/bin/env bash
# Runs the workspace test suite with the release artefact (a BOLT-rewritten or panic=abort binary) in place of the one
# cargo built: every integration test that starts `oxiroute` (CARGO_BIN_EXE_oxiroute: process, supervised, CLI and
# panic tests) then drives the final artefact. BOLT bugs live in the rewritten binary and are invisible to a normal
# `cargo test`.
#
#   test-release-binary.sh BIN [cargo test arguments]
#
# The tests are built without LTO (a test build does not need the shipped code generation, only the binary under test
# does), then BIN replaces target/release/oxiroute, and the script fails if cargo replaced it again before the tests ran.
# Environment: BOLT_TEST_TARGET_DIR (default: $CARGO_TARGET_DIR or the build scratch directory), PGO_JOBS (default 4).
set -euo pipefail

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
binary=${1:-}
[[ -f ${binary} && ${binary} != -* ]] || { echo "usage: $0 BIN [cargo test arguments]" >&2; exit 2; }
shift
export CARGO_TARGET_DIR=${BOLT_TEST_TARGET_DIR:-${CARGO_TARGET_DIR:-${PGO_WORK:-${HOME}/.cache/oxiroute-pgo}/target-test}}
export CARGO_PROFILE_RELEASE_LTO=off CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 CARGO_PROFILE_RELEASE_DEBUG=0
export CARGO_INCREMENTAL=0
jobs=${PGO_JOBS:-4}
expected=$(sha256sum "${binary}" | cut -d' ' -f1)
installed=${CARGO_TARGET_DIR}/release/oxiroute

cd "${repo}"
nice cargo test --release --workspace --locked --jobs "${jobs}" --no-run
install -Dm755 "${binary}" "${installed}"
[[ $(sha256sum "${installed}" | cut -d' ' -f1) == "${expected}" ]]
status=0
nice cargo test --release --workspace --locked --jobs "${jobs}" --no-fail-fast "$@" || status=$?
[[ $(sha256sum "${installed}" | cut -d' ' -f1) == "${expected}" ]] || {
  echo "cargo replaced the binary under test; the results above are not for ${binary}" >&2
  exit 1
}
exit "${status}"
