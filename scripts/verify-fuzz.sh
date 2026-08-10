#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=/dev/null
source "${repo_dir}/fuzz/targets.sh"

failures=0

fail() {
  printf 'fuzz contract: %s\n' "$1" >&2
  failures=$((failures + 1))
}

declare -A expected_targets=()
for spec in "${FUZZ_TARGET_SPECS[@]}"; do
  IFS=: read -r target maximum <<<"${spec}"
  if [[ -n "${expected_targets[${target}]+present}" ]]; then
    fail "duplicate target contract: ${target}"
  fi
  if [[ ! "${maximum}" =~ ^[1-9][0-9]*$ ]]; then
    fail "invalid input bound for ${target}: ${maximum}"
  fi
  expected_targets["${target}"]="${maximum}"
done

declare -A manifest_targets=()
in_bin=false
bin_name=
bin_path=

record_manifest_bin() {
  if [[ "${in_bin}" != true ]]; then
    return
  fi
  if [[ -z "${bin_name}" || -z "${bin_path}" ]]; then
    fail 'fuzz/Cargo.toml contains an incomplete [[bin]] entry'
  else
    if [[ -n "${manifest_targets[${bin_name}]+present}" ]]; then
      fail "duplicate [[bin]] entry: ${bin_name}"
    fi
    manifest_targets["${bin_name}"]="${bin_path}"
  fi
}

