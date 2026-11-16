#!/usr/bin/env bash
# Sourced by the scripts/test checks of scripts/verify: a temporary git repository (`work`, inside the caller's `root`)
# that the check named by `verify` runs against, `baseline_file` is its baseline path inside the repository and
# `guidance` is the fixer advice every rejection must carry.

die() { printf 'FAILED: %s\n' "$*" >&2; exit 1; }

new_repo() {
  rm -rf -- "${work}"
  mkdir -p "${work}/scripts/verify"
  git -C "${work}" init -q
  : >"${work}/${baseline_file}"
}

track() {
  mkdir -p "$(dirname -- "${work}/$1")"
  if [[ -n ${2:-} ]]; then seq "$2" >"${work}/$1"; else : >"${work}/$1"; fi
  git -C "${work}" add -- "$1"
}

untrack() { git -C "${work}" rm -q -f -- "$1"; }

baseline() { printf '%s\n' "$@" >"${work}/${baseline_file}"; }

expect_accepted() {
  "${verify}" "${work}" >/dev/null || die "rejected: $1"
}

expect_rejected() {
  local output
  ! output=$("${verify}" "${work}" 2>&1) || die "accepted: $1"
  grep -qF -- "$2" <<<"${output}" || die "rejection of $1 does not mention '$2'"
  grep -qF -- "${guidance}" <<<"${output}" || die "no fixer guidance for $1"
}
