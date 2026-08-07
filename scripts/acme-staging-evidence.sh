#!/usr/bin/env bash
set -euo pipefail

required_env=(
  OXIROUTE_ACME_STAGING_CONFIG
  OXIROUTE_ACME_STAGING_CERTIFICATE
  OXIROUTE_ACME_STAGING_DOMAIN
  OXIROUTE_ACME_STAGING_DIRECTORY_URL
  OXIROUTE_ACME_STAGING_MANAGEMENT_ENDPOINT
  OXIROUTE_ACME_STAGING_TOKEN_FILE
  OXIROUTE_ACME_STAGING_TLS_ADDRESS
  OXIROUTE_ACME_STAGING_CA_FILE
)

for name in "${required_env[@]}"; do
  if [[ -z "${!name:-}" ]]; then
    printf 'missing required environment variable: %s\n' "${name}" >&2
    exit 2
  fi
done

config=${OXIROUTE_ACME_STAGING_CONFIG}
certificate=${OXIROUTE_ACME_STAGING_CERTIFICATE}
domain=${OXIROUTE_ACME_STAGING_DOMAIN}
directory_url=${OXIROUTE_ACME_STAGING_DIRECTORY_URL}
management_endpoint=${OXIROUTE_ACME_STAGING_MANAGEMENT_ENDPOINT%/}
token_file=${OXIROUTE_ACME_STAGING_TOKEN_FILE}
tls_address=${OXIROUTE_ACME_STAGING_TLS_ADDRESS}
ca_file=${OXIROUTE_ACME_STAGING_CA_FILE}
mode=${OXIROUTE_ACME_STAGING_MODE:-issuance}

case "${directory_url}" in
  https://acme-staging-v02.api.letsencrypt.org/directory) ;;
  *)
    printf 'refusing non-LetsEncrypt-staging directory URL\n' >&2
    exit 2
    ;;
esac

case "${management_endpoint}" in
  http://127.0.0.1:*|http://\[::1\]:*) ;;
  *)
    printf 'management endpoint must be loopback HTTP\n' >&2
    exit 2
    ;;
esac

case "${mode}" in
  issuance|renewal) ;;
  *)
    printf 'OXIROUTE_ACME_STAGING_MODE must be issuance or renewal\n' >&2
    exit 2
    ;;
esac

for path in "${config}" "${token_file}" "${ca_file}"; do
  [[ -f "${path}" ]] || {
    printf 'required staging file is not a regular file\n' >&2
    exit 2
  }
done

for command in curl jq openssl; do
  command -v "${command}" >/dev/null 2>&1 || {
    printf 'required staging command is unavailable: %s\n' "${command}" >&2
    exit 2
  }
done

umask 077
auth_headers=$(mktemp)
generation_json=$(mktemp)
inventory_before_json=$(mktemp)
renewal_json=$(mktemp)
inventory_after_json=$(mktemp)
certificate_evidence=$(mktemp)
certificate_error=$(mktemp)
server_log=$(mktemp)
server_pid=
trap '
  if [[ -n "${server_pid}" ]] && kill -0 "${server_pid}" 2>/dev/null; then
    kill -TERM "${server_pid}" 2>/dev/null || true
    wait "${server_pid}" 2>/dev/null || true
  fi
  rm -f -- "${auth_headers}" "${generation_json}" "${inventory_before_json}" \
    "${renewal_json}" "${inventory_after_json}" "${certificate_evidence}" \
    "${certificate_error}" "${server_log}"
' EXIT

token=$(tr -d '\r\n' <"${token_file}")
[[ -n "${token}" ]] || {
  printf 'staging management token file is empty\n' >&2
  exit 2
}
printf 'Authorization: Bearer %s\n' "${token}" >"${auth_headers}"
unset token

api() {
  curl \
    --silent \
    --show-error \
    --fail \
    --connect-timeout 5 \
    --max-time 30 \
    --header "@${auth_headers}" \
    "$@"
}

server_command=()
if [[ -n "${OXIROUTE_ACME_STAGING_BIN:-}" ]]; then
  server_command=("${OXIROUTE_ACME_STAGING_BIN}" serve "${config}")
