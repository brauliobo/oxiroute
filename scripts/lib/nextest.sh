#!/usr/bin/env bash
# Sourced by the scripts/test lanes: the one place that knows how nextest runs tests.
#
#   run_tiers "TIER..." [nextest arguments]     the binaries of the tiers of scripts/ci/tiers.txt
#   run_filter FILTERSET [nextest arguments]
#
# OXIROUTE_TEST_ARCHIVE names a `cargo nextest archive` of the binaries of the lane (scripts/ci/archive.sh): the lane
# unpacks it once into the workspace target directory, where the tests find the binaries their build recorded, and runs
# it instead of building. Without an archive a lane builds only the test binaries of its own tiers. Most tests
# wait on sockets and clocks, so a lane runs OXIROUTE_TEST_THREADS of them at once (default: three per CPU), which keeps
# four CPUs busy. A lane then fails when a test ran longer than OXIROUTE_TEST_BUDGET seconds (scripts/ci/test_budget.sh).

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
    rm -f "${build_metadata}"/*/junit.xml
    cargo nextest run "${source[@]}" --config-file scripts/ci/nextest.toml \
      --test-threads "${OXIROUTE_TEST_THREADS:-$(($(nproc) * 3))}" -E "${filterset}" "$@" &&
      scripts/ci/test_budget.sh "${build_metadata}"/*/junit.xml
  )
}

require_archived() {
  local missing
  # shellcheck disable=SC2086
  missing=$(comm -23 <("${tiers_script}" ids $1 | sort) \
    <(jq -r '."rust-binaries"[]."binary-id"' "${build_metadata}/binaries-metadata.json" | sort))
  [[ -z ${missing} ]] || {
    printf 'the archive %s lacks test binaries of the tiers %s (scripts/ci/archive.sh):\n%s\n' \
      "${OXIROUTE_TEST_ARCHIVE}" "$1" "${missing}" >&2
    exit 1
  }
}

run_tiers() {
  local selection=$1 build=()
  shift
  if [[ -n ${OXIROUTE_TEST_ARCHIVE:-} ]]; then
    unpack_archive
    require_archived "${selection}"
  else
    # shellcheck disable=SC2086
    read -ra build <<<"$("${tiers_script}" targets ${selection})"
  fi
  # shellcheck disable=SC2086
  run_filter "$("${tiers_script}" filter ${selection})" "${build[@]}" "$@"
}
