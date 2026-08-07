#!/usr/bin/bash
set -euo pipefail

package_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_dir=$(git -C "${package_dir}" rev-parse --show-toplevel)

# Read only declarative PKGBUILD values needed to verify the release archive.
source "${package_dir}/PKGBUILD"
archive_name="oxiroute-${pkgver}.tar.gz"
expected_sha256=${sha256sums[0]}
work_dir="${package_dir}/.makepkg"
source_dir="${work_dir}/sources"
archive=

if [[ -n "${1:-}" && "${1}" != -* ]]; then
  archive=${1}
  shift
  archive=$(realpath -- "${archive}")
else
  mkdir -p -- "${source_dir}"
  archive="${source_dir}/${archive_name}"
  source_date_epoch=${SOURCE_DATE_EPOCH:-$(
    git -C "${repo_dir}" log -1 --format=%ct HEAD -- . ':(exclude)packaging/arch'
  )}
  export SOURCE_DATE_EPOCH=${source_date_epoch}
  "${repo_dir}/scripts/create-release-archive.sh" "${archive}" "${pkgver}"
fi

"${repo_dir}/scripts/verify-release-archive.sh" "${archive}" "${pkgver}" "${expected_sha256}"
actual_sha256=$(sha256sum -- "${archive}")
actual_sha256=${actual_sha256%% *}
if [[ "${actual_sha256}" != "${expected_sha256}" ]]; then
  printf 'source archive checksum mismatch\nexpected: %s\nactual:   %s\n' \
    "${expected_sha256}" "${actual_sha256}" >&2
  exit 1
fi

"${package_dir}/test-service.sh"

mkdir -p -- "${source_dir}" "${work_dir}/build" "${work_dir}/packages"
cached_archive="${source_dir}/${archive_name}"
if [[ "${archive}" != "${cached_archive}" ]]; then
  cp -- "${archive}" "${cached_archive}"
fi

printf 'verified %s (%s)\n' "${archive_name}" "${actual_sha256}"
SRCDEST="${source_dir}" \
BUILDDIR="${work_dir}/build" \
PKGDEST="${work_dir}/packages" \
SRCPKGDEST="${work_dir}/packages" \
makepkg --dir "${package_dir}" --cleanbuild "$@"
