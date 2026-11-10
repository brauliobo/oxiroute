#!/usr/bin/env bash
# Profile-guided optimisation of the release binary.
#
#   pgo.sh train        instrumented build, replay the R2 data-path lanes (no perf needed), merge into $PGO_PROFILE
#   pgo.sh gen          the instrumented build alone, and pgo.sh lane NAME replays one lane of it (train = gen, every
#                       lane of PGO_LANES, merge; the two steps let a long training be split and resumed)
#   pgo.sh merge        merge the per-lane profiles of the last training into $PGO_PROFILE (lane weights, see below)
#   pgo.sh build        release build that uses $PGO_PROFILE and fails if the compiler did not apply it
#   pgo.sh plain        the same release build without a profile
#   pgo.sh smoke BIN    start BIN on a generated configuration and push proxied requests through it
#
# The profile is bound to the exact compiler: rustc mangles every symbol with a hash of its own version, and the
# profile is keyed by symbol name, so a different rustc (or a different cargo command line, e.g. without --target)
# matches nothing and the build would silently fall back to plain optimisation. `build` therefore counts the functions
# the compiler could not match and fails above PGO_MAX_MISSING_PCT. Train with the toolchain that builds.
# llvm-profdata must read the compiler's raw-profile format: use the rustup `llvm-tools` component
# (`rustup component add llvm-tools`), or point LLVM_PROFDATA at a compatible one.
#
# Every lane keeps its own merged profile ($PGO_WORK/lane/NAME.profdata), so the mix can be changed without training
# again: PGO_LANES entries are NAME or NAME:WEIGHT (llvm-profdata -weighted-input, default 1); `merge` combines them.
#
# Environment: PGO_WORK (scratch and default profile location), PGO_PROFILE, PGO_LANES, PGO_CARGO_PROFILE, PGO_SCALE
# (request-count multiplier of every lane), PGO_JOBS, PGO_TARGET, LLVM_PROFDATA, CARGO_TARGET_DIR (final build only),
# R2_PROXY_CPUS / R2_UPSTREAM_CPUS / R2_LOAD_CPUS (default: derived from nproc below 16 CPUs, harness defaults above).
set -euo pipefail

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
target=${PGO_TARGET:-x86_64-unknown-linux-gnu}
work=${PGO_WORK:-/dev/shm/oxiroute-pgo}
profile=${PGO_PROFILE:-${work}/oxiroute.profdata}
# Core lanes (weight 4) keep the hot proxy path laid out as the 6.3 mix did; the weight-1 lanes add the features that
# the core lanes never execute (HTTP/2 bodies, WebSocket, TCP relay, cache, gzip, large bodies, TLS 1.2, the 6.0
# features, least-connections pools, forward proxy), which a profile without them lays out as cold code.
core_lanes=h1_b1k:4,f_fixed:4,h1_newconn_b1k:4,tls_h1_keepalive:4,static_f1k:4,h2_b1k_c4s16:4,tls_hs_full_tls13:4,h1_b64k:4,h1_post4k:4,f_access_log:4
wide_lanes=h2_b64k_c4s16,h2_post1k_c4s16,ws_echo_1k,l4_echo_1k,cache_hit_b1k,gzip_html64k,h1_post64k,h1_b1m,tls_h1_b1m,tls_hs_full_tls12,f_all_6_0,pool_lc_s64,h1_hdr2048,fwd_h1_b1k
lanes=${PGO_LANES:-${core_lanes},${wide_lanes}}
scale=${PGO_SCALE:-0.5}
jobs=${PGO_JOBS:-4}
cargo_profile=${PGO_CARGO_PROFILE:-release}
cargo_dir=${cargo_profile}
max_missing_pct=${PGO_MAX_MISSING_PCT:-5}
llvm_profdata=${LLVM_PROFDATA:-$(rustc --print sysroot)/lib/rustlib/${target}/bin/llvm-profdata}

# The cargo command of packaging/arch/PKGBUILD; every build of this script uses exactly these arguments. A profile other
# than release (PGO_CARGO_PROFILE=release-abort) has its own profile data: unwinding changes the control-flow hashes.
cargo_build() {
  local select=(--release)
  [[ ${cargo_profile} == release ]] || select=(--profile "${cargo_profile}")
  (cd "${repo}" && nice cargo build "${select[@]}" --locked --package oxiroute --package oxiroute-supervisor-process \
    --target "${target}" --jobs "${jobs}")
}

# The training only needs the three roles on disjoint CPUs; small runners have 2 to 4.
default_cpus() {
  local n; n=$(nproc)
  (( n >= 16 )) && return
  local proxy=$(( n >= 4 ? n / 2 : 1 )) load=$(( n > 2 ? n - 1 : 1 ))
  export R2_PROXY_CPUS=${R2_PROXY_CPUS:-0-$((proxy - 1))} R2_UPSTREAM_CPUS=${R2_UPSTREAM_CPUS:-${proxy}} \
    R2_LOAD_CPUS=${R2_LOAD_CPUS:-${load}}
}

