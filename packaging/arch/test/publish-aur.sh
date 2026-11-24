#!/usr/bin/bash
# Proves publish-aur.sh refuses an unpinned recipe or another version, then fills the AUR repositories (local bare
# repositories stand in for the remotes) with exactly the files of each recipe, removes what the old recipe left behind,
# commits "Update to VERSION-1" once and pushes both. Needs git and makepkg; without makepkg it reports a skip.
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
command -v makepkg > /dev/null && command -v git > /dev/null || { printf 'publish-aur tests skipped: makepkg and git are required\n'; exit 0; }
root=$(mktemp -d)
trap 'rm -rf -- "${root}"' EXIT
export GIT_AUTHOR_NAME=alice GIT_AUTHOR_EMAIL=alice@example.com GIT_COMMITTER_NAME=alice GIT_COMMITTER_EMAIL=alice@example.com

version=$(sed -n 's/^pkgver=//p' "${here}/PKGBUILD")
mkdir -p -- "${root}/pkg/bin" "${root}/release" "${root}/remotes"
cp -- "${here}"/verify-checksums.sh "${here}"/pin-bin-release.sh "${here}"/publish-aur.sh "${here}"/PKGBUILD "${here}"/.SRCINFO \
  "${here}"/README.md "${here}"/oxiroute.* "${root}/pkg/"
cp -- "${here}"/bin/PKGBUILD "${here}"/bin/.SRCINFO "${here}"/bin/oxiroute.install "${here}"/bin/README.md "${root}/pkg/bin/"
sed -i -E "s/[0-9a-f]{64}/$(printf '0%.0s' {1..64})/" "${root}/pkg/bin/PKGBUILD" "${root}/pkg/bin/.SRCINFO"

publish() { AUR_REMOTE_BASE=file://${root}/remotes "${root}/pkg/publish-aur.sh" "$@"; }
remote_files() { git -C "${root}/remotes/$1.git" ls-tree -r --name-only master | LC_ALL=C sort | tr '\n' ' '; }
remote_commits() { git -C "${root}/remotes/$1.git" log --format=%s master 2> /dev/null || true; }

for package in oxiroute oxiroute-bin; do git init --quiet --bare --initial-branch=master "${root}/remotes/${package}.git"; done
seed=${root}/seed
git clone --quiet "${root}/remotes/oxiroute-bin.git" "${seed}" 2> /dev/null
printf 'old\n' > "${seed}/oxiroute.service"
printf 'old\n' > "${seed}/PKGBUILD"
git -C "${seed}" add oxiroute.service PKGBUILD
git -C "${seed}" commit --quiet -m 'Update to 7.0.0-1'
git -C "${seed}" push --quiet origin HEAD:master 2> /dev/null

if publish "${version}" "${root}/clones" > /dev/null 2>&1; then
  printf 'publish-aur.sh published a recipe with the placeholder checksum\n' >&2
  exit 1
fi

name=oxiroute-${version}-1-x86_64.pkg.tar.zst
printf 'arch package\n' > "${root}/release/${name}"
(cd "${root}/release" && sha256sum "${name}" > "${name}.sha256")
OXIROUTE_RELEASE_URL=file://${root}/release "${root}/pkg/pin-bin-release.sh" "${version}" > /dev/null

if publish 9.9.9 "${root}/clones" > /dev/null 2>&1; then
  printf 'publish-aur.sh published another version than the one of the recipes\n' >&2
  exit 1
fi
[[ -z $(remote_commits oxiroute) ]] || { printf 'a refused run pushed\n' >&2; exit 1; }

publish "${version}" "${root}/clones" > /dev/null 2>&1

[[ $(remote_commits oxiroute) == "Update to ${version}-1" ]] || { printf 'oxiroute has no update commit\n' >&2; exit 1; }
[[ $(remote_commits oxiroute-bin | head -n 1) == "Update to ${version}-1" ]] || { printf 'oxiroute-bin has no update commit\n' >&2; exit 1; }
[[ $(remote_files oxiroute-bin) == '.SRCINFO PKGBUILD README.md oxiroute.install ' ]] ||
  { printf 'oxiroute-bin has unexpected files: %s\n' "$(remote_files oxiroute-bin)" >&2; exit 1; }
[[ $(remote_files oxiroute) == '.SRCINFO PKGBUILD README.md oxiroute.ensure-token oxiroute.env oxiroute.install oxiroute.kdl oxiroute.lua oxiroute.modules oxiroute.service oxiroute.sysusers oxiroute.tmpfiles ' ]] ||
  { printf 'oxiroute has unexpected files: %s\n' "$(remote_files oxiroute)" >&2; exit 1; }
git -C "${root}/remotes/oxiroute-bin.git" show master:PKGBUILD | cmp -s - "${root}/pkg/bin/PKGBUILD" ||
  { printf 'the pushed oxiroute-bin PKGBUILD is not the recipe\n' >&2; exit 1; }

publish "${version}" "${root}/clones" > /dev/null 2>&1
[[ $(remote_commits oxiroute | wc -l) == 1 ]] || { printf 'a second run committed again\n' >&2; exit 1; }
printf 'publish-aur tests passed\n'
