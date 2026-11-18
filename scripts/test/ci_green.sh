#!/usr/bin/env bash
# Tests of scripts/ci/ci_green.sh against a stub `gh` that answers each poll with the JSON of one scripted list of runs.
set -euo pipefail

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
gate=${repo}/scripts/ci/ci_green.sh
root=$(mktemp -d)
trap 'rm -rf -- "${root}"' EXIT
mkdir -p "${root}/bin"
cat > "${root}/bin/gh" <<'STUB'
#!/usr/bin/env bash
poll=$(($(cat "${STUB_DIR}/polls" 2> /dev/null || echo 0) + 1))
printf '%s' "${poll}" > "${STUB_DIR}/polls"
answer=${STUB_DIR}/poll-${poll}
[[ -f ${answer} ]] || answer=$(printf '%s\n' "${STUB_DIR}"/poll-* | sort -V | tail -1)
while (($# > 0)); do
  [[ $1 != --jq ]] || { jq -r "$2" "${answer}"; exit; }
  shift
done
STUB
chmod +x "${root}/bin/gh"

die() { printf 'FAILED: %s\n' "$*" >&2; exit 1; }

polls() {
  rm -rf -- "${root}/stub"
  mkdir -p "${root}/stub"
  local index=0 answer
  for answer in "$@"; do
    index=$((index + 1))
    printf '[%s]' "${answer}" > "${root}/stub/poll-${index}"
  done
}

run() { jq -nc --arg status "$1" --arg conclusion "$2" --arg event "${3:-push}" '{status: $status, conclusion: $conclusion, event: $event}'; }

gate_run() {
  STUB_DIR=${root}/stub PATH=${root}/bin:${PATH} OXIROUTE_CI_POLL=0 env "$@" "${gate}" 0123abc > "${root}/out" 2>&1
}

passes() { gate_run OXIROUTE_CI_WAIT=1000 OXIROUTE_CI_GRACE=1000 || die "refused ${what}: $(cat "${root}/out")"; }

refuses() {
  ! gate_run "$@" || die "accepted ${what}"
  grep -qF -- "${expect}" "${root}/out" || die "refusal of ${what} does not say '${expect}': $(cat "${root}/out")"
}

what='a green push run'
polls "$(run completed success)"
passes

what='a green manual run'
polls "$(run completed success workflow_dispatch)"
passes

what='a green rerun beside a failed run'
polls "$(run completed failure),$(run completed success)"
passes

what='a run that is still going and then passes'
polls "$(run in_progress ''),$(run completed failure)" "$(run in_progress '')" "$(run completed success)"
passes

what='a commit whose run starts a moment later'
polls '' '' "$(run queued '')"  "$(run completed success)"
passes

what='a failed run'; expect='did not pass'
polls "$(run completed failure)"
refuses OXIROUTE_CI_WAIT=1000

what='a cancelled run'; expect='did not pass'
polls "$(run completed cancelled),$(run completed failure)"
refuses OXIROUTE_CI_WAIT=1000

what='a green pull request run only'; expect='no ci.yml run'
polls "$(run completed success pull_request)"
refuses OXIROUTE_CI_WAIT=1000 OXIROUTE_CI_GRACE=0

what='a commit that no run saw'; expect='no ci.yml run'
polls ''
refuses OXIROUTE_CI_WAIT=1000 OXIROUTE_CI_GRACE=0

what='a run that never finishes'; expect='still running'
polls "$(run in_progress '')"
refuses OXIROUTE_CI_WAIT=0

printf 'ci_green tests passed\n'
