#!/usr/bin/env python3
"""Print results of run.py as a table.

Usage: table.py RESULTS.json [BEFORE.json]
           a compact table of one results file; with BEFORE it adds percentage deltas of instructions/request.
       table.py --markdown OUT_DIR LANE [LANE...]
           the instruction-count comparison of `compare.sh` as a markdown table: one row per lane, a column for the
           older release, the new binary with the lane off and the new binary, the change against the older release
           and the spread of the repetitions.
"""

import json
import sys
from pathlib import Path

COLUMNS = ["instr_per_req", "instr_spread_pct", "cycles_per_req", "vctxsw_per_req", "rps", "rps_spread_pct", "p50_us",
           "p99_us", "p999_us", "rss_kb", "mallocs_per_req", "alloc_bytes_per_req", "syscalls_per_req"]
VARIANTS = ("base", "classic", "new")  # the directories `compare.sh` writes: <variant>_<lane>/results.json


def load(path):
    return {row["lane"]: row for row in json.load(open(path))}


def lane_row(out_dir, variant, lane):
    path = Path(out_dir) / f"{variant}_{lane}" / "results.json"
    rows = json.loads(path.read_text()) if path.exists() else []
    row = rows[0] if rows else {}
    return row if row.get("instr_per_req") else None


def instructions(row):
    return "" if row is None else f"{int(row['instr_per_req']):,}"


def change(base, new):
    return "" if base is None or new is None else f"{(new['instr_per_req'] / base['instr_per_req'] - 1) * 100:+.1f}%"


def markdown(out_dir, lanes):
    lines = ["| lane | older release | new, lane off | new | change | spread older / new |",
             "| --- | ---: | ---: | ---: | ---: | ---: |"]
    for lane in lanes:
        base, classic, new = (lane_row(out_dir, variant, lane) for variant in VARIANTS)
        spread = " / ".join("" if row is None else f"{row.get('instr_spread_pct', 0):.1f}%" for row in (base, new))
        lines.append(f"| `{lane}` | {instructions(base)} | {instructions(classic)} | {instructions(new)} "
                     f"| {change(base, new)} | {spread} |")
    return "\n".join(lines)


def compact(path, before_path):
    rows = load(path)
    before = load(before_path) if before_path else {}
    columns = [name for name in COLUMNS if any(name in row for row in rows.values())]
    print("lane".ljust(30) + "".join(name.replace("_per_req", "/r").rjust(14) for name in columns) + ("  d_instr%" if before else ""))
    for name, row in rows.items():
        line = name.ljust(30) + "".join(str(row.get(column, "")).rjust(14) for column in columns)
        if name in before and "instr_per_req" in row and "instr_per_req" in before[name]:
            line += f"  {100.0 * (row['instr_per_req'] / before[name]['instr_per_req'] - 1):+.1f}"
        print(line)


def main(argv):
    if len(argv) > 3 and argv[1] == "--markdown":
        print(markdown(argv[2], argv[3:]))
    elif len(argv) in (2, 3):
        compact(argv[1], argv[2] if len(argv) > 2 else None)
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv)
