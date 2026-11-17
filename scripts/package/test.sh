#!/usr/bin/env bash
# Installs a package in a fresh container of every image of its family (same architecture) and checks that it
# installs what the layout promises, that the program works (version, check of the shipped configuration, a request
# relayed through a proxy to a local origin), that the systemd unit verifies, that a reinstall or an upgrade keeps the
# configuration, and that remove and purge leave what the policy says.
#
#   test.sh FAMILY ARCH PACKAGE [PREVIOUS_PACKAGE]
#
# PREVIOUS_PACKAGE is an older release package of the same family: it is installed first and upgraded to PACKAGE.
# Environment: PGO (default 1: the build record of the package must say profile-guided; 0 for a plain local build),
# PACKAGE_IMAGES (space-separated images instead of the ones of the family), PACKAGE_RUNTIME, PACKAGE_CPUSET and
# PACKAGE_NICE (runtime.sh). An image of another architecture than the host runs only under emulation: the result
# then tests the layout and the installation, not the native code.
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source "${here}/families.sh"
source "${here}/runtime.sh"

family=${1:-} arch=${2:-} package=${3:-} previous=${4:-}
[[ $# == 3 || $# == 4 ]] || { echo "usage: $0 FAMILY ARCH PACKAGE [PREVIOUS_PACKAGE]" >&2; exit 2; }
require_member family "${family}" "${package_families[@]}"
require_member architecture "${arch}" "${package_arches[@]}"
[[ -f ${package} ]] || { printf 'no such package: %s\n' "${package}" >&2; exit 1; }
version=$(file_version "${family}" "${package}")
[[ $(basename -- "${package}") == "$(package_file "${family}" "${arch}" "${version}")" ]] ||
  { printf '%s is not the %s package of %s for %s\n' "${package}" "${family}" "${version}" "${arch}" >&2; exit 1; }
[[ ${arch} == "$(host_arch)" ]] || echo "EMULATED: ${arch} containers on a $(host_arch) host test the layout, not native code" >&2

scratch=$(mktemp -d)
trap 'rm -rf -- "${scratch}"' EXIT
mkdir "${scratch}/out"
cp -- "${package}" "${package}.build.json" "${scratch}/out/"
[[ -z ${previous} ]] || cp -- "${previous}" "${scratch}/out/"

test_image() {
  local image=$1
  container_run "${arch}" "${image}" -e FAMILY="${family}" -e ARCH="${arch}" -e RELEASE_VERSION="${version}" \
    -e PACKAGE="$(basename -- "${package}")" ${previous:+-e PREVIOUS="$(basename -- "${previous}")"} \
    -e REQUIRE_PGO="${PGO:-1}" \
    -v "${scratch}/out:/out:ro" -v "${package_scripts}:/package:ro" -v "${package_repo}/packaging:/packaging:ro" \
    -- /package/container/test.sh
}

read -ra images <<< "${PACKAGE_IMAGES:-$(test_images "${family}")}"
pids=()
for index in "${!images[@]}"; do
  test_image "${images[index]}" > "${scratch}/log.${index}" 2>&1 &
  pids+=($!)
done
status=0
for index in "${!images[@]}"; do
  printf '== %s\n' "${images[index]}"
  wait "${pids[index]}" || status=1
  cat "${scratch}/log.${index}"
done
exit "${status}"
