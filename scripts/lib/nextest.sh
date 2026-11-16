#!/usr/bin/env bash
# Sourced by the scripts/test lanes: the one place that knows how nextest runs tests.
#
#   run_tiers "TIER..." [nextest arguments]     the binaries of the tiers of scripts/ci/tiers.txt
#   run_filter FILTERSET [nextest arguments]
#
# OXIROUTE_TEST_ARCHIVE names a `cargo nextest archive` of the workspace: the lane unpacks it once into the workspace
# target directory, where the tests find the binaries their build recorded, and runs it instead of building. Most tests
# wait on sockets and clocks, so a lane runs OXIROUTE_TEST_THREADS of them at once (default: three per CPU), which keeps
# four CPUs busy.

repo_dir=$(cd -- "${BASH_SOURCE[0]%/*}/../.." && pwd)
tiers_script=${repo_dir}/scripts/ci/tiers.sh
build_metadata=${repo_dir}/target/nextest

unpack_archive() {
  [[ -f ${build_metadata}/binaries-metadata.json ]] || zstd -dc "${OXIROUTE_TEST_ARCHIVE}" | tar -x -C "${repo_dir}"
}

run_filter() {
  local filterset=$1 source=(--workspace --locked)
  shift
  if [[ -n ${OXIROUTE_TEST_ARCHIVE:-} ]]; then
    unpack_archive
    source=(--binaries-metadata "${build_metadata}/binaries-metadata.json"
      --cargo-metadata "${build_metadata}/cargo-metadata.json"
      --target-dir-remap "${repo_dir}/target" --workspace-remap "${repo_dir}")
  fi
  (
    cd "${repo_dir}" || exit
    cargo nextest run "${source[@]}" --config-file scripts/ci/nextest.toml \
      --test-threads "${OXIROUTE_TEST_THREADS:-$(($(nproc) * 3))}" -E "${filterset}" "$@"
  )
}

run_tiers() {
  local selection=$1
  shift
  # shellcheck disable=SC2086
  run_filter "$("${tiers_script}" filter ${selection})" "$@"
}
