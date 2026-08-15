#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=release-archive-policy.sh
source "${repo_dir}/scripts/release-archive-policy.sh"
archive=${1:-}
version=${2:-}

if [[ -z "${archive}" || -z "${version}" || "${archive}" == -* || "${version}" == -* ]]; then
  printf 'usage: %s ARCHIVE VERSION\n' "${BASH_SOURCE[0]}" >&2
  exit 2
fi

archive=$(realpath -m -- "${archive}")
mkdir -p -- "$(dirname -- "${archive}")"
source_date_epoch=${SOURCE_DATE_EPOCH:-$(git -C "${repo_dir}" log -1 --format=%ct HEAD)}
export SOURCE_DATE_EPOCH=${source_date_epoch}
temporary_archive=$(mktemp "${archive}.XXXXXX")
temporary_tree=$(mktemp -d "${archive}.tree.XXXXXX")
trap 'rm -f -- "${temporary_archive}"; rm -rf -- "${temporary_tree}"' EXIT

git -C "${repo_dir}" archive \
  --format=tar \
  HEAD -- \
  . \
  "${RELEASE_ARCHIVE_EXCLUDES[@]}" | tar -x -C "${temporary_tree}"
find "${temporary_tree}" -mindepth 1 ! -type d -printf '%P\0' | LC_ALL=C sort -z | \
  tar \
    --directory="${temporary_tree}" \
    --null \
    --no-recursion \
    --files-from=- \
    --mtime="@${source_date_epoch}" \
    --owner=0 \
    --group=0 \
    --numeric-owner \
    --transform="s|^|oxiroute-${version}/|" \
    --create \
    --file=- | gzip -n >"${temporary_archive}"

"${repo_dir}/scripts/verify-release-archive.sh" "${temporary_archive}" "${version}" --compare-worktree
mv -- "${temporary_archive}" "${archive}"
rm -rf -- "${temporary_tree}"
trap - EXIT
