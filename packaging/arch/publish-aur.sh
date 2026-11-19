#!/usr/bin/bash
# Updates the two AUR packages of a release from this directory: oxiroute (the source recipe, this directory) and
# oxiroute-bin (bin/, which installs the Arch package of the release). Run it by hand after the release workflow has
# published the assets and pin-bin-release.sh has pinned bin/ to them; the workflow cannot do it, the push needs the key of
# the AUR maintainer.
#
#   publish-aur.sh VERSION [CLONE_DIR]
#
# Both recipes must be at VERSION with pinned (non-placeholder) checksums, pass verify-checksums.sh and have a .SRCINFO
# that is what makepkg --printsrcinfo generates. Each AUR repository is cloned (or updated) below CLONE_DIR (default: a
# temporary directory that is removed afterwards), receives exactly the files of its recipe (the PKGBUILD, .SRCINFO,
# README.md, the install script and the local sources; any other tracked file is removed), is checked again with makepkg,
# and is committed as "Update to VERSION-PKGREL". Nothing is pushed until both repositories are ready. Authentication is
# whatever ssh offers the current user for the remote (an agent, ~/.ssh/config); no key or host name is kept here.
#
# Environment: AUR_REMOTE_BASE (default ssh://aur@aur.archlinux.org; the repository of a package is
# AUR_REMOTE_BASE/PACKAGE.git).
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
version=${1:-}
clones=${2:-}
[[ $# == 1 || $# == 2 ]] && [[ -n ${version} && ${version} != -* ]] || { printf 'usage: %s VERSION [CLONE_DIR]\n' "${BASH_SOURCE[0]}" >&2; exit 2; }
remote_base=${AUR_REMOTE_BASE:-ssh://aur@aur.archlinux.org}
command -v makepkg > /dev/null || { echo 'makepkg is required to check .SRCINFO' >&2; exit 1; }

if [[ -z ${clones} ]]; then
  clones=$(mktemp -d)
  trap 'rm -rf -- "${clones}"' EXIT
fi
mkdir -p -- "${clones}"
clones=$(cd -- "${clones}" && pwd)

recipe_field() { (source "$1/PKGBUILD" && printf '%s' "${!2}"); }

require_pinned() {
  local dir=$1 pinned
  [[ $(recipe_field "${dir}" pkgver) == "${version}" ]] || { printf '%s is not at version %s\n' "${dir}" "${version}" >&2; return 1; }
  pinned=$(recipe_field "${dir}" 'sha256sums[*]')
  [[ ${pinned} != *0000000000000000000000000000000000000000000000000000000000000000* ]] ||
    { printf '%s still has the placeholder checksum: run pin-bin-release.sh %s\n' "${dir}" "${version}" >&2; return 1; }
  "${here}/verify-checksums.sh" "${dir}" > /dev/null
}

recipe_files() {
  local dir=$1 source
  {
    printf '%s\n' PKGBUILD .SRCINFO README.md "$(recipe_field "${dir}" install)"
    for source in $(recipe_field "${dir}" 'source[*]'); do [[ ${source} == *://* ]] || printf '%s\n' "${source}"; done
  } | sed '/^$/d' | LC_ALL=C sort -u
}

clone_of() {
  local package=$1 clone=${clones}/$1
  if [[ -d ${clone}/.git ]]; then git -C "${clone}" pull --quiet --ff-only; else git clone --quiet "${remote_base}/${package}.git" "${clone}"; fi
}

fill_clone() {
  local package=$1 dir=$2 clone=${clones}/$1 file keep
  keep=$(recipe_files "${dir}")
  while IFS= read -r file; do
    [[ -z ${file} ]] || grep -qxF -- "${file}" <<< "${keep}" || git -C "${clone}" rm --quiet -f -- "${file}"
  done < <(git -C "${clone}" ls-files)
  while IFS= read -r file; do cp -- "${dir}/${file}" "${clone}/${file}"; done <<< "${keep}"
  (cd -- "${clone}" && makepkg --printsrcinfo | diff -u .SRCINFO - > /dev/null) ||
    { printf '%s: .SRCINFO is not what makepkg generates\n' "${package}" >&2; return 1; }
  git -C "${clone}" add -- ${keep}
}

commit_clone() {
  local package=$1 dir=$2 clone=${clones}/$1
  if git -C "${clone}" diff --cached --quiet; then
    printf '%s: nothing to publish\n' "${package}"
  else
    git -C "${clone}" commit --quiet -m "Update to ${version}-$(recipe_field "${dir}" pkgrel)"
    printf '%s: committed Update to %s-%s\n' "${package}" "${version}" "$(recipe_field "${dir}" pkgrel)"
  fi
}

declare -A recipe_dir=([oxiroute]=${here} [oxiroute-bin]=${here}/bin)
packages=(oxiroute oxiroute-bin)

for package in "${packages[@]}"; do require_pinned "${recipe_dir[${package}]}"; done
for package in "${packages[@]}"; do
  clone_of "${package}"
  fill_clone "${package}" "${recipe_dir[${package}]}"
  commit_clone "${package}" "${recipe_dir[${package}]}"
done
for package in "${packages[@]}"; do
  git -C "${clones}/${package}" push origin HEAD:master
done
