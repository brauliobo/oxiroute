#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
archive=${1:-}
version=${2:-}
shift 2 || true
expected_sha256=
compare_worktree=false

while (($# > 0)); do
  case "$1" in
    --compare-worktree)
      compare_worktree=true
      ;;
    --*)
      printf 'unknown option: %s\n' "$1" >&2
      exit 2
      ;;
    *)
      [[ -z "${expected_sha256}" ]] || {
        printf 'only one expected checksum may be supplied\n' >&2
        exit 2
      }
      expected_sha256=$1
      ;;
  esac
  shift
done

if [[ -z "${archive}" || -z "${version}" ]]; then
  printf 'usage: %s ARCHIVE VERSION [EXPECTED_SHA256] [--compare-worktree]\n' "${BASH_SOURCE[0]}" >&2
  exit 2
fi
[[ -f "${archive}" ]] || {
  printf 'release archive not found: %s\n' "${archive}" >&2
  exit 1
}

archive=$(realpath -- "${archive}")
root="oxiroute-${version}"
entries=$(mktemp)
sorted_entries=$(mktemp)
expected_entries=$(mktemp)
sorted_expected=$(mktemp)
private_key_entries=$(mktemp)
trap 'rm -f -- "${entries}" "${sorted_entries}" "${expected_entries}" "${sorted_expected}" "${private_key_entries}"' EXIT

tar -tzf "${archive}" >"${entries}"
[[ -s "${entries}" ]] || {
  printf 'release archive is empty: %s\n' "${archive}" >&2
  exit 1
}
LC_ALL=C sort "${entries}" >"${sorted_entries}"
LC_ALL=C sort "${entries}" -c

has_cargo_lock=false
has_cargo_toml=false
has_license=false
has_ui_lock=false
while IFS= read -r entry; do
  case "${entry}" in
    "${root}"/*) relative=${entry#"${root}/"} ;;
    *)
      printf 'archive entry is outside %s/: %s\n' "${root}" "${entry}" >&2
      exit 1
      ;;
  esac
  [[ -n "${relative}" ]] || {
    printf 'archive contains an empty relative path\n' >&2
    exit 1
  }
  case "${relative}" in
    crates/oxiroute-import/tests/fixtures/haproxy/tls-chain.pem.key|\
    crates/oxiroute-import/tests/fixtures/haproxy/tls-no-identities.pem.key|\
    crates/oxiroute-import/tests/fixtures/nginx/proxy-key.pem|\
    crates/oxiroute-import/tests/fixtures/nginx/proxy-mismatched-key.pem|\
    crates/oxiroute-server/src/tls/tests.rs|\
    crates/oxiroute-server/tests/fixtures/origin-key.pem|\
    crates/oxiroute-server/tests/fixtures/proxy-a-key.pem|\
    crates/oxiroute-server/tests/fixtures/proxy-b-key.pem|\
    vendor/pingora-core/examples/keys/client-ca/key.pem|\
    vendor/pingora-core/examples/keys/clients/invalid-key.pem|\
    vendor/pingora-core/examples/keys/clients/key-1.pem|\
    vendor/pingora-core/examples/keys/clients/key-2.pem|\
    vendor/pingora-core/examples/keys/server/key.pem)
      # These deterministic test fixtures shipped in v0.4.1; keep the allowlist exact.
      ;;
    target|target/*|*/target|*/target/*|node_modules|node_modules/*|*/node_modules|*/node_modules/*)
      printf 'release archive contains a build artifact or secret-shaped path: %s\n' "${entry}" >&2
      exit 1
      ;;
    remotion/out|remotion/out/*|*/remotion/out|*/remotion/out/*|test-results|test-results/*|*/test-results|*/test-results/*)
      printf 'release archive contains a build artifact or secret-shaped path: %s\n' "${entry}" >&2
      exit 1
      ;;
    benchmarks/reports|benchmarks/reports/*|*/benchmarks/reports|*/benchmarks/reports/*|.git|.git/*|*/.git|*/.git/*)
      printf 'release archive contains a build artifact or secret-shaped path: %s\n' "${entry}" >&2
      exit 1
      ;;
    *.env|*.token|*credentials*|*.key|*.p12|*.pfx|*/id_rsa|*/id_ed25519)
      printf 'release archive contains a build artifact or secret-shaped path: %s\n' "${entry}" >&2
      exit 1
      ;;
  esac
  case "${relative}" in
    Cargo.lock) has_cargo_lock=true ;;
    Cargo.toml) has_cargo_toml=true ;;
    LICENSE) has_license=true ;;
    ui/pnpm-lock.yaml) has_ui_lock=true ;;
  esac
