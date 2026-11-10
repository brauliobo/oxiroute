#!/usr/bin/env bash
# Post-link optimisation (BOLT) of a profile-guided release binary. Research prototype, see
# docs/developer/research/PERF_6_4_BINARY_LEVEL.md.
#
#   bolt.sh instrument IN OUT   add BOLT counters to IN (OUT runs about 3x slower; needs no perf and no LBR)
#   bolt.sh train INSTRUMENTED  replay the R2 lanes through INSTRUMENTED and collect $BOLT_WORK/prof/*.fdata
#   bolt.sh lbr IN              record the same lanes with LBR branch stacks (bare-metal CPU with LBR, perf record -j)
#   bolt.sh optimize IN OUT     merge the profiles and rewrite IN into OUT (hot/cold split, block and function order)
#
# IN must have been linked with relocations: RUSTFLAGS="-Clink-arg=-Wl,-q" (code generation is unchanged). BOLT only
# reads the ELF, so it is independent of the rustc LLVM version. llvm-bolt, perf2bolt and merge-fdata come from the
# distribution (Arch: llvm-bolt) or any LLVM release build; BOLT_BIN_DIR points at them when they are not on PATH.
# Environment: BOLT_WORK (scratch), BOLT_LANES (lane:requests list for `lbr`), BOLT_SCALE (training request multiplier).
set -euo pipefail

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
work=${BOLT_WORK:-/dev/shm/oxiroute-bolt}
export PATH=${BOLT_BIN_DIR:+${BOLT_BIN_DIR}:}${PATH}
lanes=${BOLT_LANES:-h1_b1k h1_newconn_b1k tls_h1_keepalive static_f1k f_fixed h2_b1k_c4s16 tls_hs_full_tls13 h1_b64k h1_post4k f_access_log}
scale=${BOLT_SCALE:-0.3}
tool=${R2TOOL_BIN:-}
opts=(-reorder-blocks=ext-tsp -reorder-functions=cdsort -split-functions -split-all-cold -split-eh -dyno-stats)

instrument() {
  mkdir -p "${work}/prof"
  llvm-bolt "$1" -instrument -o "$2" --instrumentation-file="${work}/prof/inst.fdata" --instrumentation-file-append-pid
}

# The counters are written when the proxy exits normally (SIGTERM is handled), so every lane is its own process.
train() {
  local lane
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
  merge-fdata $(find "${work}/prof" -name '*.fdata*' -size +0) > "${work}/merged.fdata"
  llvm-bolt "$1" -o "$2" -data="${work}/merged.fdata" "${opts[@]}"
}

case ${1:-} in
  instrument) instrument "${2:?IN}" "${3:?OUT}" ;;
  train) train "${2:?INSTRUMENTED}" ;;
  lbr) lbr "${2:?IN}" ;;
  optimize) optimize "${2:?IN}" "${3:?OUT}" ;;
  *) echo "usage: $0 instrument IN OUT | train INSTRUMENTED | lbr IN | optimize IN OUT" >&2; exit 2 ;;
esac
