#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
verify=${repo_dir}/scripts/verify-file-size.sh
root=$(mktemp -d)
work=${root}/work
trap 'rm -rf -- "${root}"' EXIT

die() { printf 'FAILED: %s\n' "$*" >&2; exit 1; }

new_repo() {
  rm -rf -- "${work}"
  mkdir -p "${work}/scripts"
  git -C "${work}" init -q
  : >"${work}/scripts/file-size-baseline.txt"
}

source_file() {
  mkdir -p "$(dirname -- "${work}/$1")"
  seq "$2" >"${work}/$1"
  git -C "${work}" add -- "$1"
}

baseline() { printf '%s\n' "$@" >"${work}/scripts/file-size-baseline.txt"; }

expect_accepted() {
  "${verify}" "${work}" >/dev/null || die "rejected: $1"
}

expect_rejected() {
  local output
  ! output=$("${verify}" "${work}" 2>&1) || die "accepted: $1"
  grep -qF -- "$2" <<<"${output}" || die "rejection of $1 does not mention '$2'"
  grep -qF -- 'Split each oversized file into modules or subfolders' <<<"${output}" || die "no split guidance for $1"
}

new_repo
source_file src/at_limit.rs 1000
source_file src/notes.txt 5000
source_file vendor/crate/big.rs 5000
source_file fuzz/corpus/big.py 5000
source_file ui/src/api/generated/types.ts 5000
source_file crates/oxiroute-server/src/observability/stats_page/assets/table.rs 5000
expect_accepted 'files within the limit and excluded paths'

source_file src/old.rs 1200
baseline '1200 src/old.rs'
expect_accepted 'a baselined oversized file at its recorded count'

source_file src/old.rs 1100
expect_accepted 'a baselined file that shrank'

new_repo
source_file src/new.rs 1001
expect_rejected 'a new offender' 'src/new.rs: 1001 lines exceeds the 1000 line limit'

new_repo
source_file src/old.rs 1300
baseline '1200 src/old.rs'
expect_rejected 'a baselined file that grew' 'src/old.rs: grew from 1200 to 1300 lines'

new_repo
source_file src/old.rs 1000
baseline '1200 src/old.rs'
expect_rejected 'a stale baseline entry for a file within the limit' 'src/old.rs: stale baseline entry'

new_repo
baseline '1200 src/gone.rs'
expect_rejected 'a baseline entry for a missing file' 'src/gone.rs: stale baseline entry'

printf 'verify-file-size tests passed\n'
