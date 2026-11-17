#!/usr/bin/env bash
# Wall time of the named stages of a release build. Sourced: `timed NAME COMMAND...` runs the command, prints
# "stage-time NAME SECONDS" and returns the command's status; with STAGE_TIMES set the same line is appended to that
# file, which the release workflow publishes as the step summary, so a slow stage is visible without reading the log.
# The helper keeps every name it needs in `timed_` variables: bash scoping is dynamic, so a plainer name would shadow
# the variable of the same name in a function it runs.

timed() {
  local timed_label=$1 timed_began=${SECONDS}
  shift
  "$@"
  local timed_status=$?
  local timed_line="stage-time ${timed_label} $((SECONDS - timed_began))"
  echo "${timed_line}" >&2
  [[ -z ${STAGE_TIMES:-} ]] || echo "${timed_line}" >> "${STAGE_TIMES}"
  return "${timed_status}"
}
