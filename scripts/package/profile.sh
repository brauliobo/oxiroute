#!/usr/bin/env bash
# The profile-guided-optimisation profile of one architecture and flavour, so the release workflow can reuse a stored
# profile instead of training it in every build (docs/developer/RELEASING.md, "The profile").
#
#   profile.sh key ARCH FLAVOUR            the cache key of the profile: pgo-ARCH-FLAVOUR-vVERSION-HASH, HASH over the toolchain,
#                                          target, flavour, lanes and the hot-path sources, never over the version
#   profile.sh prefix ARCH FLAVOUR         pgo-ARCH-FLAVOUR-vVERSION-: every key of that version, the profiles that still fit it
#   profile.sh train ARCH FLAVOUR OUT_DIR  train it in the glibc-floor container (instrumented build, lanes, merge): OUT_DIR/oxiroute.profdata
#
# Both use the environment of scripts/build-release-binaries.sh (STAGE=key and STAGE=train).
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source "${here}/families.sh"
repo=$(cd -- "${here}/../.." && pwd)

usage() { sed -n '2,10p' "${BASH_SOURCE[0]}" >&2; exit 2; }

arch=${2:-} flavour=${3:-}
require_member architecture "${arch}" "${package_arches[@]}"
require_member flavour "${flavour}" unwind abort
export PGO_TARGET=$(package_target "${arch}") FLAVOUR=${flavour} PGO=1 BOLT=0

case ${1:-} in
  prefix) printf 'pgo-%s-%s-v%s-\n' "${arch}" "${flavour}" "$("${repo}/benchmarks/scripts/pgo.sh" version)" ;;
  key) printf '%s%s\n' "$("$0" prefix "${arch}" "${flavour}")" \
    "$(PGO_TOOLCHAIN=${rust_toolchain} STAGE=key "${repo}/scripts/build-release-binaries.sh" 0.0.0-profile /dev/null)" ;;
  train) STAGE=train "${here}/main.sh" binary "${arch}" 0.0.0-profile "${4:?usage: $0 train ARCH FLAVOUR OUT_DIR}" ;;
  *) usage ;;
esac
