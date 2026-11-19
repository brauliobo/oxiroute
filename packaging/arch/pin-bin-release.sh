#!/usr/bin/bash
# Pins bin/PKGBUILD and bin/.SRCINFO to a published release; verify-checksums.sh then checks that they agree (and, where
# makepkg is installed, that .SRCINFO is what makepkg generates).
#
#   pin-bin-release.sh VERSION
#
# Reads the checksum of the Arch package from the release asset oxiroute-VERSION-1-x86_64.pkg.tar.zst.sha256.
# OXIROUTE_RELEASE_URL overrides the download location (a directory holding the files, for tests).
set -euo pipefail

dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/bin
version=${1:-}
[[ -n ${version} && ${version} != -* ]] || { printf 'usage: %s VERSION\n' "${BASH_SOURCE[0]}" >&2; exit 2; }
url=$(sed -n "s/^url='\(.*\)'/\1/p" "${dir}/PKGBUILD")
release_url=${OXIROUTE_RELEASE_URL:-${url}/releases/download/v${version}}

package=oxiroute-${version}-1-x86_64.pkg.tar.zst
package_sha256=$(curl -fsSL "${release_url}/${package}.sha256")
[[ ${package_sha256} == *"  ${package}" ]] || { printf 'unexpected checksum file for %s\n' "${package}" >&2; exit 1; }
package_sha256=${package_sha256%% *}
[[ ${package_sha256} =~ ^[0-9a-f]{64}$ ]] || { printf 'not a sha256 checksum: %s\n' "${package_sha256}" >&2; exit 1; }

old_version=$(sed -n 's/^pkgver=//p' "${dir}/PKGBUILD")
srcinfo_first=$(grep -n '^[[:space:]]*sha256sums = ' "${dir}/.SRCINFO" | head -n 1 | cut -d: -f1)
sed -i -e "s/^pkgver=.*/pkgver=${version}/" -e 's/^pkgrel=.*/pkgrel=1/' \
  -e "s/^sha256sums=('[0-9a-f]\{64\}')/sha256sums=('${package_sha256}')/" "${dir}/PKGBUILD"
sed -i -e "s/${old_version//./\\.}/${version}/g" -e 's/^\([[:space:]]*pkgrel = \).*/\11/' \
  -e "${srcinfo_first}s/[0-9a-f]\{64\}/${package_sha256}/" "${dir}/.SRCINFO"
"${dir}/../verify-checksums.sh" "${dir}"
