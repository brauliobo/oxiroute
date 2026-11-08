#!/usr/bin/env python3
"""Print results.json of run.py as a compact table, optionally against a second results file.

Usage: table.py RESULTS.json [BEFORE.json]   (with BEFORE, adds percentage deltas of instructions/request)
"""

import json
import sys

COLUMNS = ["instr_per_req", "instr_spread_pct", "cycles_per_req", "vctxsw_per_req", "rps", "rps_spread_pct", "p50_us",
           "p99_us", "p999_us", "rss_kb", "mallocs_per_req", "alloc_bytes_per_req", "syscalls_per_req"]


def load(path):
    return {row["lane"]: row for row in json.load(open(path))}


def main():
    rows = load(sys.argv[1])
    before = load(sys.argv[2]) if len(sys.argv) > 2 else {}
    columns = [name for name in COLUMNS if any(name in row for row in rows.values())]
    print("lane".ljust(30) + "".join(name.replace("_per_req", "/r").rjust(14) for name in columns) + ("  d_instr%" if before else ""))
    for name, row in rows.items():
        line = name.ljust(30) + "".join(str(row.get(column, "")).rjust(14) for column in columns)
        if name in before and "instr_per_req" in row and "instr_per_req" in before[name]:
            line += f"  {100.0 * (row['instr_per_req'] / before[name]['instr_per_req'] - 1):+.1f}"
        print(line)


if __name__ == "__main__":
    main()
