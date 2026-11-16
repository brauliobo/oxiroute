#!/bin/sh
# The non-fairness measurements of the BBR decision, cubic and bbr alternating: lossy and long-round-trip downloads, the
# loss-free LAN 48 MiB download, and the LAN 1 KiB request rate. One JSON record per run is appended to OUT; every
# run is wrapped in `timeout`.
#
#   OXIROUTE_BIN=... R2TOOL_BIN=... gate.sh OUT.jsonl [REPS]
set -eu
out=$1
reps=${2:-3}
here=$(cd -- "$(dirname -- "$0")/.." && pwd)
impaired() {
  label=$1
  shift
  timeout -k 5 400 nice -n 10 python3 "$here/h3_impair.py" download --server oxiroute --size-mib 10 --runs 3 \
    --server-cpus 22-23 --client-cpus 2-3 --label "$label" --out "$out" "$@" >/dev/null || echo "FAILED $label"
}
lan_bulk() {
  label=$1
  shift
  timeout -k 5 300 nice -n 10 python3 "$here/h3_impair.py" download --server oxiroute --size-mib 48 --runs 5 \
    --server-cpus 8-9 --client-cpus 12-15 --label "$label" --out "$out" "$@" >/dev/null || echo "FAILED $label"
}
lan_small() {
  label=$1
  shift
  timeout -k 5 200 nice -n 10 python3 "$here/h3_impair.py" small --server oxiroute --size-mib 1 --requests 200000 \
    --warmup 5000 --server-cpus 8-9 --client-cpus 12-15 --label "$label" --out "$out" "$@" >/dev/null ||
    echo "FAILED $label"
}
no_offload=http3_segmentation_offload=false
for rep in $(seq "$reps"); do
  for cc in cubic bbr; do
    setting="http3_congestion_control=\"$cc\""
    impaired "wan40-loss0.5-$cc-r$rep" --rtt-ms 40 --loss-pct 0.5 --set "$setting" --set "$no_offload"
    impaired "wan100-loss1-$cc-r$rep" --rtt-ms 100 --loss-pct 1 --set "$setting" --set "$no_offload"
    impaired "wan40-loss0-$cc-r$rep" --rtt-ms 40 --set "$setting" --set "$no_offload"
    lan_bulk "lan-bulk-$cc-r$rep" --set "$setting"
    lan_small "lan-small-$cc-r$rep" --set "$setting"
  done
done
echo GATEDONE
