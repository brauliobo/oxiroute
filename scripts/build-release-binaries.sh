#!/usr/bin/env bash
# Builds the release binaries of one target and packs them as a tarball with checksum, build record and release-note
# line. Profile-guided by default (benchmarks/scripts/pgo.sh: instrumented build, training replay, merge, profile-use
# build) and then laid out by BOLT (benchmarks/scripts/bolt.sh: counters added to the binary, replay, rewrite, guards,
# then the smoke lanes and the whole workspace test suite against the rewritten binary). PGO=0 builds without a
# profile, BOLT=0 skips the layout stage. A training, profile, BOLT or test failure fails the script: there is no
# silent fallback to the previous stage.
# The binaries must not embed a home-directory, runner or checkout path (scripts/verify/binary-paths.sh). The vendored
# OpenSSL records its own build directory, so PGO_WORK must lie outside /home (default: /var/tmp/oxiroute-pgo).
#
#   build-release-binaries.sh VERSION OUT_DIR
#
# FLAVOUR=abort builds the fail-fast flavour (cargo profile release-abort, panic = "abort") with its own profile data
# and publishes it as oxiroute-VERSION-TARGET-abort.tar.gz; the default flavour keeps unwinding, which is what
# contains a panic to one connection or one generation (docs/developer/RELEASING.md).
#
# FLAVOUR=v3 builds the C code of the dependencies (the vendored OpenSSL above all) for x86-64-v3 CPUs (AVX2, BMI2:
# -march=x86-64-v3) and publishes it as oxiroute-VERSION-TARGET-x86-64-v3.tar.gz; it measures 10% fewer instructions per
# TLS handshake and does not start on older CPUs, so it is never the default artefact. Every flavour compiles that C code
# with -O3 (CFLAGS for the target only; a plain `cargo build` keeps the cc crate default).
#
# Every flavour but v3 must run on generic x86-64 and pick AVX at run time (crate oxiroute-cpu): the build refuses a
# RUSTFLAGS or CFLAGS that bakes in a CPU target (scripts/lib/reject-isa-flags.sh), and scripts/verify/baseline-isa.sh
# checks the binaries (disassembly, and `version` under qemu CPUs without AVX when qemu-x86_64 is installed).
# PGO_TARGET=aarch64-unknown-linux-gnu builds on an aarch64 host with the same gates against the ARMv8-A baseline; BOLT
# and the v3 flavour are x86_64 only and refused for it.
#
# Environment: PGO and BOLT (1 or 0, default 1), FLAVOUR (unwind, abort or v3, default unwind), PGO_TARGET (default
# x86_64-unknown-linux-gnu), PGO_WORK and the other benchmarks/scripts/pgo.sh variables, BOLT_BIN_DIR (directory of
# llvm-bolt and merge-fdata, see scripts/install-bolt.sh) and the other benchmarks/scripts/bolt.sh variables,
# PGO_KEEP_INSTRUMENTED=1 (keep the instrumented build; it is removed to save disk), PGO_PROFILE_GIVEN=1 (the merged profile
# at PGO_PROFILE, default PGO_WORK/oxiroute.profdata, was trained by an earlier job: skip the instrumented build and the
# training, and let the profile-use build decide whether it still matches, pgo.sh build), SOURCE_DATE_EPOCH (default:
# commit time of HEAD), RELEASE_COMMIT (recorded commit, default HEAD; for a source tree without a git repository).
#
# The build is two stages that can run on different machines (STAGE, default all = both in one process):
#   STAGE=pgo     training, profile-use build, path, ISA and glibc checks and the smoke lanes of the binaries; leaves
#                 OUT_DIR/stage (the binaries and stage.env). This is the stage that fixes the libc a binary links, so the
#                 release workflow runs it in a container of the oldest supported system (GLIBC_FLOOR=2.34).
#   STAGE=layout  from OUT_DIR/stage: the BOLT layout (x86_64), the tests of the shipped binary and the tarball.
#   STAGE=train   only the training: the instrumented build, the lanes and the merge, leaving OUT_DIR/oxiroute.profdata for a
#                 later build with PGO_PROFILE_GIVEN=1 (scripts/package/profile.sh, the profile workflow).
#   STAGE=key     prints the key of the profile that PGO_TOOLCHAIN (default: rustc --version) would train (pgo.sh key).
# GLIBC_FLOOR (for example 2.34) makes the script fail when a binary needs a newer glibc symbol version
# (scripts/verify/glibc_floor.sh); unset, nothing is checked. RELEASE_TESTS=0 leaves the workspace tests of the shipped
# binary (BOLT and abort flavours) to scripts/test/release/binary.sh run by another job of the workflow.
set -euo pipefail

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
version=${1:-}
out=${2:-}
[[ -n ${version} && -n ${out} && ${version} != -* && ${out} != -* ]] || { echo "usage: $0 VERSION OUT_DIR" >&2; exit 2; }
target=${PGO_TARGET:-x86_64-unknown-linux-gnu}
pgo=${PGO:-1}
bolt=${BOLT:-1}
flavour=${FLAVOUR:-unwind}
stage_name=${STAGE:-all}
release_tests=${RELEASE_TESTS:-1}
profile_given=${PGO_PROFILE_GIVEN:-0}
[[ ${pgo} == 0 || ${pgo} == 1 ]] || { echo "PGO must be 0 or 1" >&2; exit 2; }
[[ ${bolt} == 0 || ${bolt} == 1 ]] || { echo "BOLT must be 0 or 1" >&2; exit 2; }
[[ ${profile_given} == 0 || ${profile_given} == 1 ]] || { echo "PGO_PROFILE_GIVEN must be 0 or 1" >&2; exit 2; }
[[ ${release_tests} == 0 || ${release_tests} == 1 ]] || { echo "RELEASE_TESTS must be 0 or 1" >&2; exit 2; }
[[ ${flavour} == unwind || ${flavour} == abort || ${flavour} == v3 ]] || { echo "FLAVOUR must be unwind, abort or v3" >&2; exit 2; }
[[ ${stage_name} =~ ^(all|pgo|layout|train|key)$ ]] || { echo "STAGE must be all, pgo, layout, train or key" >&2; exit 2; }
[[ ${target} == x86_64-unknown-linux-gnu || ${target} == aarch64-unknown-linux-gnu ]] || { echo "PGO_TARGET must be x86_64-unknown-linux-gnu or aarch64-unknown-linux-gnu" >&2; exit 2; }
[[ ${target} == x86_64-* || ${flavour} != v3 ]] || { echo "FLAVOUR=v3 is an x86-64 flavour" >&2; exit 2; }
[[ ${target} == x86_64-* || ${bolt} == 0 ]] || { echo "BOLT is installed and verified for x86_64 only (scripts/install-bolt.sh pins amd64 packages); use BOLT=0" >&2; exit 2; }
source "${repo}/scripts/lib/reject-isa-flags.sh"
source "${repo}/scripts/lib/stage/time.sh"
reject_isa_flags "${target}"
export ISA_GUARD_DONE=1 # the CFLAGS_* below are ours, pgo.sh must not reject the v3 flavour's
pgo_script=${repo}/benchmarks/scripts/pgo.sh
bolt_script=${repo}/benchmarks/scripts/bolt.sh
work=${PGO_WORK:-/var/tmp/oxiroute-pgo}
cargo_profile=release
suffix=
c_flags=-O3
if [[ ${flavour} == abort ]]; then
  cargo_profile=release-abort suffix=-abort
  work=${work}/abort
