#!/usr/bin/env bash
# Builds the test binaries the named lane archives need, once, and writes one nextest archive per name: a lane downloads
# only the binaries of its own tiers (scripts/ci/tiers.sh archive-tiers) and nothing else is built. The archives are
# written side by side and compressed with the long-distance window, because the binaries link the same server code.
# CARGO_BUILD_JOBS bounds the build.
#
#   archive.sh DIR NAME...      writes DIR/NAME.tar.zst for each NAME: planes, tls, core or all (every tier)
set -euo pipefail

repo_dir=$(cd -- "${BASH_SOURCE[0]%/*}/../.." && pwd)
tiers=${repo_dir}/scripts/ci/tiers.sh

(($# >= 2)) || { printf 'usage: archive.sh DIR NAME...\n' >&2; exit 2; }
out=$1
shift
mkdir -p "${out}"
out=$(cd -- "${out}" && pwd)

tiers_of() {
  local name
  for name in "$@"; do "${tiers}" archive-tiers "${name}"; done
}

archive() {
  local name=$1 build=$2 tiers_to_run
  tiers_to_run=$(tiers_of "${name}")
  # shellcheck disable=SC2086
  cargo nextest archive --workspace --locked ${build} \
    -E "$("${tiers}" binaries ${tiers_to_run})" --zstd-long --zstd-level 12 --archive-file "${out}/${name}.tar.zst"
}

cd "${repo_dir}"
# shellcheck disable=SC2046
build=$("${tiers}" targets $(tiers_of "$@"))
# shellcheck disable=SC2086
cargo test --no-run --workspace --locked ${build}
pids=()
for name in "$@"; do
  archive "${name}" "${build}" &
  pids+=($!)
done
status=0
for pid in "${pids[@]}"; do
  wait "${pid}" || status=1
done
exit "${status}"