else
  command -v cargo >/dev/null 2>&1 || {
    printf 'cargo is required when OXIROUTE_ACME_STAGING_BIN is unset\n' >&2
    exit 2
  }
  repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
  server_command=(cargo run --locked --manifest-path "${repo_dir}/Cargo.toml" -p oxiroute -- serve "${config}")
fi

OXIROUTE_MANAGEMENT_TOKEN_FILE=${token_file} "${server_command[@]}" >"${server_log}" 2>&1 &
server_pid=$!

ready=false
for _ in $(seq 1 60); do
  if ! kill -0 "${server_pid}" 2>/dev/null; then
    break
  fi
  if api "${management_endpoint}/api/v1/generations" >"${generation_json}" 2>/dev/null; then
    ready=true
    break
  fi
  sleep 1
done
if [[ "${ready}" != true ]]; then
  printf 'OxiRoute staging process did not expose the management API\n' >&2
  exit 1
fi

api "${management_endpoint}/api/v1/tls" >"${inventory_before_json}"
source=$(jq -er --arg name "${certificate}" \
  '.certificates[] | select(.name == $name) | .source' "${inventory_before_json}")
[[ "${source}" == acme_managed ]] || {
  printf 'configured staging certificate is not managed ACME\n' >&2
  exit 1
}
configured_directory=$(jq -er --arg name "${certificate}" \
  '.certificates[] | select(.name == $name) | .status.directoryUrl' "${inventory_before_json}")
[[ "${configured_directory}" == "${directory_url}" ]] || {
  printf 'configured managed certificate directory is not the LetsEncrypt staging directory\n' >&2
  exit 1
}
before_revision=$(jq -er --arg name "${certificate}" \
  '.certificates[] | select(.name == $name) | .status.activeRevision' "${inventory_before_json}")
case "${mode}:${before_revision}" in
  issuance:bootstrap) ;;
  issuance:*)
    printf 'issuance mode requires the managed certificate to be at bootstrap\n' >&2
    exit 1
    ;;
  renewal:bootstrap)
    printf 'renewal mode requires an existing staged certificate\n' >&2
    exit 1
    ;;
  renewal:*) ;;
esac

active_config_revision=$(jq -er '.generation.activeRevision' "${generation_json}")
request=$(jq -cn \
  --arg revision "${active_config_revision}" \
  --arg certificate "${certificate}" \
  '{expectedActiveRevision: $revision, certificate: $certificate}')
api \
  --request POST \
  --header 'Content-Type: application/json' \
  --data "${request}" \
  "${management_endpoint}/api/v1/tls/renew" >"${renewal_json}"

outcome=$(jq -er '.outcome' "${renewal_json}")
[[ "${outcome}" == activated || "${outcome}" == unchanged ]] || {
  printf 'staging renewal returned an unexpected outcome\n' >&2
  exit 1
}

api "${management_endpoint}/api/v1/tls" >"${inventory_after_json}"
after_revision=$(jq -er --arg name "${certificate}" \
  '.certificates[] | select(.name == $name) | .status.activeRevision' "${inventory_after_json}")
disk_revision=$(jq -er --arg name "${certificate}" \
  '.certificates[] | select(.name == $name) | .status.diskRevision' "${inventory_after_json}")
[[ "${after_revision}" == "${disk_revision}" && "${after_revision}" != bootstrap ]] || {
  printf 'staging activation did not converge disk and active revisions\n' >&2
  exit 1
}
if [[ "${mode}" == renewal && "${after_revision}" == "${before_revision}" ]]; then
  printf 'staging renewal did not publish a new active revision\n' >&2
  exit 1
fi

openssl s_client \
  -connect "${tls_address}" \
  -servername "${domain}" \
  -alpn http/1.1 \
  -verify_return_error \
  -CAfile "${ca_file}" \
  </dev/null 2>"${certificate_error}" |
  openssl x509 -noout -issuer -subject -serial -fingerprint -sha256 >"${certificate_evidence}"

certificate_summary=$(tr '\n' ';' <"${certificate_evidence}")
printf 'verified real CA-staging %s for %s: revision %s; %s\n' \
  "${mode}" "${domain}" "${after_revision}" "${certificate_summary%?}"
