#!/usr/bin/env bash
# The plane-independent suites once, on the default plane chain. Every other tier (planes, tls, slow) is a lane of its
# own and the documentation tests are doc.sh; scripts/ci/tiers.txt says which binary runs where.
set -euo pipefail

# shellcheck source=../lib/nextest.sh
source "$(dirname -- "${BASH_SOURCE[0]}")/../lib/nextest.sh"

unset OXIROUTE_TEST_HTTP_DATA_PLANE OXIROUTE_INTERNAL_TEST_TLS_BACKEND EPOLL_H1_DRIVER EPOLL_H1_TRIGGER EPOLL_H1_SCHEDULING
run_tiers core "$@"
