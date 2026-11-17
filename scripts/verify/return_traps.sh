#!/usr/bin/env bash
# Fails when a git-tracked shell script sets a RETURN trap. A RETURN trap outlives the function that set it and fires
# when the next function returns, where the variables it names are out of scope (`set -u` then aborts the script).
#
#   return_traps.sh [REPO_DIR]
set -euo pipefail

root=${1:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)}
found=$(git -C "${root}" grep -nE "^[^#]*\btrap\b[^#]*\bRETURN\b" -- '*.sh' || true)
if [[ -n ${found} ]]; then
  printf '%s\n\nA RETURN trap leaks out of its function. Make the function a subshell, `f() ( ... )`, and clean up with an\nEXIT trap inside it.\n' "${found}" >&2
  exit 1
fi
echo 'verified no shell script sets a RETURN trap'
