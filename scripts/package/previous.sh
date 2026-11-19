#!/usr/bin/env bash
# Downloads the package of the family and architecture from the newest published release before CURRENT_TAG that
# carries one, for the upgrade test, and prints its path. Prints nothing when no earlier release has such a package
# (the first release with packages has no upgrade source); any other failure fails the script.
#
#   previous.sh FAMILY ARCH CURRENT_TAG OUT_DIR
#
# Needs the GitHub CLI and GH_TOKEN (read access to the repository's releases).
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source "${here}/families.sh"

family=${1:-} arch=${2:-} current=${3:-} out=${4:-}
[[ $# == 4 ]] || { echo "usage: $0 FAMILY ARCH CURRENT_TAG OUT_DIR" >&2; exit 2; }
require_member family "${family}" "${package_families[@]}"
require_family_arch "${family}" "${arch}"
mkdir -p -- "${out}"

pattern=$(package_glob "${family}" "${arch}")

package_asset_of() {
  local name
  while read -r name; do
    # shellcheck disable=SC2053
    if [[ ${name} == ${pattern} ]]; then
      echo "${name}"
      return
    fi
  done < <(gh release view "$1" --json assets --jq '.assets[].name')
}

while read -r tag; do
  [[ ${tag} != "${current}" ]] || continue
  asset=$(package_asset_of "${tag}")
  [[ -n ${asset} ]] || continue
  gh release download "${tag}" --dir "${out}" --pattern "${asset}"
  printf '%s/%s\n' "${out}" "${asset}"
  exit 0
done < <(gh release list --exclude-drafts --exclude-pre-releases --limit 30 --json tagName --jq '.[].tagName')
