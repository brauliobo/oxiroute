#!/usr/bin/env bash
# Wall time of the named stages of a release build. Sourced: `timed NAME COMMAND...` runs the command, prints
# "stage-time NAME SECONDS" and returns the command's status; with STAGE_TIMES set the same line is appended to that
# file, which the release workflow publishes as the step summary, so a slow stage is visible without reading the log.

timed() {
  local name=$1 start=${SECONDS}
  shift
  "$@"
  local status=$?
  local line="stage-time ${name} $((SECONDS - start))"
  echo "${line}" >&2
  [[ -z ${STAGE_TIMES:-} ]] || echo "${line}" >> "${STAGE_TIMES}"
  return "${status}"
}