done <"${sorted_entries}"

tar \
  --extract \
  --gzip \
  --file="${archive}" \
  --to-command='if [ "${TAR_FILETYPE:-}" = f ] && LC_ALL=C grep -aE -- "-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----" >/dev/null; then printf "%s\n" "${TAR_FILENAME}"; fi; exit 0' \
  >"${private_key_entries}"
while IFS= read -r entry; do
  case "${entry}" in
    "${root}"/*) relative=${entry#"${root}/"} ;;
    *)
      printf 'private-key scan returned an entry outside %s/: %s\n' "${root}" "${entry}" >&2
      exit 1
      ;;
  esac
  case "${relative}" in
    crates/oxiroute-import/tests/fixtures/haproxy/tls-chain.pem.key|\
    crates/oxiroute-import/tests/fixtures/haproxy/tls-no-identities.pem.key|\
    crates/oxiroute-import/tests/fixtures/nginx/proxy-key.pem|\
    crates/oxiroute-import/tests/fixtures/nginx/proxy-mismatched-key.pem|\
    crates/oxiroute-server/src/tls/tests.rs|\
    crates/oxiroute-server/tests/fixtures/origin-key.pem|\
    crates/oxiroute-server/tests/fixtures/proxy-a-key.pem|\
    crates/oxiroute-server/tests/fixtures/proxy-b-key.pem|\
    vendor/pingora-core/examples/keys/client-ca/key.pem|\
    vendor/pingora-core/examples/keys/clients/invalid-key.pem|\
    vendor/pingora-core/examples/keys/clients/key-1.pem|\
    vendor/pingora-core/examples/keys/clients/key-2.pem|\
    vendor/pingora-core/examples/keys/server/key.pem)
      ;;
    *)
      printf 'release archive contains unallowlisted private-key material: %s\n' "${entry}" >&2
      exit 1
      ;;
  esac
done <"${private_key_entries}"

${has_cargo_lock} || { printf 'archive is missing Cargo.lock\n' >&2; exit 1; }
${has_cargo_toml} || { printf 'archive is missing Cargo.toml\n' >&2; exit 1; }
${has_license} || { printf 'archive is missing LICENSE\n' >&2; exit 1; }
${has_ui_lock} || { printf 'archive is missing ui/pnpm-lock.yaml\n' >&2; exit 1; }

if [[ "${compare_worktree}" == true ]]; then
  while IFS= read -r -d '' path; do
    printf '%s/%s\n' "${root}" "${path}"
  done < <(
    git -C "${repo_dir}" ls-files -z -- \
      . \
      ':(exclude)packaging/arch/**' \
      ':(exclude)benchmarks/reports/**' \
      ':(exclude)target/**' \
      ':(exclude)**/target/**' \
      ':(exclude)node_modules/**' \
      ':(exclude)**/node_modules/**' \
      ':(exclude)remotion/out/**' \
      ':(exclude)test-results/**'
  ) >"${expected_entries}"
  LC_ALL=C sort "${expected_entries}" >"${sorted_expected}"
  if ! diff -u "${sorted_expected}" "${sorted_entries}"; then
    printf 'release archive file list does not match Git-tracked release inputs\n' >&2
    exit 1
  fi
fi

actual_sha256=$(sha256sum -- "${archive}")
actual_sha256=${actual_sha256%% *}
if [[ -n "${expected_sha256}" && "${actual_sha256}" != "${expected_sha256}" ]]; then
  printf 'release archive checksum mismatch\nexpected: %s\nactual:   %s\n' \
    "${expected_sha256}" "${actual_sha256}" >&2
  exit 1
fi

printf 'verified release archive %s (%s)\n' "${archive}" "${actual_sha256}"
