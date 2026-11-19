#!/usr/bin/env bash
# The asset set of a release: what it must carry, the check that refuses an incomplete or plain-built one, and the
# one SHA256SUMS that covers it.
#
#   assets.sh list VERSION         the expected assets, one per line, SHA256SUMS last
#   assets.sh verify DIR VERSION   fail unless every expected asset is in DIR, every build record says
#                                  profile-guided (ALLOW_PLAIN=1 accepts the plain builds of a dry run), every .sha256
#                                  matches its file and the record of every package is the one of the tarball of its
#                                  architecture (one binary per architecture)
#   assets.sh sums DIR VERSION     verify, then write DIR/SHA256SUMS over every other asset and check it
#   assets.sh notes DIR VERSION    the "Assets" section of the release text: the build line of every tarball (the
#                                  pgo-note-*.md files of DIR) and the packages
#
# Environment: X86_64_FLAVOURS (the flavours of the x86_64 tarball, default "unwind abort"; add v3 for a release
# that carries the x86-64-v3 flavour).
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source "${here}/families.sh"

usage() { printf 'usage: %s list VERSION | verify DIR VERSION | sums DIR VERSION | notes DIR VERSION\n' "${BASH_SOURCE[0]}" >&2; exit 2; }

require_complete() {
  local dir=$1 version=$2 asset missing=()
  while IFS= read -r asset; do
    [[ -s ${dir}/${asset} ]] || missing+=("${asset}")
  done < <(release_assets "${version}")
  ((${#missing[@]} == 0)) || { printf 'incomplete release, missing or empty in %s:\n  %s\n' "${dir}" "${missing[*]}" >&2; return 1; }
}

require_records() {
  local dir=$1 version=$2 record bad=()
  while IFS= read -r record; do
    grep -q "\"version\": \"${version}\"" "${dir}/${record}" || bad+=("${record}: not a record of ${version}")
    [[ ${ALLOW_PLAIN:-0} == 1 ]] || grep -q '"pgo": true' "${dir}/${record}" || bad+=("${record}: not profile-guided")
  done < <(release_assets "${version}" | grep '\.build\.json$')
  ((${#bad[@]} == 0)) || { printf 'refusing the release:\n' >&2; printf '  %s\n' "${bad[@]}" >&2; return 1; }
}

require_checksums() {
  local dir=$1 version=$2 sidecar
  while IFS= read -r sidecar; do
    (cd "${dir}" && sha256sum --check --status "${sidecar}") || { printf '%s does not match its file\n' "${sidecar}" >&2; return 1; }
  done < <(release_assets "${version}" | grep '\.sha256$')
}

require_one_binary_per_arch() {
  local dir=$1 version=$2 family arch bad=() record
  while read -r family arch; do
    record=${dir}/$(tarball_stem "${version}" "$(package_target "${arch}")" unwind).build.json
    cmp -s -- "${record}" "${dir}/$(package_file "${family}" "${arch}" "${version}").build.json" ||
      bad+=("$(package_file "${family}" "${arch}" "${version}"): not the binary of the ${arch} tarball")
  done < <(package_pairs)
  ((${#bad[@]} == 0)) || { printf 'refusing the release:\n' >&2; printf '  %s\n' "${bad[@]}" >&2; return 1; }
}

verify() {
  require_complete "$1" "$2"
  require_records "$1" "$2"
  require_checksums "$1" "$2"
  require_one_binary_per_arch "$1" "$2"
}

sums() {
  local dir=$1 version=$2
  verify "${dir}" "${version}"
  (cd "${dir}" && release_assets "${version}" | LC_ALL=C sort | xargs sha256sum -- > SHA256SUMS &&
    sha256sum --check --quiet SHA256SUMS)
}

notes() {
  local dir=$1 version=$2 note family arch
  printf '## Assets\n\nEvery file of this release is listed with its SHA-256 in `SHA256SUMS`; the workflow attests the archive, the\ntarballs and the packages.\n\n'
  for note in "${dir}"/pgo-note-*.md; do printf -- '- %s\n' "$(cat -- "${note}")"; done
  printf '\nPackages (each repackages the binary of the tarball of its architecture, which is built once on glibc %s, and is\ninstalled in fresh containers of every supported image of its family before it is published):\n\n' "${glibc_floor}"
  while read -r family arch; do printf -- '- `%s`\n' "$(package_file "${family}" "${arch}" "${version}")"; done < <(package_pairs)
}

case ${1:-} in
  notes) [[ $# == 3 ]] || usage; require_version "$3"; notes "$2" "$3" ;;
  list) [[ $# == 2 ]] || usage; require_version "$2"; release_assets "$2"; echo SHA256SUMS ;;
  verify) [[ $# == 3 ]] || usage; require_version "$3"; verify "$2" "$3" ;;
  sums) [[ $# == 3 ]] || usage; require_version "$3"; sums "$2" "$3" ;;
  *) usage ;;
esac
