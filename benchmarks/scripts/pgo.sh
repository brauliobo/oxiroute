#!/usr/bin/env bash
# Profile-guided optimisation of the release binary.
#
#   pgo.sh train   instrumented build, replay the R2 data-path lanes, merge into $PGO_PROFILE
#   pgo.sh build   release build that uses $PGO_PROFILE (same cargo command as packaging/arch/PKGBUILD)
#
# The profile is bound to the exact compiler: rustc mangles every symbol with a hash of its own version, and the
# profile is keyed by symbol name, so a different rustc (or a different cargo command line, e.g. without --target)
# matches nothing and the build silently falls back to plain optimisation. Train with the toolchain that builds.
# llvm-profdata must be at least as old as the rustc's LLVM raw-profile format: use the rustup `llvm-tools`
# component (`rustup component add llvm-tools`), or point LLVM_PROFDATA at a compatible one.
set -euo pipefail

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
host=x86_64-unknown-linux-gnu
profile=${PGO_PROFILE:-${repo}/packaging/pgo/oxiroute.profdata}
work=${PGO_WORK:-/dev/shm/oxiroute-pgo}
lanes=${PGO_LANES:-h1_b1k,f_fixed,h1_newconn_b1k,tls_h1_keepalive,static_f1k,h2_b1k_c4s16,tls_hs_full_tls13,h1_b64k,h1_post4k,f_access_log}
llvm_profdata=${LLVM_PROFDATA:-$(rustc --print sysroot)/lib/rustlib/${host}/bin/llvm-profdata}

cargo_build() {
  (cd "${repo}" && nice cargo build --release --locked --package oxiroute --package oxiroute-supervisor-process \
    --target "${host}" --jobs 4)
}

train() {
  rm -rf "${work}"
  mkdir -p "${work}/raw" "$(dirname -- "${profile}")"
  RUSTFLAGS="${RUSTFLAGS:-} -Cprofile-generate=${work}/unused" CARGO_TARGET_DIR="${work}/target-gen" cargo_build
  for lane in ${lanes//,/ }; do
    OXIROUTE_BIN="${work}/target-gen/${host}/release/oxiroute" "${repo}/benchmarks/r2-data-path/run.sh" \
      --lanes "${lane}" --reps 1 --scale 0.5 --out "${work}/out/${lane}" \
      --proxy-env "LLVM_PROFILE_FILE=${work}/raw/%m_%p.profraw"
  done
  "${llvm_profdata}" merge -o "${profile}" "${work}"/raw/*.profraw
  ls -l "${profile}"
}

build() {
  [[ -f ${profile} ]] || { echo "no profile at ${profile}; run: $0 train" >&2; exit 1; }
  RUSTFLAGS="${RUSTFLAGS:-} -Cprofile-use=${profile}" CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${work}/target-use}" cargo_build
}

case ${1:-} in
  train) train ;;
  build) build ;;
  *) echo "usage: $0 train|build" >&2; exit 2 ;;
esac
