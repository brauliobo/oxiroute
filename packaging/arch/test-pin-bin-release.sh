#!/usr/bin/bash
# Proves pin-bin-release.sh rewrites the pinned version and checksums from a release directory and keeps .SRCINFO
# consistent, without network access (file:// release locations).
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
root=$(mktemp -d)
trap 'rm -rf -- "${root}"' EXIT

mkdir -p -- "${root}/pkg/bin" "${root}/release" "${root}/docs"
cp -- "${here}"/verify-checksums.sh "${here}"/pin-bin-release.sh "${here}"/PKGBUILD "${here}"/.SRCINFO "${here}"/README.md \
  "${here}"/oxiroute.* "${root}/pkg/"
cp -- "${here}"/bin/PKGBUILD "${here}"/bin/.SRCINFO "${here}"/bin/oxiroute.* "${here}"/bin/README.md "${root}/pkg/bin/"

name=oxiroute-9.9.9-x86_64-unknown-linux-gnu.tar.gz
printf 'binary tarball\n' >"${root}/release/${name}"
(cd "${root}/release" && sha256sum "${name}" >"${name}.sha256")
printf 'management documentation\n' >"${root}/docs/MANAGEMENT_CLI.md"

OXIROUTE_RELEASE_URL=file://${root}/release OXIROUTE_RAW_URL=file://${root}/docs \
  "${root}/pkg/pin-bin-release.sh" 9.9.9 >/dev/null

tarball_sha256=$(sha256sum "${root}/release/${name}")
doc_sha256=$(sha256sum "${root}/docs/MANAGEMENT_CLI.md")
grep -q '^pkgver=9.9.9$' "${root}/pkg/bin/PKGBUILD"
grep -q "'${tarball_sha256%% *}'" "${root}/pkg/bin/PKGBUILD"
grep -q "'${doc_sha256%% *}'" "${root}/pkg/bin/PKGBUILD"
grep -q "sha256sums = ${tarball_sha256%% *}" "${root}/pkg/bin/.SRCINFO"
grep -q 'pkgver = 9.9.9' "${root}/pkg/bin/.SRCINFO"
modules_sha256=$(sha256sum "${root}/pkg/bin/oxiroute.modules")
grep -q "'${modules_sha256%% *}'" "${root}/pkg/bin/PKGBUILD"
grep -q "sha256sums = ${modules_sha256%% *}" "${root}/pkg/bin/.SRCINFO"

printf 'tampered\n' >"${root}/release/${name}.sha256"
if OXIROUTE_RELEASE_URL=file://${root}/release OXIROUTE_RAW_URL=file://${root}/docs \
  "${root}/pkg/pin-bin-release.sh" 9.9.9 >/dev/null 2>&1; then
  printf 'pin-bin-release.sh accepted a malformed checksum file\n' >&2
  exit 1
fi
printf 'pin-bin-release tests passed\n'
