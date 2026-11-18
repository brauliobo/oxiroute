#!/usr/bin/env bash
# Succeeds only when ci.yml passed on the exact commit: a push to main or a manual run of that commit, not a pull request,
# which tests a merge with the base. A run that is still going is waited for, and a commit that no run has seen yet is
# given OXIROUTE_CI_GRACE seconds (default 300) to start one; a commit whose runs all failed or were cancelled is refused.
# The release workflow runs its own gates only for what ci.yml does not cover, so a tag on a commit without a green
# ci.yml run must never publish.
#
#   ci_green.sh SHA     needs gh, GH_TOKEN and GITHUB_REPOSITORY; OXIROUTE_CI_WAIT seconds in all (default 3000)
set -euo pipefail

sha=${1:?usage: ci_green.sh SHA}
wait_seconds=${OXIROUTE_CI_WAIT:-3000}
grace_seconds=${OXIROUTE_CI_GRACE:-300}
poll_seconds=${OXIROUTE_CI_POLL:-30}
started=$SECONDS

runs() {
  gh run list --workflow ci.yml --commit "${sha}" --limit 50 --json status,conclusion,event \
    --jq '.[] | select(.event == "push" or .event == "workflow_dispatch") | "\(.status) \(.conclusion)"'
}

while :; do
  seen=$(runs)
  grep -qx 'completed success' <<< "${seen}" && { printf 'ci.yml passed on %s\n' "${sha}"; exit 0; }
  elapsed=$((SECONDS - started))
  if [[ -n ${seen} ]]; then
    grep -qv '^completed ' <<< "${seen}" || { printf 'ci.yml did not pass on %s:\n%s\n' "${sha}" "${seen}" >&2; exit 1; }
    ((elapsed < wait_seconds)) || { printf 'ci.yml is still running on %s after %s s\n' "${sha}" "${elapsed}" >&2; exit 1; }
  else
    ((elapsed < grace_seconds)) || { printf 'no ci.yml run of %s: tag a commit that passed CI\n' "${sha}" >&2; exit 1; }
  fi
  sleep "${poll_seconds}"
done