elif [[ ${flavour} == v3 ]]; then
  suffix=-x86-64-v3 c_flags="-O3 -march=x86-64-v3"
  work=${work}/v3
fi
# Target-specific, so build scripts of the build machine keep their flags; reaches every cc-crate build (OpenSSL, aws-lc, lua).
export "CFLAGS_${target//-/_}=${c_flags}"
export PGO_WORK=${work} PGO_CARGO_PROFILE=${cargo_profile}
profile=${PGO_PROFILE:-${work}/oxiroute.profdata}
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git -C "${repo}" log -1 --format=%ct HEAD)}
name=oxiroute-${version}-${target}${suffix}
# BOLT rewrites the linked binary and needs its relocations; code generation does not change, so the profile still matches.
[[ ${bolt} == 0 ]] || export RUSTFLAGS="${RUSTFLAGS:-} -Clink-arg=-Wl,-q"
# The v3 flavour needs AVX2 by design; the launcher has no `version` command, so only `oxiroute` runs under qemu.
verify_isa() { [[ ${flavour} == v3 ]] || "${repo}/scripts/verify/baseline-isa.sh" "$@"; }
verify_glibc() { [[ -z ${GLIBC_FLOOR:-} ]] || "${repo}/scripts/verify/glibc_floor.sh" "${GLIBC_FLOOR}" "$@"; }
verify_binary() {
  "${repo}/scripts/verify/binary-paths.sh" "$1" "${repo}" "${CARGO_HOME:-${HOME}/.cargo}"
  verify_glibc "$1"
}
out=$(realpath -m -- "${out}")
stage_dir=${out}/stage
[[ ${stage_name} == key ]] || mkdir -p -- "${out}"

