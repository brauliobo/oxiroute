#!/usr/bin/bash
# Proves pin-bin-release.sh rewrites the pinned version and checksum from the checksum file of the Arch package asset of a
# release directory and keeps .SRCINFO consistent, without network access (file:// release locations), and refuses a
# checksum file that does not name the package or does not hold a sha256.
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
root=$(mktemp -d)
trap 'rm -rf -- "${root}"' EXIT

mkdir -p -- "${root}/pkg/bin" "${root}/release"
cp -- "${here}"/verify-checksums.sh "${here}"/pin-bin-release.sh "${here}"/PKGBUILD "${here}"/.SRCINFO "${here}"/README.md \
  "${here}"/oxiroute.* "${root}/pkg/"
cp -- "${here}"/bin/PKGBUILD "${here}"/bin/.SRCINFO "${here}"/bin/oxiroute.install "${root}/pkg/bin/"

pin() { OXIROUTE_RELEASE_URL=file://${root}/release "${root}/pkg/pin-bin-release.sh" "$@"; }

name=oxiroute-9.9.9-1-x86_64.pkg.tar.zst
printf 'arch package\n' >"${root}/release/${name}"
(cd "${root}/release" && sha256sum "${name}" >"${name}.sha256")

pin 9.9.9 >/dev/null

package_sha256=$(sha256sum "${root}/release/${name}")
grep -q '^pkgver=9.9.9$' "${root}/pkg/bin/PKGBUILD"
grep -q '^pkgrel=1$' "${root}/pkg/bin/PKGBUILD"
grep -q "^sha256sums=('${package_sha256%% *}')$" "${root}/pkg/bin/PKGBUILD"
grep -q "sha256sums = ${package_sha256%% *}" "${root}/pkg/bin/.SRCINFO"
grep -q 'pkgver = 9.9.9' "${root}/pkg/bin/.SRCINFO"
grep -q "source = ${name}::.*/v9.9.9/${name}$" "${root}/pkg/bin/.SRCINFO"
! grep -q '7\.1\.0' "${root}/pkg/bin/.SRCINFO" "${root}/pkg/bin/PKGBUILD"

printf 'tampered\n' >"${root}/release/${name}.sha256"
if pin 9.9.9 >/dev/null 2>&1; then
  printf 'pin-bin-release.sh accepted a malformed checksum file\n' >&2
  exit 1
fi

printf '%s  %s\n' "$(printf 'x%.0s' {1..64})" "${name}" >"${root}/release/${name}.sha256"
if pin 9.9.9 >/dev/null 2>&1; then
  printf 'pin-bin-release.sh accepted a checksum that is not a sha256\n' >&2
  exit 1
fi

if pin 9.9.8 >/dev/null 2>&1; then
  printf 'pin-bin-release.sh pinned a release without a package asset\n' >&2
  exit 1
fi
printf 'pin-bin-release tests passed\n'
