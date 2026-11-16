#!/bin/sh
# Runs the fairness matrix: round trips 20 and 80 ms, buffers of 0.5, 1 and 4 BDP, the three flow pairs and each flow
# alone, RUNS repetitions per cell, appending one JSON record per run to OUT. Every run is wrapped in `timeout`.
#
#   OXIROUTE_BIN=/path/to/oxiroute matrix.sh OUT.jsonl [RUNS] [CPUS]
set -eu
out=$1
runs=${2:-3}
cpus=${3:-16-21}
here=$(cd -- "$(dirname -- "$0")" && pwd)
for rtt in 20 80; do
  for buffer in 0.5 1 4; do
    for flows in bbr,cubic bbr,tcp cubic,tcp bbr cubic tcp; do
      for run in $(seq "$runs"); do
        label="rtt${rtt}-buf${buffer}-${flows}-r${run}"
        grep -q "\"label\": \"$label\"" "$out" 2>/dev/null && continue
        timeout -k 5 150 nice -n 10 python3 "$here/lane.py" --flows "$flows" --rtt-ms "$rtt" --buffer-bdp "$buffer" \
          --cpus "$cpus" --warmup 15 --measure 25 --label "$label" --out "$out" >/dev/null || echo "FAILED $label"
      done
    done
  done
done
echo MATRIXDONE