train_profile() {
  if [[ ${profile_given} == 1 ]]; then
    [[ -f ${profile} ]] || { echo "PGO_PROFILE_GIVEN=1 but there is no profile at ${profile}; train it (pgo.sh train) or unset it" >&2; exit 1; }
    return
  fi
  timed training "${pgo_script}" train
  [[ ${PGO_KEEP_INSTRUMENTED:-0} == 1 ]] || rm -rf -- "${work}/target-gen"
}

start_tool_build() {
  [[ ${pgo} == 0 || ${profile_given} == 1 ]] || return 0
  "${pgo_script}" tool &
  tool_pid=$!
}

pgo_stage() {
  local tool_pid=
  start_tool_build
  if [[ ${pgo} == 1 ]]; then
    train_profile
    timed profile-use "${pgo_script}" build
    release=${CARGO_TARGET_DIR:-${work}/target-use}/${target}/${cargo_profile}
    pgo_json=true
    note="built with profile-guided optimization (trained on the pinned compiler by the release workflow; \`${name}.build.json\` records it)"
    profile_sha256="\"$(sha256sum "${profile}" | cut -d' ' -f1)\""
  else
    timed plain-build "${pgo_script}" plain
    release=${CARGO_TARGET_DIR:-${work}/target-plain}/${target}/${cargo_profile}
    pgo_json=false
    note="a plain build without profile-guided optimization (the release workflow was started with PGO disabled)"
    profile_sha256=null
  fi
  for binary in oxiroute oxiroute-worker-launcher; do
    verify_binary "${release}/${binary}"
    verify_isa "${release}/${binary}"
  done
  verify_isa --run "${release}/oxiroute"
  [[ -z ${tool_pid} ]] || wait "${tool_pid}"
  timed smoke "${pgo_script}" smoke "${release}/oxiroute"
}

save_stage() {
  rm -rf -- "${stage_dir}"
  install -Dm755 "${release}/oxiroute" "${stage_dir}/oxiroute"
  install -Dm755 "${release}/oxiroute-worker-launcher" "${stage_dir}/oxiroute-worker-launcher"
  install -Dm755 "${work}/target-tool/release/r2tool" "${stage_dir}/r2tool"
  printf 'pgo_json=%q\nnote=%q\nprofile_sha256=%q\n' "${pgo_json}" "${note}" "${profile_sha256}" > "${stage_dir}/stage.env"
}

load_stage() {
  [[ -f ${stage_dir}/stage.env ]] || { echo "no stage of the profile-guided build in ${stage_dir} (STAGE=pgo)" >&2; exit 1; }
  source "${stage_dir}/stage.env"
  release=${stage_dir}
  install -Dm755 "${stage_dir}/r2tool" "${work}/target-tool/release/r2tool"
  for binary in oxiroute oxiroute-worker-launcher; do
    verify_binary "${release}/${binary}"
  done
  "${pgo_script}" smoke "${release}/oxiroute"
}

