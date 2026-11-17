#!/usr/bin/env bash
# The profile-guided-optimisation profile of one architecture and flavour, so the release workflow can reuse a stored
# profile instead of training it in every build (docs/developer/RELEASING.md, "The profile").
#
#   profile.sh key ARCH FLAVOUR            the cache key of the profile: toolchain, target, flavour, lanes and the hot-path sources
#   profile.sh train ARCH FLAVOUR OUT_DIR  train it in the glibc-floor container (instrumented build, lanes, merge): OUT_DIR/oxiroute.profdata
#
# Both use the environment of scripts/build-release-binaries.sh (STAGE=key and STAGE=train).
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source "${here}/families.sh"
repo=$(cd -- "${here}/../.." && pwd)

usage() { sed -n '2,8p' "${BASH_SOURCE[0]}" >&2; exit 2; }

arch=${2:-} flavour=${3:-}
require_member architecture "${arch}" "${package_arches[@]}"
require_member flavour "${flavour}" unwind abort
export PGO_TARGET=$(package_target "${arch}") FLAVOUR=${flavour} PGO=1 BOLT=0

case ${1:-} in
  key) PGO_TOOLCHAIN=${rust_toolchain} STAGE=key "${repo}/scripts/build-release-binaries.sh" 0.0.0-profile /dev/null ;;
  train) STAGE=train "${here}/main.sh" binary "${arch}" 0.0.0-profile "${4:?usage: $0 train ARCH FLAVOUR OUT_DIR}" ;;
  *) usage ;;
esac
