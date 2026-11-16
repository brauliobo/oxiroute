#!/usr/bin/env bash
# Assembles the package of one family and architecture from the binary tarball of the release, inside a container of
# the family (dpkg-buildpackage or rpmbuild), and writes OUT_DIR/PACKAGE and OUT_DIR/PACKAGE.build.json (the build record
# of the binary inside it). Nothing is compiled: OUT_DIR must already hold oxiroute-VERSION-TARGET.tar.gz, the tarball
# of binary.sh or of the release workflow, and the package is that binary repackaged, in seconds.
#
#   build.sh FAMILY ARCH VERSION OUT_DIR
#
# Environment: PACKAGE_RUNTIME, PACKAGE_CPUSET and PACKAGE_NICE (runtime.sh). An aarch64 package is assembled on an
# aarch64 host.
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source "${here}/families.sh"
source "${here}/runtime.sh"

family=${1:-} arch=${2:-} version=${3:-} out=${4:-}
[[ $# == 4 ]] || { echo "usage: $0 FAMILY ARCH VERSION OUT_DIR" >&2; exit 2; }
require_member family "${family}" "${package_families[@]}"
require_member architecture "${arch}" "${package_arches[@]}"
require_version "${version}"
[[ ${arch} == "$(host_arch)" ]] || { echo "an ${arch} package is assembled natively; this host is $(host_arch)" >&2; exit 1; }
tarball=${out}/$(tarball_stem "${version}" "$(package_target "${arch}")" unwind).tar.gz
[[ -f ${tarball} ]] || { printf 'no binary tarball: %s (scripts/package/binary.sh builds it)\n' "${tarball}" >&2; exit 1; }
out=$(cd -- "${out}" && pwd)

scratch=$(mktemp -d)
trap 'rm -rf -- "${scratch}"' EXIT
source_snapshot "${scratch}/src.tar"

container_run "${arch}" "$(build_image "${family}")" \
  -e FAMILY="${family}" -e ARCH="${arch}" -e RELEASE_VERSION="${version}" -e HOST_UID="$(id -u)" \
  -e SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git -C "${package_repo}" log -1 --format=%ct HEAD)}" \
  -v "${scratch}/src.tar:/src.tar:ro" -v "${package_scripts}:/package:ro" -v "${out}:/out" \
  -- /package/container/package.sh
