#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=release-archive-policy.sh
source "${repo_dir}/scripts/release-archive-policy.sh"
archive=${1:-}
version=${2:-}
shift 2 || true
expected_sha256=
compare_worktree=false

while (($# > 0)); do
  case "$1" in
    --compare-worktree)
      compare_worktree=true
      ;;
    --*)
      printf 'unknown option: %s\n' "$1" >&2
      exit 2
      ;;
    *)
      [[ -z "${expected_sha256}" ]] || {
        printf 'only one expected checksum may be supplied\n' >&2
        exit 2
      }
      expected_sha256=$1
      ;;
  esac
  shift
done

if [[ -z "${archive}" || -z "${version}" ]]; then
  printf 'usage: %s ARCHIVE VERSION [EXPECTED_SHA256] [--compare-worktree]\n' "${BASH_SOURCE[0]}" >&2
  exit 2
fi
[[ -f "${archive}" ]] || {
  printf 'release archive not found: %s\n' "${archive}" >&2
  exit 1
}

archive=$(realpath -- "${archive}")
root="oxiroute-${version}"
entries=$(mktemp)
sorted_entries=$(mktemp)
expected_entries=$(mktemp)
sorted_expected=$(mktemp)
archive_files=$(mktemp)
secret_entries=$(mktemp)
trap 'rm -f -- "${entries}" "${sorted_entries}" "${expected_entries}" "${sorted_expected}" "${archive_files}" "${secret_entries}"' EXIT

tar -tzf "${archive}" >"${entries}"
[[ -s "${entries}" ]] || {
  printf 'release archive is empty: %s\n' "${archive}" >&2
  exit 1
}
LC_ALL=C sort "${entries}" >"${sorted_entries}"
LC_ALL=C sort "${entries}" -c

declare -A required_paths=()
declare -A allowed_secret_paths=()
for path in "${RELEASE_REQUIRED_PATHS[@]}"; do required_paths["${path}"]=false; done
for path in "${RELEASE_ALLOWED_SECRET_PATHS[@]}"; do allowed_secret_paths["${path}"]=true; done
while IFS= read -r entry; do
  case "${entry}" in
    "${root}/") continue ;;
    "${root}"/*) relative=${entry#"${root}/"} ;;
    *)
      printf 'archive entry is outside %s/: %s\n' "${root}" "${entry}" >&2
      exit 1
      ;;
  esac
  [[ -n "${relative}" ]] || {
    printf 'archive contains an empty relative path\n' >&2
    exit 1
  }
  path_for_policy=${relative,,}
  if [[ -z "${allowed_secret_paths[${path_for_policy}]+allowed}" ]]; then
    for pattern in "${RELEASE_DENIED_PATH_PATTERNS[@]}" "${RELEASE_SECRET_PATH_PATTERNS[@]}"; do
      if [[ "${path_for_policy}" == ${pattern} ]]; then
        printf 'release archive contains a build artifact or secret-shaped path: %s\n' "${entry}" >&2
        exit 1
      fi
    done
  fi
  if [[ -n "${required_paths[${relative}]+required}" ]]; then required_paths["${relative}"]=true; fi
done <"${sorted_entries}"

tar \
  --extract \
  --gzip \
  --file="${archive}" \
  --to-command='
    if [ "${TAR_FILETYPE:-}" = f ]; then
       LC_ALL=C grep -aE -- "${RELEASE_SECRET_CONTENT_PATTERN}" >/dev/null
      status=$?
      if [ "${status}" -gt 1 ]; then
        exit "${status}"
      fi
      if [ "${status}" -eq 0 ]; then
        printf "%s\n" "${TAR_FILENAME}"
      fi
    fi
    exit 0
  ' \
  >"${secret_entries}"
while IFS= read -r entry; do
  case "${entry}" in
    "${root}"/*) relative=${entry#"${root}/"} ;;
    *)
      printf 'secret scan returned an entry outside %s/: %s\n' "${root}" "${entry}" >&2
      exit 1
      ;;
  esac
  [[ -n "${allowed_secret_paths[${relative,,}]+allowed}" ]] || {
    printf 'release archive contains unallowlisted private-key or credential material: %s\n' "${entry}" >&2
    exit 1
  }
done <"${secret_entries}"

for path in "${RELEASE_REQUIRED_PATHS[@]}"; do
  [[ "${required_paths[${path}]}" == true ]] || { printf 'archive is missing %s\n' "${path}" >&2; exit 1; }
done

if [[ "${compare_worktree}" == true ]]; then
  git -C "${repo_dir}" archive \
    --format=tar \
    --prefix="${root}/" \
    HEAD -- \
      . \
      "${RELEASE_ARCHIVE_EXCLUDES[@]}" | tar -tf - | \
    while IFS= read -r entry; do
      [[ "${entry}" == */ ]] || printf '%s\n' "${entry}"
    done >"${expected_entries}"
  while IFS= read -r entry; do
    [[ "${entry}" == */ ]] || printf '%s\n' "${entry}"
  done <"${sorted_entries}" >"${archive_files}"
  LC_ALL=C sort "${expected_entries}" >"${sorted_expected}"
  if ! diff -u "${sorted_expected}" "${archive_files}"; then
    printf 'release archive file list does not match Git-tracked release inputs\n' >&2
    exit 1
  fi
fi

actual_sha256=$(sha256sum -- "${archive}")
actual_sha256=${actual_sha256%% *}
if [[ -n "${expected_sha256}" && "${actual_sha256}" != "${expected_sha256}" ]]; then
  printf 'release archive checksum mismatch\nexpected: %s\nactual:   %s\n' \
    "${expected_sha256}" "${actual_sha256}" >&2
  exit 1
fi

printf 'verified release archive %s (%s)\n' "${archive}" "${actual_sha256}"
