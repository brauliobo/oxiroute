#!/usr/bin/bash
# Proves verify-checksums.sh accepts a consistent recipe and rejects a stale local-file hash, a PKGBUILD/.SRCINFO
# disagreement and a missing local file, with and without makepkg.
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
root=$(mktemp -d)
trap 'rm -rf -- "${root}"' EXIT

fresh_copy() {
  rm -rf -- "${root}/pkg"
  mkdir -- "${root}/pkg"
  cp -- "${here}"/PKGBUILD "${here}"/.SRCINFO "${here}"/oxiroute.* "${here}"/README.md "${here}"/verify-checksums.sh \
    "${root}/pkg/"
  mkdir -- "${root}/pkg/bin"
  cp -- "${here}"/bin/PKGBUILD "${here}"/bin/.SRCINFO "${here}"/bin/oxiroute.* "${here}"/bin/README.md "${root}/pkg/bin/"
  verify=${root}/pkg/verify-checksums.sh
}

expect_rejected() {
  local label=$1 mode
  for mode in 0 1; do
    if VERIFY_CHECKSUMS_NO_MAKEPKG=${mode} "${verify}" >/dev/null 2>&1; then
      printf 'verify-checksums.sh accepted %s (no-makepkg=%s)\n' "${label}" "${mode}" >&2
      exit 1
    fi
  done
}

fresh_copy
"${verify}" >/dev/null
VERIFY_CHECKSUMS_NO_MAKEPKG=1 "${verify}" >/dev/null

fresh_copy
printf '\nedited after the checksum was recorded\n' >>"${root}/pkg/README.md"
expect_rejected 'a local file edited after its checksum was recorded'

fresh_copy
printf '\n# edited\n' >>"${root}/pkg/oxiroute.service"
expect_rejected 'an edited service unit'

fresh_copy
stale=$(sha256sum -- "${root}/pkg/oxiroute.kdl")
sed -i "s/${stale%% *}/$(printf '0%.0s' {1..64})/" "${root}/pkg/PKGBUILD" "${root}/pkg/.SRCINFO"
expect_rejected 'a stale hash recorded identically in PKGBUILD and .SRCINFO'

fresh_copy
stale=$(sha256sum -- "${root}/pkg/oxiroute.kdl")
sed -i "s/${stale%% *}/$(printf '1%.0s' {1..64})/" "${root}/pkg/.SRCINFO"
expect_rejected 'a .SRCINFO hash that differs from PKGBUILD'

fresh_copy
sed -i 's/^pkgrel=.*/pkgrel=9/' "${root}/pkg/PKGBUILD"
expect_rejected 'a PKGBUILD edit not regenerated into .SRCINFO'

fresh_copy
rm -- "${root}/pkg/oxiroute.lua"
expect_rejected 'a missing local source'

fresh_copy
old=$(sha256sum -- "${root}/pkg/bin/oxiroute.service")
printf '\n# diverged\n' >>"${root}/pkg/bin/oxiroute.service"
new=$(sha256sum -- "${root}/pkg/bin/oxiroute.service")
sed -i "s/${old%% *}/${new%% *}/" "${root}/pkg/bin/PKGBUILD" "${root}/pkg/bin/.SRCINFO"
expect_rejected 'a binary-recipe copy that drifted from the source recipe with a consistent hash'

fresh_copy
printf '\n' >>"${root}/pkg/bin/README.md"
expect_rejected 'a stale hash in the binary recipe'

printf 'verify-checksums tests passed\n'
