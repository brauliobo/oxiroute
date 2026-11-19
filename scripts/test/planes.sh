#!/usr/bin/env bash
# The plane-sensitive suites on one or every HTTP data plane, with the engine suites of the plane's driver. The leg of CI
# named after a plane runs exactly this lane; locally `planes.sh all` runs them all. The lane fails when a test of the
# planes tier never chooses a plane (scripts/ci/plane_use.sh).
#
#   planes.sh [tokio|epoll|io_uring|all]    (default: all)
set -euo pipefail

# shellcheck source=../lib/nextest.sh
source "$(dirname -- "${BASH_SOURCE[0]}")/../lib/nextest.sh"

selected=${1:-all}
planes=(tokio epoll io_uring)
[[ ${selected} == all ]] || planes=("${selected}")
status=0

lane() {
  printf '\n== %s\n' "$*"
  "$@" || status=1
}

planes_lane() {
  local report
  report=$(mktemp)
  OXIROUTE_TEST_PLANE_USE_FILE=${report} lane run_tiers "$@"
  lane "${repo_dir}/scripts/ci/plane_use.sh" check "${report}" "${build_metadata}"/*/junit.xml
  rm -f "${report}"
}

plane_lanes() {
  case $1 in
    tokio) planes_lane planes ;;
    epoll)
      planes_lane "planes engine epoll"
      EPOLL_H1_TRIGGER=level lane run_tiers "engine epoll"
      ;;
    io_uring) EPOLL_H1_DRIVER=io_uring planes_lane "planes engine uring" ;;
    *) printf 'usage: planes.sh [tokio|epoll|io_uring|all]\n' >&2; exit 2 ;;
  esac
}

raise_memlock() {
  sudo -n prlimit --pid $$ --memlock=unlimited:unlimited
  [[ $(ulimit -l) == unlimited ]] || { printf 'locked memory is %s KiB after raising it\n' "$(ulimit -l)" >&2; exit 1; }
}

for plane in "${planes[@]}"; do
  [[ ${plane} != io_uring || -z ${OXIROUTE_RAISE_MEMLOCK:-} ]] || raise_memlock
  "${repo_dir}/scripts/ci/probe.sh" "${plane}"
  OXIROUTE_TEST_HTTP_DATA_PLANE=${plane} plane_lanes "${plane}"
done
exit "${status}"
