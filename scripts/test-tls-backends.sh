#!/usr/bin/env bash
# Runs the downstream TLS wire tests once per TLS backend: the same tests, OXIROUTE_INTERNAL_TEST_TLS_BACKEND selects
# the backend of the profile their harness builds (wire_tls_rustls.rs runs both backends in one process).
# Usage: scripts/test-tls-backends.sh [cargo test arguments, after --]. CARGO overrides the cargo command.
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
read -ra cargo <<<"${CARGO:-cargo}"
tests=(wire_tls_interop wire_tls_tickets wire_tls_handshake_pool wire_tls_rustls wire_tls_parity wire_tls_clients)
selection=()
for test in "${tests[@]}"; do selection+=(--test "$test"); done

cd "$repo_dir"
for backend in openssl rustls; do
  printf '== downstream TLS backend: %s\n' "$backend"
  OXIROUTE_INTERNAL_TEST_TLS_BACKEND=$backend "${cargo[@]}" test --offline -p oxiroute "${selection[@]}" "$@"
done
