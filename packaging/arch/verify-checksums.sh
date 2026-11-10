#!/usr/bin/bash
# Verifies the local-file checksums pinned in an Arch package recipe and that .SRCINFO agrees with its PKGBUILD.
#
#   verify-checksums.sh [PACKAGE_DIR...]     (default: the directory of this script)
#
# Every source without a URL must exist next to the PKGBUILD and match its sha256sums entry; PKGBUILD and .SRCINFO must
# list the same pkgver, pkgrel, sources and checksums. makepkg --printsrcinfo must reproduce .SRCINFO exactly when
# makepkg is installed (VERIFY_CHECKSUMS_NO_MAKEPKG=1 forces the shell-only comparison). Remote sources are pinned by
# their sha256sums entry and checked by makepkg --verifysource at build time, not here.
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)

# Prints "key<TAB>value" lines for the fields both files must agree on, from the sourced PKGBUILD.
pkgbuild_fields() {
  (
    source "$1/PKGBUILD"
    printf 'pkgver\t%s\npkgrel\t%s\n' "${pkgver}" "${pkgrel}"
    printf 'source\t%s\n' "${source[@]}"
    printf 'sha256sums\t%s\n' "${sha256sums[@]}"
  )
}

srcinfo_fields() {
  sed -n -E 's/^[[:space:]]*(pkgver|pkgrel|source|sha256sums) = (.*)$/\1\t\2/p' "$1/.SRCINFO"
}

verify_local_sources() {
  local dir=$1 sources sums i file actual
  mapfile -t sources < <(pkgbuild_fields "${dir}" | sed -n 's/^source\t//p')
  mapfile -t sums < <(pkgbuild_fields "${dir}" | sed -n 's/^sha256sums\t//p')
  (( ${#sources[@]} == ${#sums[@]} )) || { printf '%s: %d sources but %d sha256sums\n' "${dir}" "${#sources[@]}" "${#sums[@]}" >&2; return 1; }
  for i in "${!sources[@]}"; do
    [[ ${sources[i]} == *://* ]] && continue
    file=${sources[i]}
    [[ -f ${dir}/${file} ]] || { printf '%s: local source %s is missing\n' "${dir}" "${file}" >&2; return 1; }
    actual=$(sha256sum -- "${dir}/${file}")
    [[ ${actual%% *} == "${sums[i]}" ]] || {
      printf '%s: stale sha256 for %s\n  PKGBUILD: %s\n  actual:   %s\n' "${dir}" "${file}" "${sums[i]}" "${actual%% *}" >&2
      return 1
    }
  done
}

verify_srcinfo() {
  local dir=$1
  diff -u <(pkgbuild_fields "${dir}") <(srcinfo_fields "${dir}") >&2 || { printf '%s: .SRCINFO disagrees with PKGBUILD\n' "${dir}" >&2; return 1; }
  if [[ ${VERIFY_CHECKSUMS_NO_MAKEPKG:-0} != 1 ]] && command -v makepkg >/dev/null 2>&1; then
    makepkg --printsrcinfo --dir "${dir}" | diff -u "${dir}/.SRCINFO" - >&2 || { printf '%s: .SRCINFO is not what makepkg generates\n' "${dir}" >&2; return 1; }
  fi
}

for dir in "${@:-${here}}"; do
  dir=$(cd -- "${dir}" && pwd)
  verify_local_sources "${dir}"
  verify_srcinfo "${dir}"
  printf 'verified package checksums in %s\n' "${dir}"
done
