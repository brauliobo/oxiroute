#!/usr/bin/env bash
# Prints true when a file changed between BASE and HEAD that a package depends on (recipe_paths_re of families.sh),
# false otherwise: the pull request and branch runs of the release workflow build packages only then.
#
#   changed.sh BASE
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source "${here}/families.sh"
source "${here}/runtime.sh"

[[ $# == 1 ]] || { echo "usage: $0 BASE" >&2; exit 2; }
if git -C "${package_repo}" diff --name-only "$1" HEAD | grep -Eq "${recipe_paths_re}"; then echo true; else echo false; fi
