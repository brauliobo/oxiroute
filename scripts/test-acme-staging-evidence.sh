#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
harness=${script_dir}/acme-staging-evidence.sh
temporary_directory=$(mktemp -d)
trap 'rm -rf -- "${temporary_directory}"' EXIT

assert_contains() {
  local haystack=$1
  local needle=$2
  [[ "$haystack" == *"$needle"* ]] || {
    printf 'expected harness output to contain: %s\n' "$needle" >&2
    exit 1
  }
}

assert_not_contains() {
  local haystack=$1
  local needle=$2
  [[ "$haystack" != *"$needle"* ]] || {
    printf 'expected harness output not to contain: %s\n' "$needle" >&2
    exit 1
  }
}

run_preflight() {
  local output status
  if output=$(env -i PATH="$PATH" HOME="${HOME:-/tmp}" "$@" 2>&1); then
    status=0
  else
    status=$?
  fi
  printf '%s\n%s\n' "$status" "$output"
}

missing_result=$(run_preflight bash "$harness" --preflight)
missing_status=${missing_result%%$'\n'*}
missing_output=${missing_result#*$'\n'}
[[ "$missing_status" == 2 ]] || {
  printf 'missing prerequisite preflight returned %s, expected 2\n' "$missing_status" >&2
  exit 1
}
for name in \
  OXIROUTE_ACME_STAGING_CONFIG \
  OXIROUTE_ACME_STAGING_CERTIFICATE \
  OXIROUTE_ACME_STAGING_DOMAIN \
  OXIROUTE_ACME_STAGING_DIRECTORY_URL \
  OXIROUTE_ACME_STAGING_MANAGEMENT_ENDPOINT \
  OXIROUTE_ACME_STAGING_TOKEN_FILE \
  OXIROUTE_ACME_STAGING_TLS_ADDRESS \
  OXIROUTE_ACME_STAGING_CA_FILE; do
  assert_contains "$missing_output" "missing required environment variable: ${name}"
done
assert_not_contains "$missing_output" 'verified real CA-staging'

config_file=${temporary_directory}/config.kdl
token_file=${temporary_directory}/management.token
ca_file=${temporary_directory}/ca.pem
marker_file=${temporary_directory}/server-invoked
server_binary=${temporary_directory}/server
printf 'placeholder config\n' >"$config_file"
printf '%032d\n' 7 >"$token_file"
chmod 644 "$token_file"
printf 'not a certificate\n' >"$ca_file"
printf '#!/usr/bin/env bash\nprintf invoked >"%s"\n' "$marker_file" >"$server_binary"
chmod 700 "$server_binary"

invalid_result=$(run_preflight \
  OXIROUTE_ACME_STAGING_CONFIG="$config_file" \
  OXIROUTE_ACME_STAGING_CERTIFICATE=managed-staging \
  OXIROUTE_ACME_STAGING_DOMAIN=example.test \
  OXIROUTE_ACME_STAGING_DIRECTORY_URL=https://example.invalid/directory \
  OXIROUTE_ACME_STAGING_MANAGEMENT_ENDPOINT=http://example.invalid:9080 \
  OXIROUTE_ACME_STAGING_TOKEN_FILE="$token_file" \
  OXIROUTE_ACME_STAGING_TLS_ADDRESS=example.test:443 \
  OXIROUTE_ACME_STAGING_CA_FILE="$ca_file" \
  OXIROUTE_ACME_STAGING_MODE=invalid \
  OXIROUTE_ACME_STAGING_BIN="$server_binary" \
  bash "$harness" --preflight)
invalid_status=${invalid_result%%$'\n'*}
invalid_output=${invalid_result#*$'\n'}
[[ "$invalid_status" == 2 ]] || {
  printf 'invalid input preflight returned %s, expected 2\n' "$invalid_status" >&2
  exit 1
}
assert_contains "$invalid_output" 'refusing non-LetsEncrypt-staging directory URL'
assert_contains "$invalid_output" 'OXIROUTE_ACME_STAGING_MODE must be issuance or renewal'
assert_contains "$invalid_output" 'staging domain is invalid or a reserved placeholder'
assert_contains "$invalid_output" 'management endpoint must be loopback HTTP with a numeric port'
assert_contains "$invalid_output" 'management token file mode must be 0400 or 0600'
assert_contains "$invalid_output" 'CA trust file is not a parseable PEM bundle'
assert_not_contains "$invalid_output" 'example.test'
[[ ! -e "$marker_file" ]] || {
  printf 'preflight unexpectedly invoked the configured server binary\n' >&2
  exit 1
}

printf 'ACME staging harness validation tests passed\n'
