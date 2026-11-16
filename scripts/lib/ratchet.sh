#!/usr/bin/env bash
# Sourced by the scripts/verify checks. A check fills `current` (key -> violation count), sets ratchet_unit,
# ratchet_rule, ratchet_guidance and ratchet_ok, then calls `ratchet_run BASELINE`. The baseline lists "COUNT KEY" per
# line and only shrinks: a new key, a key above its recorded count and a recorded key that no longer violates all fail.

declare -A current recorded
ratchet_status=0

ratchet_fail() { printf '%s\n' "$1" >&2; ratchet_status=1; }

ratchet_keys() {
  local -n map=$1
  printf '%s\n' "${!map[@]}" | sed '/^$/d' | LC_ALL=C sort
}

ratchet_load() {
  local count key
  while read -r count key; do
    [[ -z ${key} ]] || recorded[${key}]=${count}
  done <"$1"
}

ratchet_check_current() {
  local key
  while read -r key; do
    if [[ -z ${recorded[${key}]:-} ]]; then
      ratchet_fail "${key}: ${current[${key}]} ${ratchet_unit} ${ratchet_rule} and is not in the baseline"
    elif (( current[${key}] > recorded[${key}] )); then
      ratchet_fail "${key}: grew from ${recorded[${key}]} to ${current[${key}]} ${ratchet_unit}"
    fi
  done < <(ratchet_keys current)
}

ratchet_check_recorded() {
  local key
  while read -r key; do
    [[ -n ${current[${key}]:-} ]] || ratchet_fail "${key}: stale baseline entry, no longer a violation"
  done < <(ratchet_keys recorded)
}

ratchet_run() {
  ratchet_load "$1"
  ratchet_check_current
  ratchet_check_recorded
  if (( ratchet_status != 0 )); then
    printf '%s\n' "${ratchet_guidance}" >&2
    exit "${ratchet_status}"
  fi
  printf '%s\n' "${ratchet_ok}"
}
