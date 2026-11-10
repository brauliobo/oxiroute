#!/usr/bin/env bash
# Fails when a release binary embeds a builder path (home directories, CI runner workspaces, the checkout, the cargo
# home): such strings leak the build host and break reproducibility.
#
#   verify-binary-paths.sh BINARY [EXTRA_FORBIDDEN_STRING...]
set -euo pipefail

binary=${1:-}
[[ -n ${binary} && ${binary} != -* && -f ${binary} ]] || { echo "usage: $0 BINARY [FORBIDDEN...]" >&2; exit 2; }
shift
forbidden=(/home/ /runner/work "$@")

status=0
for needle in "${forbidden[@]}"; do
  [[ -n ${needle} ]] || continue
  if grep -aqF -- "${needle}" "${binary}"; then
    printf '%s embeds the build path %s, for example:\n' "${binary}" "${needle}" >&2
    grep -aoE -- "[[:print:]]{0,40}$(printf '%s' "${needle}" | sed 's/[][\.*^$]/\\&/g')[[:print:]]{0,80}" "${binary}" | sort -u | sed -n 1,3p >&2
    status=1
  fi
done
exit "${status}"
