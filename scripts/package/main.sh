#!/usr/bin/env bash
# The release packages of oxiroute: .deb and .rpm of every family and architecture, assembled from the one binary of the
# architecture and tested in containers.
#
#   main.sh binary ARCH VERSION OUT_DIR       build the binary tarball of an architecture on the oldest supported glibc
#   main.sh layout ARCH VERSION OUT_DIR       the BOLT layout and the tarball of a binary built with STAGE=pgo (host)
#   main.sh build FAMILY ARCH VERSION OUT_DIR assemble a package of the tarball in OUT_DIR in a container of its family
#   main.sh test FAMILY ARCH PACKAGE [PREVIOUS]
#                                             install it in fresh containers of every image of the family and check it
#   main.sh all FAMILY ARCH VERSION OUT_DIR   build, then test
#   main.sh name FAMILY ARCH VERSION          the file name of the package
#   main.sh stem VERSION ARCH FLAVOUR         the name of the binary tarball, without .tar.gz
#   main.sh matrix assemble|install|extras|tests|flavours|any
#                                             the job matrices of the release workflow (JSON) and the x86_64 flavours
#   main.sh assets list|verify|sums|notes ... the release asset set, its SHA256SUMS and its notes
#   main.sh changed BASE                      whether a file the packages depend on changed since BASE
#   main.sh previous FAMILY ARCH TAG OUT_DIR  the package of the previous release, for the upgrade test
#   main.sh selftest                          the offline checks of these scripts
#
# FAMILY is deb, el9 or amzn2023; ARCH is x86_64 or aarch64; VERSION is the release version without the leading v.
# The VERSION is an input: nothing here reads or changes the version of the workspace. Each subcommand documents its
# environment in its own file.
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source "${here}/families.sh"

usage() { sed -n '2,23p' "${BASH_SOURCE[0]}" >&2; exit 2; }

all() {
  local family=$1 arch=$2 version=$3 out=$4
  "${here}/build.sh" "${family}" "${arch}" "${version}" "${out}"
  "${here}/test.sh" "${family}" "${arch}" "${out}/$(package_file "${family}" "${arch}" "${version}")"
}

command=${1:-}
shift || true
case ${command} in
  binary | layout | build | test | matrix | assets | changed | previous | selftest) exec "${here}/${command}.sh" "$@" ;;
  all) [[ $# == 4 ]] || usage; all "$@" ;;
  name) [[ $# == 3 ]] || usage; package_file "$@" ;;
  stem) [[ $# == 3 ]] || usage; tarball_stem "$1" "$(package_target "$2")" "$3" ;;
  *) usage ;;
esac
