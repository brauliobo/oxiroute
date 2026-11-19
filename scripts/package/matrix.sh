#!/usr/bin/env bash
# Prints a job matrix of the release workflow as GitHub JSON, and whether it has any job.
#
#   matrix.sh assemble ARCH   one job per package family released for ARCH: its package is assembled from the binary of ARCH
#   matrix.sh install ARCH    one job per image of every such family: the package is installed and checked in a fresh container
#   matrix.sh extras          the x86_64 flavours that are built next to the default one (abort, v3)
#   matrix.sh tests           the x86_64 binaries the workspace tests run against (the BOLT layout and the abort flavour)
#   matrix.sh flavours        the x86_64 flavours of the release, for assets.sh (X86_64_FLAVOURS)
#   matrix.sh any MATRIX [ARCH]
#                             true when `matrix.sh MATRIX [ARCH]` has at least one job, else false
#
# Environment: ABORT and V3 (1 or 0: the fail-fast and the x86-64-v3 flavours; defaults 1 and 0), BOLT (1 or 0, default 1:
# the layout stage of the x86_64 binary). The aarch64 binary is always built without BOLT: scripts/install-bolt.sh pins
# the amd64 packages of one distribution and no BOLT replay is verified on aarch64.
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source "${here}/families.sh"

json() {
  local IFS=,
  printf '{"include":[%s]}\n' "$*"
}

families_of() {
  local family arch
  while read -r family arch; do [[ ${arch} != "$1" ]] || echo "${family}"; done < <(package_pairs)
}

assemble() {
  local family
  for family in $(families_of "$1"); do printf '{"family":"%s"}\n' "${family}"; done
}

install() {
  local family image
  for family in $(families_of "$1"); do
    for image in $(test_images "${family}"); do printf '{"family":"%s","image":"%s"}\n' "${family}" "${image}"; done
  done
}

flavours() {
  echo "unwind$([[ ${ABORT:-1} == 1 ]] && echo ' abort')$([[ ${V3:-0} == 1 ]] && echo ' v3')"
}

extras() {
  local flavour
  for flavour in $(flavours); do [[ ${flavour} == unwind ]] || printf '{"flavour":"%s"}\n' "${flavour}"; done
}

tests() {
  [[ ${BOLT:-1} == 0 ]] || echo '{"flavour":"unwind"}'
  [[ ${ABORT:-1} == 0 ]] || echo '{"flavour":"abort"}'
}

usage() {
  printf 'usage: %s assemble ARCH | install ARCH | extras | tests | flavours | any MATRIX [ARCH]\n' "${BASH_SOURCE[0]}" >&2
  exit 2
}

matrix_jobs() {
  case $1 in
    assemble | install) require_member architecture "${2:-}" "${package_arches[@]}" && "$1" "$2" ;;
    extras | tests) "$1" ;;
    *) usage ;;
  esac
}

case ${1:-} in
  flavours) flavours ;;
  any) jobs=$(matrix_jobs "${2:-}" "${3:-}"); [[ -n ${jobs} ]] && echo true || echo false ;;
  *) jobs=$(matrix_jobs "${1:-}" "${2:-}"); mapfile -t lines <<< "${jobs}"; json "${lines[@]}" ;;
esac
