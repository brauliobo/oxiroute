#!/usr/bin/env bash
# Post-link optimisation (BOLT) of a profile-guided release binary.
#
#   bolt.sh all IN OUT          instrument IN, replay the R2 lanes through it, rewrite IN into OUT, check OUT
#   bolt.sh instrument IN OUT   add BOLT counters to IN (OUT runs about 3x slower; needs no perf and no LBR)
#   bolt.sh train INSTRUMENTED  replay the R2 lanes through INSTRUMENTED and collect $BOLT_WORK/prof/*.fdata
#   bolt.sh optimize IN OUT     merge the profiles and rewrite IN into OUT (hot/cold split, block and function order)
#   bolt.sh check IN OUT        the guards alone: profile coverage, size, PIE, BIND_NOW, RELRO, starts
#   bolt.sh lbr IN              record the same lanes with LBR branch stacks (bare-metal CPU with LBR; research only)
#
# IN must have been linked with relocations: RUSTFLAGS="-Clink-arg=-Wl,-q" (code generation is unchanged). BOLT only
# reads the ELF, so it is independent of the rustc LLVM version. llvm-bolt and merge-fdata come from BOLT_BIN_DIR
# (scripts/install-bolt.sh installs a checksum-verified release into one) or the PATH.
# Any failure fails the script: nothing here falls back to the input binary.
# Environment: BOLT_WORK (scratch), BOLT_LANES (replayed lanes), BOLT_SCALE (training request multiplier),
# BOLT_MIN_PROFILED (functions that must have a profile, default 1000), BOLT_MAX_GROWTH_PCT (default 50).
set -euo pipefail
export LC_ALL=C

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
work=${BOLT_WORK:-/var/tmp/oxiroute-bolt}
export PATH=${BOLT_BIN_DIR:+${BOLT_BIN_DIR}:}${PATH}
lanes=${BOLT_LANES:-h1_b1k h1_newconn_b1k tls_h1_keepalive static_f1k f_fixed h2_b1k_c4s16 tls_hs_full_tls13 h1_b64k h1_post4k f_access_log}
scale=${BOLT_SCALE:-0.3}
min_profiled=${BOLT_MIN_PROFILED:-1000}
max_growth_pct=${BOLT_MAX_GROWTH_PCT:-50}
tool=${R2TOOL_BIN:-}
opts=(-reorder-blocks=ext-tsp -reorder-functions=cdsort -split-functions -split-all-cold -split-eh -dyno-stats)

default_cpus() {
  local n; n=$(nproc)
  (( n >= 16 )) && return
  local proxy=$(( n >= 4 ? n / 2 : 1 )) load=$(( n > 2 ? n - 1 : 1 ))
  export R2_PROXY_CPUS=${R2_PROXY_CPUS:-0-$((proxy - 1))} R2_UPSTREAM_CPUS=${R2_UPSTREAM_CPUS:-${proxy}} \
    R2_LOAD_CPUS=${R2_LOAD_CPUS:-${load}}
}

instrument() {
  mkdir -p "${work}/prof"
  llvm-bolt "$1" -instrument -o "$2" --instrumentation-file="${work}/prof/inst.fdata" --instrumentation-file-append-pid
}

# The counters are written when the proxy exits normally (SIGTERM is handled), so every lane is its own process.
train() {
  local lane
  default_cpus
  rm -f "${work}"/prof/inst.fdata.*
  for lane in ${lanes}; do
    OXIROUTE_BIN=$1 R2TOOL_BIN=${tool} nice "${repo}/benchmarks/r2-data-path/run.sh" --train --lanes "${lane}" \
      --scale "${scale}" --out "${work}/out/${lane}"
    find "${work}/prof" -name 'inst.fdata.*' -size +0 | grep -q . || { echo "no BOLT profile after ${lane}" >&2; exit 1; }
  done
}

