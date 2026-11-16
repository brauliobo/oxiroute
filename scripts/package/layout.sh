#!/usr/bin/env bash
# The second stage of a binary that binary.sh built with STAGE=pgo (OUT_DIR/stage): the BOLT instruction layout, the
# checks of the rewritten binary and the tarball, on the host, because the pinned BOLT is an Ubuntu 24.04 package that does
# not run on the libc of the build container. BOLT rewrites the code without changing the glibc the binary needs, and the
# floor of families.sh is checked again on the result. Needs Rust (the toolchain of families.sh), BOLT on PATH or in
# BOLT_BIN_DIR (scripts/install-bolt.sh) and, for the checks of the shipped binary, the UI dependencies.
#
#   layout.sh ARCH VERSION OUT_DIR
#
# Environment: FLAVOUR (default unwind), RELEASE_TESTS (default 0: the workspace tests of the shipped binary run in their own
# job of the release workflow), PGO_WORK (default /var/tmp/oxiroute-pgo) and the variables of
# scripts/build-release-binaries.sh. BOLT is x86_64 only.
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source "${here}/families.sh"
source "${here}/runtime.sh"

arch=${1:-} version=${2:-} out=${3:-}
[[ $# == 3 ]] || { echo "usage: $0 ARCH VERSION OUT_DIR" >&2; exit 2; }
require_member architecture "${arch}" "${package_arches[@]}"
require_version "${version}"

STAGE=layout BOLT=1 GLIBC_FLOOR=${glibc_floor} PGO_TARGET=$(package_target "${arch}") FLAVOUR=${FLAVOUR:-unwind} \
  RELEASE_TESTS=${RELEASE_TESTS:-0} CARGO_INCREMENTAL=0 "${package_repo}/scripts/build-release-binaries.sh" "${version}" "${out}"
