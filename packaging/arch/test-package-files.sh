#!/usr/bin/bash
# Runs package() of the source and binary recipes against a stub build tree and checks the installed file list, the
# modes and that the support files are installed verbatim. No build, download or installation takes place.
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
root=$(mktemp -d)
trap 'rm -rf -- "${root}"' EXIT

expected_files='etc/oxiroute/oxiroute.env
etc/oxiroute/oxiroute.kdl
etc/oxiroute/oxiroute.lua
usr/bin/oxiroute
usr/bin/oxr
usr/lib/modules-load.d/oxiroute.conf
usr/lib/oxiroute/oxiroute-worker-launcher
usr/lib/systemd/system/oxiroute.service
usr/lib/sysusers.d/oxiroute.conf
usr/lib/tmpfiles.d/oxiroute.conf
usr/share/doc/oxiroute/arch-packaging.md
usr/share/doc/oxiroute/management-cli.md
usr/share/licenses/oxiroute/LICENSE'

# Stub build output for the layout each recipe's package() expects, relative to its "oxiroute-${pkgver}*" directory.
stub_tree() {
  local dir=$1 binary=$2 launcher=$3
  mkdir -p -- "${dir}/$(dirname -- "${binary}")" "${dir}/$(dirname -- "${launcher}")" "${dir}/docs"
  printf 'binary\n' >"${dir}/${binary}"
  printf 'launcher\n' >"${dir}/${launcher}"
  printf 'license\n' >"${dir}/LICENSE"
  printf 'management\n' >"${dir}/docs/MANAGEMENT_CLI.md"
}

check_package() {
  local label=$1 recipe=$2 tree=$3 binary=$4 launcher=$5 work=${root}/$1
  local srcdir=${work}/src pkgdir=${work}/pkg actual
  mkdir -p -- "${srcdir}" "${pkgdir}"
  cp -- "${recipe}"/oxiroute.* "${recipe}/README.md" "${srcdir}/"
  (
    source "${recipe}/PKGBUILD"
    tree=${tree//@VERSION@/${pkgver}}
    stub_tree "${srcdir}/${tree}" "${binary}" "${launcher}"
    printf 'management\n' >"${srcdir}/management-cli-${pkgver}.md"
    objcopy() { :; }
    cd -- "${srcdir}"
    package
  )
  actual=$(cd -- "${pkgdir}" && find . -mindepth 1 ! -type d -printf '%P\n' | LC_ALL=C sort)
  [[ ${actual} == "${expected_files}" ]] || {
    printf '%s installs an unexpected file list:\n' "${label}" >&2
    diff -u <(printf '%s\n' "${expected_files}") <(printf '%s\n' "${actual}") >&2 || true
    exit 1
  }
  cmp -- "${recipe}/oxiroute.modules" "${pkgdir}/usr/lib/modules-load.d/oxiroute.conf"
  [[ $(stat -c %a "${pkgdir}/usr/lib/modules-load.d/oxiroute.conf") == 644 ]] || {
    printf '%s installs the kernel module list with a mode other than 0644\n' "${label}" >&2
    exit 1
  }
}

check_package source "${here}" 'oxiroute-@VERSION@' target/x86_64-unknown-linux-gnu/release/oxiroute \
  target/x86_64-unknown-linux-gnu/release/oxiroute-worker-launcher
check_package binary "${here}/bin" 'oxiroute-@VERSION@-x86_64-unknown-linux-gnu' bin/oxiroute \
  lib/oxiroute/oxiroute-worker-launcher
printf 'package file list tests passed\n'
