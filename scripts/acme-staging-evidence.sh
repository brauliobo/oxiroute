#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

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

usage() {
  printf 'usage: %s [--preflight]\n' "${BASH_SOURCE[0]}" >&2
}

preflight_only=false
case "${1:-}" in
  --preflight)
    preflight_only=true
    shift
    ;;
  '') ;;
  *)
    usage
    exit 2
    ;;
esac
if (($# != 0)); then
  usage
  exit 2
fi

config=${OXIROUTE_ACME_STAGING_CONFIG:-}
certificate=${OXIROUTE_ACME_STAGING_CERTIFICATE:-}
domain=${OXIROUTE_ACME_STAGING_DOMAIN:-}
directory_url=${OXIROUTE_ACME_STAGING_DIRECTORY_URL:-}
management_endpoint=${OXIROUTE_ACME_STAGING_MANAGEMENT_ENDPOINT:-}
management_endpoint=${management_endpoint%/}
token_file=${OXIROUTE_ACME_STAGING_TOKEN_FILE:-}
tls_address=${OXIROUTE_ACME_STAGING_TLS_ADDRESS:-}
ca_file=${OXIROUTE_ACME_STAGING_CA_FILE:-}
mode=${OXIROUTE_ACME_STAGING_MODE:-issuance}

blockers=()

add_blocker() {
  blockers+=("$1")
}

valid_dns_name() {
  local candidate=$1
  local -a labels
  local label

  [[ ${#candidate} -ge 3 && ${#candidate} -le 253 ]] || return 1
  [[ "$candidate" == *.* ]] || return 1
  [[ "$candidate" != *".."* ]] || return 1
  [[ "$candidate" != *"/"* && "$candidate" != *[[:space:]]* ]] || return 1
  IFS='.' read -r -a labels <<<"$candidate"
  ((${#labels[@]} >= 2)) || return 1
  for label in "${labels[@]}"; do
    [[ ${#label} -le 63 ]] || return 1
    [[ "$label" =~ ^[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?$ ]] || return 1
  done
  case "${candidate,,}" in
    *.test|*.invalid|*.localhost|localhost|example|example.*|*.example) return 1 ;;
  esac
}

check_regular_file() {
  local label=$1
  local path=$2
  if [[ -z "$path" ]]; then
    return
  fi
  if [[ ! -f "$path" || ! -r "$path" ]]; then
    add_blocker "${label} is not a readable regular file"
  fi
}

read_management_token() {
  token=''
  if [[ -f "$token_file" && -r "$token_file" ]]; then
    IFS= read -r -d '' token <"$token_file" || true
    case "$token" in
      *$'\r\n') token=${token%$'\r\n'} ;;
      *$'\n') token=${token%$'\n'} ;;
    esac
  fi
}

validate_prerequisites() {
  local name command token_mode token_size staging_status parsed_ca
  local management_re='^http://(127\.0\.0\.1|\[::1\]):([0-9]{1,5})$'
  local tls_re='^(\[[0-9A-Fa-f:]+\]|[A-Za-z0-9.-]+):([0-9]{1,5})$'

  for name in "${required_env[@]}"; do
    if [[ -z "${!name:-}" ]]; then
      add_blocker "missing required environment variable: ${name}"
    fi
  done

  if [[ -n "$directory_url" && "$directory_url" != \
    'https://acme-staging-v02.api.letsencrypt.org/directory' ]]; then
    add_blocker 'refusing non-LetsEncrypt-staging directory URL'
  fi
  if [[ -n "$mode" && "$mode" != issuance && "$mode" != renewal ]]; then
    add_blocker 'OXIROUTE_ACME_STAGING_MODE must be issuance or renewal'
  fi
  if [[ -n "$certificate" && "$certificate" == *[[:space:]]* ]]; then
    add_blocker 'staging certificate name must not contain whitespace'
  fi
  if [[ -n "$domain" ]] && ! valid_dns_name "$domain"; then
    add_blocker 'staging domain is invalid or a reserved placeholder'
  fi
  if [[ -n "$management_endpoint" ]] && ! [[ "$management_endpoint" =~ $management_re ]]; then
    add_blocker 'management endpoint must be loopback HTTP with a numeric port'
  elif [[ "$management_endpoint" =~ $management_re ]]; then
    if ((10#${BASH_REMATCH[2]} < 1 || 10#${BASH_REMATCH[2]} > 65535)); then
      add_blocker 'management endpoint port is outside 1-65535'
    fi
  fi
  if [[ -n "$tls_address" ]] && ! [[ "$tls_address" =~ $tls_re ]]; then
    add_blocker 'TLS address must be a host:port or bracketed IPv6:port endpoint'
  elif [[ "$tls_address" =~ $tls_re ]]; then
    if ((10#${BASH_REMATCH[2]} < 1 || 10#${BASH_REMATCH[2]} > 65535)); then
      add_blocker 'TLS address port is outside 1-65535'
    fi
  fi

  check_regular_file 'staging config' "$config"
  check_regular_file 'management token file' "$token_file"
  check_regular_file 'CA trust file' "$ca_file"

  for command in curl jq openssl; do
    command -v "$command" >/dev/null 2>&1 ||
      add_blocker "required staging command is unavailable: ${command}"
  done
  if [[ -n "${OXIROUTE_ACME_STAGING_BIN:-}" ]]; then
    if [[ "${OXIROUTE_ACME_STAGING_BIN}" == */* ]]; then
      [[ -x "${OXIROUTE_ACME_STAGING_BIN}" ]] ||
        add_blocker 'configured staging server binary is not executable'
    else
      command -v "${OXIROUTE_ACME_STAGING_BIN}" >/dev/null 2>&1 ||
        add_blocker 'configured staging server binary is unavailable'
    fi
  else
    command -v cargo >/dev/null 2>&1 ||
      add_blocker 'cargo is required when OXIROUTE_ACME_STAGING_BIN is unset'
  fi

  if [[ -f "$token_file" && -r "$token_file" ]]; then
    if [[ -L "$token_file" ]]; then
      add_blocker 'management token file must not be a symbolic link'
    elif ! command -v stat >/dev/null 2>&1; then
      add_blocker 'stat is required to validate management token file permissions'
    else
      token_mode=$(stat -c '%a' "$token_file" 2>/dev/null || true)
      [[ "$token_mode" == 400 || "$token_mode" == 600 ]] ||
        add_blocker 'management token file mode must be 0400 or 0600'
    fi
    token_size=$(wc -c <"$token_file")
    if [[ "$token_size" =~ ^[0-9]+$ ]] && ((token_size <= 514)); then
      read_management_token
      if ! [[ "$token" =~ ^[!-~]{32,512}$ ]]; then
        add_blocker 'management token must be 32-512 visible ASCII bytes plus one optional line ending'
      fi
      unset token
    else
      add_blocker 'management token file exceeds the supported size'
    fi
  fi

  if [[ -f "$ca_file" && -r "$ca_file" ]] && command -v openssl >/dev/null 2>&1; then
    parsed_ca=$(openssl crl2pkcs7 -nocrl -certfile "$ca_file" -outform PEM 2>/dev/null |
      openssl pkcs7 -print_certs -noout 2>/dev/null || true)
    [[ -n "$parsed_ca" ]] || add_blocker 'CA trust file is not a parseable PEM bundle'
    unset parsed_ca
  fi

  if [[ -n "$domain" ]] && valid_dns_name "$domain"; then
    if command -v getent >/dev/null 2>&1; then
      getent ahosts "$domain" >/dev/null 2>&1 || add_blocker 'staging domain did not resolve in DNS'
    else
      add_blocker 'getent is required to verify staging domain DNS resolution'
    fi
  fi

  if ((${#blockers[@]} == 0)); then
    staging_status=$(curl --silent --show-error --fail --connect-timeout 5 --max-time 15 \
      --output /dev/null --write-out '%{http_code}' \
      'https://acme-staging-v02.api.letsencrypt.org/directory' 2>/dev/null || true)
    [[ "$staging_status" == 200 ]] ||
      add_blocker 'LetsEncrypt staging directory was not reachable with HTTP 200'
  fi

  if ((${#blockers[@]} > 0)); then
    printf 'staging preflight blocked:\n' >&2
    printf ' - %s\n' "${blockers[@]}" >&2
    return 2
  fi
}

validate_prerequisites
if [[ "$preflight_only" == true ]]; then
  printf 'staging preflight passed; no issuance or renewal attempted\n'
  exit 0
fi

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

read_management_token
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
configured_challenge=$(jq -er --arg name "${certificate}" \
  '.certificates[] | select(.name == $name) | .status.challenge' "${inventory_before_json}")
case "${configured_challenge}" in
  http01|dns01|tls_alpn01) ;;
  *)
    printf 'configured managed certificate has an unsupported ACME challenge\n' >&2
    exit 1
    ;;
esac
if [[ "${configured_challenge}" == tls_alpn01 && "${tls_address}" != *:443 ]]; then
  printf 'TLS-ALPN-01 staging evidence requires a TLS address on port 443\n' >&2
  exit 1
fi
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
  openssl x509 -noout -issuer -serial -fingerprint -sha256 >"${certificate_evidence}"

certificate_summary=$(tr '\n' ';' <"${certificate_evidence}")
printf 'verified real CA-staging %s via %s: revision %s; %s\n' \
  "${mode}" "${configured_challenge}" "${after_revision}" "${certificate_summary%?}"
