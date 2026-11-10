#!/usr/bin/bash
# Pins bin/PKGBUILD to a published release and regenerates .SRCINFO.
#
#   pin-bin-release.sh VERSION
#
# Reads the release tarball checksum from the release asset oxiroute-VERSION-<target>.tar.gz.sha256 and hashes the
# management documentation published at the release tag. OXIROUTE_RELEASE_URL and OXIROUTE_RAW_URL override the
# download locations (they are directories holding the files, for tests).
set -euo pipefail

dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/bin
version=${1:-}
[[ -n ${version} && ${version} != -* ]] || { printf 'usage: %s VERSION\n' "${BASH_SOURCE[0]}" >&2; exit 2; }
url=$(sed -n "s/^url='\(.*\)'/\1/p" "${dir}/PKGBUILD")
target=$(sed -n "s/^_target='\(.*\)'/\1/p" "${dir}/PKGBUILD")
release_url=${OXIROUTE_RELEASE_URL:-${url}/releases/download/v${version}}
raw_url=${OXIROUTE_RAW_URL:-https://raw.githubusercontent.com/brauliobo/oxiroute/v${version}/docs}

tarball=oxiroute-${version}-${target}.tar.gz
tarball_sha256=$(curl -fsSL "${release_url}/${tarball}.sha256")
[[ ${tarball_sha256} == *"  ${tarball}" ]] || { printf 'unexpected checksum file for %s\n' "${tarball}" >&2; exit 1; }
tarball_sha256=${tarball_sha256%% *}
doc_sha256=$(curl -fsSL "${raw_url}/MANAGEMENT_CLI.md" | sha256sum)
doc_sha256=${doc_sha256%% *}

first=$(grep -n '^sha256sums=' "${dir}/PKGBUILD" | cut -d: -f1)
sed -i -e "s/^pkgver=.*/pkgver=${version}/" -e 's/^pkgrel=.*/pkgrel=1/' \
  -e "${first}s/[0-9a-f]\{64\}/${tarball_sha256}/" -e "$((first + 1))s/[0-9a-f]\{64\}/${doc_sha256}/" "${dir}/PKGBUILD"
makepkg --printsrcinfo --dir "${dir}" >"${dir}/.SRCINFO"
"${dir}/../verify-checksums.sh" "${dir}"
