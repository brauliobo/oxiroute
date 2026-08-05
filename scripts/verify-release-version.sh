#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
version=${1:-}

if [[ -z "${version}" || "${version}" == -* ]]; then
  printf 'usage: %s VERSION\n' "${BASH_SOURCE[0]}" >&2
  exit 2
fi

workspace_version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "${repo_dir}/Cargo.toml")
package_version=$(sed -n 's/^pkgver=//p' "${repo_dir}/packaging/arch/PKGBUILD")
srcinfo_version=$(sed -n 's/^[[:space:]]*pkgver = //p' "${repo_dir}/packaging/arch/.SRCINFO")

[[ "${workspace_version}" == "${version}" ]] || {
  printf 'Cargo.toml version mismatch: expected %s, found %s\n' "${version}" "${workspace_version}" >&2
  exit 1
}
[[ "${package_version}" == "${version}" ]] || {
  printf 'PKGBUILD version mismatch: expected %s, found %s\n' "${version}" "${package_version}" >&2
  exit 1
}
[[ "${srcinfo_version}" == "${version}" ]] || {
  printf '.SRCINFO version mismatch: expected %s, found %s\n' "${version}" "${srcinfo_version}" >&2
  exit 1
}
[[ -f "${repo_dir}/docs/RELEASE_NOTES_${version}.md" ]] || {
  printf 'release notes missing for %s\n' "${version}" >&2
  exit 1
}

printf 'verified release version %s across workspace, package metadata, and release notes\n' "${version}"
