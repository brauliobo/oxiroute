#!/usr/bin/env python3
"""Summarise a perf.data of instructions:u samples by component and by symbol.

Usage: profile_summary.py FILE.perf.data [TOP]
Components: oxiroute (this repo), pingora, http-types (http/httparse/bytes), tokio, libc, vdso clock, openssl, other.
"""

import os
import re
import subprocess
import sys
from collections import Counter

COMPONENTS = [
    ("oxiroute", re.compile(r"oxiroute_server|oxiroute_cache|oxiroute_config|oxiroute_acme")),
    ("pingora", re.compile(r"pingora")),
    ("http-types", re.compile(r"\bhttp::|httparse|bytes::|http_body")),
    ("tokio", re.compile(r"tokio::|mio::")),
    ("serde_json", re.compile(r"serde_json|serde::")),
]


def component(dso, symbol):
    if "vdso" in dso or "clock_gettime" in symbol:
        return "clock_gettime"
    if "libcrypto" in dso or "libssl" in dso:
        return "openssl"
    if "libc.so" in dso:
        return "libc"
    if "ld-linux" in dso:
        return "ld.so"
    if dso.startswith("[kernel") or "[k]" in symbol:
        return "kernel"
    for name, pattern in COMPONENTS:
        if pattern.search(symbol):
            return name
    return "other-rust (std/alloc/core/deps)"


def main():
    data = sys.argv[1]
    top = int(sys.argv[2]) if len(sys.argv) > 2 else 25
    report = subprocess.run(
        ["perf", "report", "--stdio", "-i", data, "--no-children", "-g", "none", "--sort", "dso,sym"],
        capture_output=True, text=True, env={**os.environ, "LC_ALL": "C"}).stdout
    totals, symbols = Counter(), []
    for line in report.splitlines():
        match = re.match(r"\s*([\d.]+)%\s+(\S+)\s+\[(.)\]\s+(.*)$", line)
        if not match:
            continue
        percent, dso, _, symbol = float(match.group(1)), match.group(2), match.group(3), match.group(4)
        totals[component(dso, symbol)] += percent
        symbols.append((percent, dso, symbol))
    print(f"== {os.path.basename(data)} (share of sampled user instructions)")
    for name, percent in totals.most_common():
        print(f"{percent:6.2f}%  {name}")
    print("-- top symbols")
    for percent, dso, symbol in sorted(symbols, reverse=True)[:top]:
        print(f"{percent:6.2f}%  {dso[:18]:18}  {symbol[:150]}")


if __name__ == "__main__":
    main()
