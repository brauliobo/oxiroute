#!/usr/bin/env bash
# Runs the test binaries that start `oxiroute` against the shipped binary (a BOLT-rewritten or panic=abort one) in place
# of the one cargo built, from the nextest archive of the workspace (OXIROUTE_TEST_ARCHIVE, see scripts/lib/nextest.sh):
# every integration test that starts the process (process, supervised, CLI and panic tests) then drives the final
# artefact, where BOLT bugs live. The other test binaries do not execute it and run in ci.yml. A test binary starts the
# process when it embeds the path cargo gave it as CARGO_BIN_EXE_oxiroute, so the selection follows the tests, and the
# slow tier runs as well.
#
#   scripts/test/release/shipped.sh BIN [nextest arguments]
set -euo pipefail

# shellcheck source=../../lib/nextest.sh
source "$(dirname -- "${BASH_SOURCE[0]}")/../../lib/nextest.sh"

binary=${1:-}
[[ -f ${binary} && ${binary} != -* ]] || { echo "usage: $0 BIN [nextest arguments]" >&2; exit 2; }
shift
unpack_archive
metadata=${build_metadata}/binaries-metadata.json
built=$(jq -r '."rust-build-meta"."target-directory"' "${metadata}")
installed=${repo_dir}/target/debug/oxiroute
[[ ${built} -ef ${repo_dir}/target ]] || {
  echo "the archive was built in ${built}: its tests start the binary at that path, not in ${repo_dir}/target" >&2
  exit 1
}

starting_filter() {
  jq -r --arg built "${built}" --arg here "${repo_dir}/target" \
    '."rust-binaries"[] | "\(."binary-id") \($here + (."binary-path" | ltrimstr($built)))"' "${metadata}" |
    while read -r id path; do
      ! grep -aqF -- "${built}/debug/oxiroute" "${path}" || printf 'binary_id(=%s)\n' "${id}"
    done | paste -sd '|' | sed 's/|/ | /g'
}

filter=$(starting_filter)
[[ -n ${filter} ]] || { echo "no test binary of the archive starts oxiroute" >&2; exit 1; }
expected=$(sha256sum "${binary}" | cut -d' ' -f1)
install -m 755 "${binary}" "${installed}"
unset OXIROUTE_TEST_HTTP_DATA_PLANE OXIROUTE_INTERNAL_TEST_TLS_BACKEND EPOLL_H1_DRIVER EPOLL_H1_TRIGGER EPOLL_H1_SCHEDULING
status=0
OXIROUTE_TEST_BUDGET=none run_filter "${filter}" --no-tests=fail "$@" || status=$?
[[ $(sha256sum "${installed}" | cut -d' ' -f1) == "${expected}" ]] || {
  echo "the binary under test changed during the run; the results above are not for ${binary}" >&2
  exit 1
}
exit "${status}"
