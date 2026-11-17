#!/usr/bin/env bash
# The plane-sensitive suites on one or every HTTP data plane, with the engine suites of the plane's driver. The leg of CI
# named after a plane runs exactly this lane; locally `planes.sh all` runs them all.
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

plane_lanes() {
  case $1 in
    tokio) lane run_tiers planes ;;
    epoll)
      lane run_tiers "planes engine epoll"
      EPOLL_H1_TRIGGER=level lane run_tiers "engine epoll"
      ;;
    io_uring) EPOLL_H1_DRIVER=io_uring lane run_tiers "planes engine uring" --profile io_uring ;;
    *) printf 'usage: planes.sh [tokio|epoll|io_uring|all]\n' >&2; exit 2 ;;
  esac
}

for plane in "${planes[@]}"; do
  "${repo_dir}/scripts/ci/probe.sh" "${plane}"
  OXIROUTE_TEST_HTTP_DATA_PLANE=${plane} plane_lanes "${plane}"
done
exit "${status}"