lbr() {
  local entry lane
  for entry in ${BOLT_LANES_LBR:-h1_b1k:1000000 f_fixed:2000000 h1_newconn_b1k:400000 tls_h1_keepalive:800000 static_f1k:1500000 h2_b1k_c4s16:600000 tls_hs_full_tls13:40000 h1_b64k:400000 h1_post4k:300000 f_access_log:600000}; do
    lane=${entry%%:*}
    OXIROUTE_BIN=$1 R2TOOL_BIN=${tool} nice "${repo}/benchmarks/r2-data-path/run.sh" --no-perf --lanes "${lane}" --profile \
      --profile-call-graph branches --profile-event cycles:u --profile-period 100003 --profile-requests "${entry##*:}" \
      --profile-warmup 2000 --profile-bin "$1" --out "${work}/lbr/${lane}"
    perf2bolt -p "${work}/lbr/${lane}/profile/${lane}.perf.data" -o "${work}/prof/lbr-${lane}.fdata" "$1"
  done
}

optimize() {
  local log="${work}/optimize.log"
  merge-fdata $(find "${work}/prof" -name '*.fdata*' -size +0) > "${work}/merged.fdata"
  [[ -s ${work}/merged.fdata ]] || { echo "BOLT profile is empty" >&2; exit 1; }
  llvm-bolt "$1" -o "$2" -data="${work}/merged.fdata" "${opts[@]}" 2>&1 | tee "${log}"
  [[ ${PIPESTATUS[0]} == 0 ]] || { echo "llvm-bolt failed" >&2; exit 1; }
}

# BOLT must have seen the hot code, must not have bloated the file, and must not have lost the hardening of the link.
check() {
  local in=$1 out=$2 profiled in_size out_size
  profiled=$(sed -n 's/^BOLT-INFO: \([0-9]*\) out of [0-9]* functions in the binary .* have non-empty execution profile.*/\1/p' \
    "${work}/optimize.log" | head -1)
  echo "bolt: ${profiled:-0} functions have a profile (need ${min_profiled})"
  (( ${profiled:-0} >= min_profiled )) || { echo "BOLT matched too little of the profile to the binary" >&2; exit 1; }
  in_size=$(stat -c %s "${in}") out_size=$(stat -c %s "${out}")
  echo "bolt: size ${in_size} -> ${out_size}"
  (( out_size * 100 <= in_size * (100 + max_growth_pct) )) || { echo "BOLT output grew more than ${max_growth_pct}%" >&2; exit 1; }
  readelf -h "${out}" | grep -q 'Type:.*DYN' || { echo "BOLT output is not position independent" >&2; exit 1; }
  readelf -lW "${out}" | grep -q GNU_RELRO || { echo "BOLT output lost RELRO" >&2; exit 1; }
  readelf -dW "${out}" | grep -q 'BIND_NOW\|NOW' || { echo "BOLT output lost BIND_NOW" >&2; exit 1; }
  "${out}" --version > /dev/null
}

all() {
  local instrumented="${work}/oxiroute.instrumented"
  rm -rf "${work}/prof" "${work}/out" "${work}/optimize.log" "${work}/merged.fdata"
  instrument "$1" "${instrumented}"
  train "${instrumented}"
  rm -f -- "${instrumented}"
  optimize "$1" "$2"
  check "$1" "$2"
}

case ${1:-} in
  all) all "${2:?IN}" "${3:?OUT}" ;;
  instrument) instrument "${2:?IN}" "${3:?OUT}" ;;
  train) train "${2:?INSTRUMENTED}" ;;
  lbr) lbr "${2:?IN}" ;;
  optimize) optimize "${2:?IN}" "${3:?OUT}" ;;
  check) check "${2:?IN}" "${3:?OUT}" ;;
  *) echo "usage: $0 all IN OUT | instrument IN OUT | train INSTRUMENTED | optimize IN OUT | check IN OUT | lbr IN" >&2; exit 2 ;;
esac