require_bolt() {
  export PATH=${BOLT_BIN_DIR:+${BOLT_BIN_DIR}:}${PATH}
  command -v llvm-bolt > /dev/null && command -v merge-fdata > /dev/null || {
    echo "llvm-bolt and merge-fdata are required (scripts/install-bolt.sh DIR, then BOLT_BIN_DIR=DIR/usr/lib/llvm-18/bin), or BOLT=0" >&2
    exit 1
  }
}

layout_stage() {
  shipped=${release}/oxiroute
  if [[ ${bolt} == 1 ]]; then
    export BOLT_WORK=${work}/bolt R2TOOL_BIN=${work}/target-tool/release/r2tool
    shipped=${BOLT_WORK}/oxiroute
    mkdir -p "${BOLT_WORK}"
    timed bolt "${bolt_script}" all "${release}/oxiroute" "${shipped}"
    verify_binary "${shipped}"
    verify_isa --run "${shipped}"
    timed smoke-bolt "${pgo_script}" smoke "${shipped}"
    note="${note}, then relinked by BOLT for instruction layout"
  fi
  if [[ ${bolt} == 1 || ${flavour} == abort ]]; then
    if [[ ${release_tests} == 1 ]]; then
      # The binary that ships, not the one cargo test builds, runs the whole workspace test suite.
      BOLT_TEST_TARGET_DIR=${work}/target-test timed workspace-tests "${repo}/scripts/test/release/binary.sh" "${shipped}"
      rm -rf -- "${work}/target-test"
      note="${note} (the workspace tests ran against it)"
    else
      note="${note} (the release workflow runs the workspace tests against it before it publishes)"
    fi
  fi
  if [[ ${flavour} == v3 ]]; then
    note="${note}. This is the x86-64-v3 flavour: the C dependencies, the vendored OpenSSL above all, are compiled with \`-march=x86-64-v3\` and the binary needs a CPU with AVX2 and BMI2 (Intel Haswell, AMD Excavator or newer)"
  fi
  if [[ ${flavour} == abort ]]; then
    note="${note}. This is the fail-fast flavour (\`panic = \"abort\"\`): a panic ends the whole worker process, which the supervisor reports as a failed generation, instead of closing one connection"
  fi
}

pack() {
  local stage root
  stage=$(mktemp -d)
  trap 'rm -rf -- "${stage}"' RETURN
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
  "commit": "${RELEASE_COMMIT:-$(git -C "${repo}" rev-parse HEAD)}",
  "rustc": "$(rustc --version)",
  "pgo": ${pgo_json},
  "pgo_profile_sha256": ${profile_sha256},
  "bolt": $([[ ${bolt} == 1 ]] && echo true || echo false),
  "panic": "$([[ ${flavour} == abort ]] && echo abort || echo unwind)",
  "c_flags": "${c_flags}"
}
JSON
  tar --directory="${stage}" --sort=name --mtime="@${SOURCE_DATE_EPOCH}" --owner=0 --group=0 --numeric-owner \
    --create "${name}" | gzip -n > "${out}/${name}.tar.gz"
  (cd "${out}" && sha256sum "${name}.tar.gz" > "${name}.tar.gz.sha256")
  cp -- "${root}/BUILD-INFO.json" "${out}/${name}.build.json"
  printf "The \`%s\` binaries%s are %s.\n" "${target}" "${suffix:+ (\`${flavour}\` flavour)}" "${note}" > "${out}/pgo-note-${target}${suffix}.md"
  ls -l -- "${out}"
}

[[ ${stage_name} != layout && ${stage_name} != all || ${bolt} == 0 ]] || require_bolt
case ${stage_name} in
  key) "${pgo_script}" key "${PGO_TOOLCHAIN:-$(rustc --version)}" ;;
  train) train_profile; install -Dm644 "${profile}" "${out}/oxiroute.profdata" ;;
  pgo) pgo_stage; save_stage ;;
  layout) load_stage; layout_stage; verify_glibc "${shipped}"; timed pack pack ;;
  all) pgo_stage; layout_stage; verify_glibc "${shipped}"; timed pack pack ;;
esac
