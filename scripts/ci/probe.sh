#!/usr/bin/env bash
# Fails loudly unless this machine can run the data plane a CI leg is about to test: prints the image and kernel, then
# starts the real binary on the plane (the binary refuses a plane it cannot run at plan time) and checks that it
# announces that plane. Run it before the plane lane so a leg never runs on another plane than its name says.
#
#   probe.sh tokio|epoll|io_uring
set -euo pipefail

plane=${1:?usage: probe.sh tokio|epoll|io_uring}
# shellcheck source=../lib/nextest.sh
source "$(dirname -- "${BASH_SOURCE[0]}")/../lib/nextest.sh"

printf 'runner image: %s %s\n' "${ImageOS:-unknown}" "${ImageVersion:-}"
printf 'kernel: %s\n' "$(uname -r)"
printf 'limits: locked-memory=%s open-files=%s address-space=%s\n' "$(ulimit -l)" "$(ulimit -n)" "$(ulimit -v)"
for setting in /proc/sys/kernel/io_uring_disabled /proc/sys/vm/overcommit_memory /proc/sys/vm/max_map_count \
  /proc/sys/fs/pipe-user-pages-soft /sys/fs/cgroup/memory.max; do
  printf '%s: %s\n' "${setting}" "$(cat "${setting}" 2>/dev/null || printf 'absent')"
done

OXIROUTE_TEST_HTTP_DATA_PLANE=${plane} run_filter 'binary_id(=oxiroute::plane_probe)' --no-tests=fail
