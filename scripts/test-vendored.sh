#!/usr/bin/env bash
# Runs the vendored Pingora test suites against the vendored sources, inside a network namespace that has only
# loopback, so a test that needs the Internet, a fixed port or a host sysctl fails here the same way it does in a
# sandbox. Each crate runs with its default features and its unit tests again with the OpenSSL backend the
# product uses. Usage: scripts/test-vendored.sh [crate...] [-- cargo test arguments]
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
all=(core proxy load-balancing runtime timeout)
declare -A openssl_features=([core]=openssl,connection_filter [proxy]=openssl [load-balancing]=openssl)

crates=()
while (($#)) && [[ $1 != -- ]]; do crates+=("$1"); shift; done
shift || true
((${#crates[@]})) || crates=("${all[@]}")

patches=()
for crate in "${all[@]}"; do
  patches+=(--config "patch.crates-io.pingora-${crate}.path=\"${repo_dir}/vendor/pingora-${crate}\"")
done

run() {
  local crate=$1
  shift
  (cd "${repo_dir}/vendor/pingora-${crate}" &&
    unshare --map-root-user --net bash -c \
      'ip link set lo up && exec cargo test --offline --no-fail-fast "$@"' _ "${patches[@]}" "$@")
}

status=0
for crate in "${crates[@]}"; do
  printf '== pingora-%s\n' "$crate"
  run "$crate" "$@" || status=1
  if [[ -n ${openssl_features[$crate]:-} ]]; then
    printf '== pingora-%s (%s)\n' "$crate" "${openssl_features[$crate]}"
    run "$crate" --features "${openssl_features[$crate]}" --lib "$@" || status=1
  fi
done
exit "$status"
