#!/usr/bin/env bash
# Builds the release binaries of one target and packs them as a tarball with checksum, build record and release-note
# line. Profile-guided by default (benchmarks/scripts/pgo.sh: instrumented build, training replay, merge, profile-use
# build) and then laid out by BOLT (benchmarks/scripts/bolt.sh: counters added to the binary, replay, rewrite, guards,
# then the smoke lanes and the whole workspace test suite against the rewritten binary). PGO=0 builds without a
# profile, BOLT=0 skips the layout stage. A training, profile, BOLT or test failure fails the script: there is no
# silent fallback to the previous stage.
#
#   build-release-binaries.sh VERSION OUT_DIR
#
# FLAVOUR=abort builds the fail-fast flavour (cargo profile release-abort, panic = "abort") with its own profile data
# and publishes it as oxiroute-VERSION-TARGET-abort.tar.gz; the default flavour keeps unwinding, which is what
# contains a panic to one connection or one generation (docs/developer/RELEASING.md).
#
# Environment: PGO and BOLT (1 or 0, default 1), FLAVOUR (unwind or abort, default unwind), PGO_TARGET (default
# x86_64-unknown-linux-gnu), PGO_WORK and the other benchmarks/scripts/pgo.sh variables, BOLT_BIN_DIR (directory of
# llvm-bolt and merge-fdata, see scripts/install-bolt.sh) and the other benchmarks/scripts/bolt.sh variables,
# PGO_KEEP_INSTRUMENTED=1 (keep the instrumented build; it is removed to save disk), SOURCE_DATE_EPOCH (default:
# commit time of HEAD).
set -euo pipefail

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
version=${1:-}
out=${2:-}
[[ -n ${version} && -n ${out} && ${version} != -* && ${out} != -* ]] || { echo "usage: $0 VERSION OUT_DIR" >&2; exit 2; }
target=${PGO_TARGET:-x86_64-unknown-linux-gnu}
pgo=${PGO:-1}
bolt=${BOLT:-1}
flavour=${FLAVOUR:-unwind}
[[ ${pgo} == 0 || ${pgo} == 1 ]] || { echo "PGO must be 0 or 1" >&2; exit 2; }
[[ ${bolt} == 0 || ${bolt} == 1 ]] || { echo "BOLT must be 0 or 1" >&2; exit 2; }
[[ ${flavour} == unwind || ${flavour} == abort ]] || { echo "FLAVOUR must be unwind or abort" >&2; exit 2; }
pgo_script=${repo}/benchmarks/scripts/pgo.sh
bolt_script=${repo}/benchmarks/scripts/bolt.sh
work=${PGO_WORK:-${HOME}/.cache/oxiroute-pgo}
cargo_profile=release
suffix=
if [[ ${flavour} == abort ]]; then
  cargo_profile=release-abort suffix=-abort
  work=${work}/abort
fi
export PGO_WORK=${work} PGO_CARGO_PROFILE=${cargo_profile}
profile=${PGO_PROFILE:-${work}/oxiroute.profdata}
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git -C "${repo}" log -1 --format=%ct HEAD)}
name=oxiroute-${version}-${target}${suffix}
# BOLT rewrites the linked binary and needs its relocations; code generation does not change, so the profile still matches.
if [[ ${bolt} == 1 ]]; then
  export RUSTFLAGS="${RUSTFLAGS:-} -Clink-arg=-Wl,-q"
  export PATH=${BOLT_BIN_DIR:+${BOLT_BIN_DIR}:}${PATH}
  command -v llvm-bolt > /dev/null && command -v merge-fdata > /dev/null || {
    echo "llvm-bolt and merge-fdata are required (scripts/install-bolt.sh DIR, then BOLT_BIN_DIR=DIR/usr/lib/llvm-18/bin), or BOLT=0" >&2
    exit 1
  }
fi
out=$(realpath -m -- "${out}")
mkdir -p -- "${out}"

if [[ ${pgo} == 1 ]]; then
  "${pgo_script}" train
  [[ ${PGO_KEEP_INSTRUMENTED:-0} == 1 ]] || rm -rf -- "${work}/target-gen"
  "${pgo_script}" build
  release=${CARGO_TARGET_DIR:-${work}/target-use}/${target}/${cargo_profile}
  pgo_json=true
  note="built with profile-guided optimization (trained on the pinned compiler by the release workflow; \`${name}.build.json\` records it)"
  profile_sha256="\"$(sha256sum "${profile}" | cut -d' ' -f1)\""
else
  "${pgo_script}" plain
  release=${CARGO_TARGET_DIR:-${work}/target-plain}/${target}/${cargo_profile}
  pgo_json=false
  note="a plain build without profile-guided optimization (the release workflow was started with PGO disabled)"
  profile_sha256=null
fi
"${pgo_script}" smoke "${release}/oxiroute"

shipped=${release}/oxiroute
if [[ ${bolt} == 1 ]]; then
  export BOLT_WORK=${work}/bolt R2TOOL_BIN=${work}/target-tool/release/r2tool
  shipped=${BOLT_WORK}/oxiroute
  mkdir -p "${BOLT_WORK}"
  "${bolt_script}" all "${release}/oxiroute" "${shipped}"
  "${pgo_script}" smoke "${shipped}"
  note="${note}, then relinked by BOLT for instruction layout"
fi
# The binary that ships, not the one cargo test builds, runs the whole workspace test suite.
if [[ ${bolt} == 1 || ${flavour} == abort ]]; then
  BOLT_TEST_TARGET_DIR=${work}/target-test "${repo}/scripts/test-release-binary.sh" "${shipped}"
  rm -rf -- "${work}/target-test"
  note="${note} (the workspace tests ran against it)"
fi
if [[ ${flavour} == abort ]]; then
  note="${note}. This is the fail-fast flavour (\`panic = \"abort\"\`): a panic ends the whole worker process, which the supervisor reports as a failed generation, instead of closing one connection"
fi

stage=$(mktemp -d)
trap 'rm -rf -- "${stage}"' EXIT
root=${stage}/${name}
install -Dm755 "${shipped}" "${root}/bin/oxiroute"
ln -s oxiroute "${root}/bin/oxr"
install -Dm755 "${release}/oxiroute-worker-launcher" "${root}/lib/oxiroute/oxiroute-worker-launcher"
install -Dm644 "${repo}/LICENSE" "${root}/LICENSE"
cat > "${root}/BUILD-INFO.json" <<JSON
{
  "schema": "oxiroute.release-binary.v1",
  "version": "${version}",
  "target": "${target}",
  "commit": "$(git -C "${repo}" rev-parse HEAD)",
  "rustc": "$(rustc --version)",
  "pgo": ${pgo_json},
  "pgo_profile_sha256": ${profile_sha256},
  "bolt": $([[ ${bolt} == 1 ]] && echo true || echo false),
  "panic": "$([[ ${flavour} == abort ]] && echo abort || echo unwind)"
}
JSON

tar --directory="${stage}" --sort=name --mtime="@${SOURCE_DATE_EPOCH}" --owner=0 --group=0 --numeric-owner \
  --create "${name}" | gzip -n > "${out}/${name}.tar.gz"
(cd "${out}" && sha256sum "${name}.tar.gz" > "${name}.tar.gz.sha256")
cp -- "${root}/BUILD-INFO.json" "${out}/${name}.build.json"
printf "The \`%s\` binaries%s are %s.\n" "${target}" "${suffix:+ (\`${flavour}\` flavour)}" "${note}" > "${out}/pgo-note-${target}${suffix}.md"
ls -l -- "${out}"