while IFS= read -r line || [[ -n "${line}" ]]; do
  if [[ "${line}" == '[[bin]]' ]]; then
    record_manifest_bin
    in_bin=true
    bin_name=
    bin_path=
  elif [[ "${line}" == '[['* ]]; then
    record_manifest_bin
    in_bin=false
    bin_name=
    bin_path=
  elif [[ "${in_bin}" == true && "${line}" == 'name = "'* ]]; then
    bin_name=${line#name = \"}
    bin_name=${bin_name%\"}
  elif [[ "${in_bin}" == true && "${line}" == 'path = "'* ]]; then
    bin_path=${line#path = \"}
    bin_path=${bin_path%\"}
  fi
done <"${repo_dir}/fuzz/Cargo.toml"
record_manifest_bin

for target in "${!expected_targets[@]}"; do
  if [[ -z "${manifest_targets[${target}]+present}" ]]; then
    fail "target ${target} is missing from fuzz/Cargo.toml"
  elif [[ "${manifest_targets[${target}]}" != "fuzz_targets/${target}.rs" ]]; then
    fail "target ${target} does not use fuzz_targets/${target}.rs"
  fi
done

command_definitions=$(LC_ALL=C grep -Fc '    command=(' "${repo_dir}/fuzz/campaign.sh" || true)
command_uses=$(LC_ALL=C grep -Fc '"${command[@]}"' "${repo_dir}/fuzz/campaign.sh" || true)
if [[ "${command_definitions}" != 1 || "${command_uses}" != 2 ]]; then
  fail 'campaign must log and execute one shared command array'
fi

for target in "${!expected_targets[@]}"; do
  source_path="${repo_dir}/fuzz/fuzz_targets/${target}.rs"
  mapfile -t limit_lines < <(LC_ALL=C grep -E '^const MAX_INPUT_BYTES: usize = [0-9][0-9_]*;$' "${source_path}")
  if ((${#limit_lines[@]} != 1)); then
    fail "target ${target} must declare one literal MAX_INPUT_BYTES"
    continue
  fi
  if [[ "${limit_lines[0]}" =~ ^const\ MAX_INPUT_BYTES:\ usize\ =\ ([0-9][0-9_]*)\;$ ]]; then
    source_limit=${BASH_REMATCH[1]//_/}
    if [[ "${source_limit}" != "${expected_targets[${target}]}" ]]; then
      fail "target ${target} declares ${source_limit} bytes, expected ${expected_targets[${target}]}"
    fi
  else
    fail "target ${target} has an unreadable MAX_INPUT_BYTES declaration"
  fi
done
for target in "${!manifest_targets[@]}"; do
  if [[ -z "${expected_targets[${target}]+present}" ]]; then
    fail "fuzz/Cargo.toml has an uncontracted target: ${target}"
  fi
done

declare -A source_targets=()
shopt -s nullglob
sources=("${repo_dir}"/fuzz/fuzz_targets/*.rs)
shopt -u nullglob
for source in "${sources[@]}"; do
  filename=${source##*/}
  target=${filename%.rs}
  if [[ -z "${expected_targets[${target}]+present}" ]]; then
    fail "uncontracted fuzz harness source: fuzz/fuzz_targets/${filename}"
  fi
  source_targets["${target}"]="${source}"
done
for target in "${!expected_targets[@]}"; do
  if [[ -z "${source_targets[${target}]+present}" ]]; then
    fail "missing fuzz harness source: fuzz/fuzz_targets/${target}.rs"
  fi
done

corpus_files=0
for spec in "${FUZZ_TARGET_SPECS[@]}"; do
  IFS=: read -r target maximum <<<"${spec}"
  corpus_dir="${repo_dir}/fuzz/corpus/${target}"
  if [[ ! -d "${corpus_dir}" ]]; then
    fail "missing corpus directory: fuzz/corpus/${target}"
    continue
  fi

  shopt -s nullglob
  entries=("${corpus_dir}"/*)
  shopt -u nullglob
  if ((${#entries[@]} == 0)); then
    fail "corpus directory is empty: fuzz/corpus/${target}"
    continue
  fi

  for entry in "${entries[@]}"; do
    name=${entry##*/}
    if [[ ! -f "${entry}" ]]; then
      fail "corpus entry is not a regular file: fuzz/corpus/${target}/${name}"
      continue
    fi
    if [[ -z "${name}" || "${name}" == .* || "${name}" == */* ]]; then
      fail "corpus entry has an invalid name: fuzz/corpus/${target}/${name}"
      continue
    fi
    size=$(LC_ALL=C wc -c <"${entry}")
    if ((size > maximum)); then
      fail "corpus entry exceeds ${maximum} bytes: fuzz/corpus/${target}/${name} (${size})"
    fi

    content=
    IFS= read -r -d '' content <"${entry}" || true
    normalized=${content}
    if [[ "${normalized}" == *$'\n' ]]; then
      normalized=${normalized%$'\n'}
    fi
    if [[ "${normalized}" == *$'\r' ]]; then
      normalized=${normalized%$'\r'}
    fi
    if [[ -z "${normalized}" ]]; then
      fail "corpus entry is empty: fuzz/corpus/${target}/${name}"
    elif [[ "${normalized}" == hex:* ]]; then
      encoded=${normalized#hex:}
      if (( ${#encoded} % 2 != 0 )); then
        fail "hex corpus entry has an odd number of digits: fuzz/corpus/${target}/${name}"
      elif [[ "${encoded}" == *[!0123456789abcdefABCDEF]* ]]; then
        fail "hex corpus entry contains a non-hex byte: fuzz/corpus/${target}/${name}"
      elif (( ${#encoded} / 2 > maximum )); then
        fail "decoded hex corpus entry exceeds ${maximum} bytes: fuzz/corpus/${target}/${name}"
      fi
    elif [[ "${normalized}" == seed:* ]]; then
      case "${target}:${normalized}" in
        rtmp_handshake:seed:simple|\
        rtmp_chunk:seed:chunk|\
        rtmp_amf:seed:amf|\
        udp_datagram:seed:oversized|\
        tls_client_hello:seed:clienthello|\
        http1:seed:oversized-header)
          ;;
        *)
          fail "unknown deterministic seed marker: fuzz/corpus/${target}/${name}"
          ;;
      esac
    fi
    corpus_files=$((corpus_files + 1))
  done
done

if ((failures > 0)); then
  printf 'fuzz contract: %d failure(s)\n' "${failures}" >&2
  exit 1
fi

cargo +1.97.1 fmt --manifest-path "${repo_dir}/fuzz/Cargo.toml" --check
cargo +1.97.1 check --manifest-path "${repo_dir}/fuzz/Cargo.toml" --locked --jobs 4
printf 'verified %d bounded fuzz targets and %d corpus seeds\n' "${#expected_targets[@]}" "${corpus_files}"
