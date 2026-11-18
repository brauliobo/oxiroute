#!/usr/bin/env bash
# Tests of scripts/verify/rust_cache_keys.sh on fixture workflows and of the CPU identity that keys the native lane
# (scripts/lib/cpu_id.sh), then the verifier on the workflows of this repository.
set -euo pipefail

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
verify=${repo}/scripts/verify/rust_cache_keys.sh
cpu_id=${repo}/scripts/lib/cpu_id.sh
root=$(mktemp -d)
trap 'rm -rf -- "${root}"' EXIT

die() { printf 'FAILED: %s\n' "$*" >&2; exit 1; }

workflow() {
  rm -rf -- "${root}/repo"
  mkdir -p "${root}/repo/.github/workflows"
  cat >"${root}/repo/.github/workflows/ci.yml"
}

accepted() { "${verify}" "${root}/repo" >/dev/null 2>&1 || die "rejected: $1"; }

rejected() {
  local output
  ! output=$("${verify}" "${root}/repo" 2>&1) || die "accepted: $1"
  grep -qF -- "$2" <<<"${output}" || die "rejection of $1 does not mention '$2'"
  grep -qF -- 'Fix the cache key itself' <<<"${output}" || die "no fixer guidance for $1"
}

workflow <<'YAML'
on: push
jobs:
  lint:
    steps:
      - uses: Swatinem/rust-cache@v2
        with:
          shared-key: lint
  native:
    steps:
      - id: cpu
        run: echo "id=$(./scripts/lib/cpu_id.sh)" >> "$GITHUB_OUTPUT"
      - uses: Swatinem/rust-cache@v2
        with:
          key: ${{ steps.cpu.outputs.id }}
      - run: ./scripts/test/cpu-targets.sh
YAML
accepted 'a shared key, and a native lane keyed by the runner CPU'

workflow <<'YAML'
on: push
jobs:
  native:
    steps:
      - run: ./scripts/test/cpu-targets.sh
YAML
accepted 'a native lane that caches nothing'

workflow <<'YAML'
on: push
jobs:
  native:
    steps:
      - id: cpu
        run: echo "id=$(./scripts/lib/cpu_id.sh)" >> "$GITHUB_OUTPUT"
      - uses: Swatinem/rust-cache@v2
        with:
          shared-key: cpu-targets
          key: ${{ steps.cpu.outputs.id }}
      - run: ./scripts/test/cpu-targets.sh
YAML
rejected 'a key beside a shared key' 'ci.yml: job native: key is ignored when shared-key is set'

workflow <<'YAML'
on: push
jobs:
  native:
    steps:
      - uses: Swatinem/rust-cache@v2
        with:
          shared-key: cpu-targets
      - run: ./scripts/test/cpu-targets.sh
YAML
rejected 'a native lane without the CPU in its key' 'ci.yml: job native: a native-CPU lane caches under a key'

workflow <<'YAML'
on: push
jobs:
  native:
    steps:
      - id: toolchain
        run: rustc --version
      - uses: Swatinem/rust-cache@v2
        with:
          key: ${{ steps.toolchain.outputs.id }}
      - run: ./scripts/test/cpu-targets.sh
YAML
rejected 'a native lane keyed by a step that is not the CPU identity' 'a native-CPU lane caches under a key'

cpuinfo() { printf 'processor\t: 0\nmodel name\t: %s\nflags\t\t: %s\nprocessor\t: 1\nmodel name\t: other\nflags\t\t: y\n' "$@" >"${root}/cpuinfo"; }

identity() { cpuinfo "$@"; CPUINFO=${root}/cpuinfo "${cpu_id}"; }

[[ $(identity 'Model A' 'sse2 avx2') == "$(identity 'Model A' 'sse2 avx2')" ]] || die 'the identity of one CPU changes'
[[ $(identity 'Model A' 'sse2 avx2') != "$(identity 'Model B' 'sse2 avx2')" ]] || die 'two CPU models share an identity'
[[ $(identity 'Model A' 'sse2 avx2') != "$(identity 'Model A' 'sse2 avx2 avx512f')" ]] || die 'two feature sets share an identity'
[[ $(identity 'Model A' 'sse2 avx2') =~ ^[0-9a-f]{16}$ ]] || die 'the identity is not 16 hex digits'

"${verify}" "${repo}" >/dev/null || die "the workflows of this repository are rejected"

printf 'rust_cache_keys tests passed\n'
