#!/usr/bin/env bash
# Builds the release binaries of one architecture inside a container of the oldest supported glibc (the binary_image of
# families.sh), so the one binary runs on Debian 12 and 13, Ubuntu 22.04 and 24.04, the RHEL family 9 and Amazon Linux
# 2023: every tarball and every package is that binary, repackaged. Needs a native container runtime (an aarch64 binary
# is built on an aarch64 host).
#
#   binary.sh ARCH VERSION OUT_DIR
#
# The work is scripts/build-release-binaries.sh; its environment passes through: PGO (default 1; 0 is the plain build of
# local smoke tests and pull request dry runs), BOLT (default 0 here: BOLT needs the host, see below), FLAVOUR, STAGE
# (all or pgo), RELEASE_TESTS. STAGE=pgo leaves OUT_DIR/stage, which the host then lays out with BOLT
# (STAGE=layout scripts/build-release-binaries.sh VERSION OUT_DIR): the pinned BOLT is an Ubuntu 24.04 package that
# does not run on the libc of the container, but it rewrites the binary without changing the glibc it needs, and the
# floor is checked again on the result.
# Also: PACKAGE_JOBS (cargo jobs, default 4), PACKAGE_CACHE (host directory that keeps the Rust toolchain and the cargo
# registry between runs), PACKAGE_RUNTIME, PACKAGE_CPUSET and PACKAGE_NICE (runtime.sh).
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source "${here}/families.sh"
source "${here}/runtime.sh"

arch=${1:-} version=${2:-} out=${3:-}
[[ $# == 3 ]] || { echo "usage: $0 ARCH VERSION OUT_DIR" >&2; exit 2; }
require_member architecture "${arch}" "${package_arches[@]}"
require_version "${version}"
[[ ${arch} == "$(host_arch)" ]] || { echo "an ${arch} binary is built natively; this host is $(host_arch)" >&2; exit 1; }
mkdir -p -- "${out}"
out=$(cd -- "${out}" && pwd)

scratch=$(mktemp -d)
trap 'rm -rf -- "${scratch}"' EXIT
source_snapshot "${scratch}/src.tar"
cache=${PACKAGE_CACHE:+$(mkdir -p -- "${PACKAGE_CACHE}" && cd -- "${PACKAGE_CACHE}" && pwd)}

container_run "${arch}" "${binary_image}" \
  -e ARCH="${arch}" -e RELEASE_VERSION="${version}" -e HOST_UID="$(id -u)" -e PACKAGE_JOBS="${PACKAGE_JOBS:-4}" \
  -e PGO="${PGO:-1}" -e BOLT="${BOLT:-0}" -e FLAVOUR="${FLAVOUR:-unwind}" -e STAGE="${STAGE:-all}" \
  -e RELEASE_TESTS="${RELEASE_TESTS:-1}" \
  -e SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git -C "${package_repo}" log -1 --format=%ct HEAD)}" \
  -e RELEASE_COMMIT="${RELEASE_COMMIT:-$(git -C "${package_repo}" rev-parse HEAD)}" \
  -v "${scratch}/src.tar:/src.tar:ro" -v "${package_scripts}:/package:ro" -v "${out}:/out" \
  ${cache:+-v "${cache}:/cache"} -- /package/container/binary.sh