build_tool() {
  [[ -x ${work}/target-tool/release/r2tool ]] && return
  (cd "${repo}/benchmarks/r2-data-path/tool" && CARGO_TARGET_DIR="${work}/target-tool" nice cargo build --release \
    --locked --jobs 2)
}

gen() {
  [[ -x ${llvm_profdata} ]] || { echo "llvm-profdata not found at ${llvm_profdata}; rustup component add llvm-tools" >&2; exit 1; }
  rm -rf "${work}/raw" "${work}/lane" "${work}/out" "${profile}"
  mkdir -p "${work}/lane" "$(dirname -- "${profile}")"
  RUSTFLAGS="${RUSTFLAGS:-} -Cprofile-generate=${work}/unused" CARGO_TARGET_DIR="${work}/target-gen" cargo_build
  build_tool
}

lane() {
  local lane=$1
  mkdir -p "${work}/raw/${lane}"
  rm -f "${work}/raw/${lane}"/*.profraw
  default_cpus
  OXIROUTE_BIN="${work}/target-gen/${target}/${cargo_dir}/oxiroute" R2TOOL_BIN="${work}/target-tool/release/r2tool" \
    "${repo}/benchmarks/r2-data-path/run.sh" --train --lanes "${lane}" --scale "${scale}" --out "${work}/out/${lane}" \
    --proxy-env "LLVM_PROFILE_FILE=${work}/raw/${lane}/%m_%p.profraw"
  compgen -G "${work}/raw/${lane}/*.profraw" > /dev/null || { echo "lane ${lane} wrote no raw profiles" >&2; exit 1; }
  "${llvm_profdata}" merge -o "${work}/lane/${lane}.profdata" "${work}/raw/${lane}"/*.profraw
}

train() {
  gen
  for entry in ${lanes//,/ }; do lane "${entry%%:*}"; done
  merge
}

merge() {
  local inputs=() entry weight
  for entry in ${lanes//,/ }; do
    weight=1
    [[ ${entry} == *:* ]] && weight=${entry#*:}
    inputs+=("-weighted-input=${weight},${work}/lane/${entry%%:*}.profdata")
  done
  "${llvm_profdata}" merge -o "${profile}" "${inputs[@]}"
  ls -l "${profile}"
}

# Functions the compiler could not match to the profile (no entry, or a control-flow hash mismatch), in percent of the
# functions the profile holds. A matching build reports almost none; a profile from another compiler reports all.
check_profile_applied() {
  local log=$1 total missing
  total=$("${llvm_profdata}" show "${profile}" | sed -n 's/^Total functions: //p')
  missing=$(grep -c 'no profile data available for function\|control flow change detected' "${log}" || true)
  echo "profile: ${missing} of ${total} functions unmatched"
  (( total > 0 && missing * 100 <= max_missing_pct * total )) || {
    echo "the compiler ignored the profile (same rustc, same cargo arguments and the same source are required)" >&2
    exit 1
  }
}

build() {
  [[ -f ${profile} ]] || { echo "no profile at ${profile}; run: $0 train" >&2; exit 1; }
  local log="${work}/use-build.log"
  mkdir -p "${work}"
  RUSTFLAGS="${RUSTFLAGS:-} -Cprofile-use=${profile} -Cllvm-args=-pgo-warn-missing-function" \
    CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${work}/target-use}" cargo_build 2>&1 | tee "${log}"
  check_profile_applied "${log}"
}

smoke() {
  local binary=${1:?usage: $0 smoke BIN}
  mkdir -p "${work}/smoke"
  python3 "${repo}/benchmarks/r2-data-path/gen_config.py" "${work}/smoke/oxiroute.lua" --proxy-port 20300 \
    --upstream-port 20310 --run-dir "${work}/smoke"
  "${binary}" config check "${work}/smoke/oxiroute.lua"
  default_cpus
  build_tool
  OXIROUTE_BIN=${binary} R2TOOL_BIN="${work}/target-tool/release/r2tool" "${repo}/benchmarks/r2-data-path/run.sh" \
    --train --lanes h1_b1k,f_fixed --scale 0.05 --out "${work}/smoke"
}

case ${1:-} in
  train) train ;;
  gen) gen ;;
  lane) lane "${2:?usage: $0 lane NAME}" ;;
  merge) merge ;;
  build) build ;;
  plain) CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${work}/target-plain}" cargo_build ;;
  smoke) smoke "${2:-}" ;;
  *) echo "usage: $0 train|gen|lane NAME|merge|build|plain|smoke BIN" >&2; exit 2 ;;
esac
