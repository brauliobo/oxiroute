#!/usr/bin/env python3
"""Medians per cell of a fairness matrix, as a Markdown table.

Usage: summarize.py RESULTS.jsonl

One row per (rtt, buffer, flows) cell with the median over its runs: Jain's index, each flow's goodput share, its loss
(an HTTP/3 flow: packets that entered the bottleneck queue and did not leave it; a TCP flow: retransmitted segments) and
its server CPU per MiB delivered, and the mean queue occupancy as a share of the buffer. The CPU of a flow that
delivered under 1% of the link is not meaningful (the counters tick at 10 ms) and is printed as a dash."""

import json
import statistics
import sys
from collections import defaultdict

STARVED_SHARE = 0.01


def cell_key(record):
    rtt, buffer, flows = record["label"].rsplit("-r", 1)[0].split("-")
    return int(rtt.removeprefix("rtt")), float(buffer.removeprefix("buf")), flows


def median(values):
    return statistics.median(values)


def loss_of(flow):
    return flow["tcp_retrans_pct"] if "tcp_retrans_pct" in flow else flow["path_loss_pct"]


def flow_cell(name, flows):
    share = median(f["share"] for f in flows)
    cpu = "-" if share < STARVED_SHARE else f"{median(f['cpu_ms_per_mib'] for f in flows):.1f}"
    return f"{name} {100 * share:.1f}% / loss {median(loss_of(f) for f in flows):.1f}% / {cpu} ms/MiB"


def row(key, records):
    rtt, buffer, flows = key
    queue = median(100 * r["queue"]["mean_bytes"] / r["queue"]["limit_bytes"] for r in records)
    cells = " ; ".join(flow_cell(name, [r["flows"][name] for r in records]) for name in records[0]["flows"])
    jain = median(r["jain"] for r in records)
    return f"| {rtt} | {buffer} | {flows.replace(',', ' + ')} | {jain:.3f} | {cells} | {queue:.0f}% |"


def main():
    cells = defaultdict(list)
    for line in open(sys.argv[1], encoding="utf-8"):
        record = json.loads(line)
        cells[cell_key(record)].append(record)
    print("| RTT ms | buffer BDP | flows | Jain | per flow: goodput share / loss / CPU | queue mean |")
    print("|---|---|---|---|---|---|")
    for key in sorted(cells):
        print(row(key, cells[key]))


if __name__ == "__main__":
    main()
