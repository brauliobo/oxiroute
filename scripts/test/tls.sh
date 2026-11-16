#!/usr/bin/env bash
# The downstream TLS suites once per TLS backend: the same tests, OXIROUTE_INTERNAL_TEST_TLS_BACKEND selects the backend
# of the profile their harness builds (wire_tls_rustls also runs both backends in one process).
#
#   tls.sh [openssl|rustls|all]    (default: all)
set -euo pipefail

# shellcheck source=../lib/nextest.sh
source "$(dirname -- "${BASH_SOURCE[0]}")/../lib/nextest.sh"

selected=${1:-all}
backends=(openssl rustls)
[[ ${selected} == all ]] || backends=("${selected}")
status=0

for backend in "${backends[@]}"; do
  printf '\n== downstream TLS backend: %s\n' "${backend}"
  OXIROUTE_INTERNAL_TEST_TLS_BACKEND=${backend} run_tiers tls || status=1
done
exit "${status}"
