#!/usr/bin/env python3
"""Share of sampled instructions (or cycles) by source file, from a flat `perf record` of a PIE binary with line tables.

Usage: source_profile.py PERF.DATA BINARY [TOP]

Every sample of the binary is resolved with `llvm-addr2line -i` to its innermost inlined frame outside the standard
library (`/rustc/...`); samples in other shared objects are grouped by object and symbol. Prints the shares by source
file and by crate directory, so a flat profile of a binary built without LTO reads as a list of stages.
"""

import os
import re
import subprocess
import sys
from collections import Counter

ENV = {**os.environ, "LC_ALL": "C"}


def run(*command, **kwargs):
    return subprocess.run(command, capture_output=True, text=True, env=ENV, **kwargs).stdout


def segments(binary):
    """(file offset, virtual address, size) of the loadable segments."""
    found = []
    for line in run("readelf", "-lW", binary).splitlines():
        parts = line.split()
        if parts[:1] == ["LOAD"]:
            found.append((int(parts[1], 16), int(parts[2], 16), int(parts[4], 16)))
    return found


def mapping(data, binary):
    """The (start, length, page offset) mappings of the binary in the recording."""
    maps = []
    for line in run("perf", "script", "-i", data, "--show-mmap-events", "-F", "event").splitlines():
        found = re.search(r"PERF_RECORD_MMAP2? .*?\[(0x[0-9a-f]+)\((0x[0-9a-f]+)\) @ (0x[0-9a-f]+|\d+).*?" + re.escape(binary), line)
        if found:
            maps.append(tuple(int(value, 16) if value.startswith("0x") else int(value) for value in found.groups()))
    return maps


def file_address(ip, maps, loads):
    for start, length, offset in maps:
        if start <= ip < start + length:
            position = ip - start + offset
            for file_offset, vaddr, size in loads:
                if file_offset <= position < file_offset + size:
                    return position - file_offset + vaddr
    return None


def owners(binary, addresses):
    """The innermost non-std source file of every address."""
    found = {}
    for chunk in range(0, len(addresses), 20000):
        part = addresses[chunk:chunk + 20000]
        text = run("llvm-addr2line", "-a", "-f", "-i", "-C", "-e", binary, *[hex(a) for a in part])
        current, frames = None, []
        for line in text.splitlines() + ["0xend"]:
            if line.startswith("0x"):
                if current is not None:
                    found[current] = frames
                current = int(line, 16) if line != "0xend" else None
                frames = []
            elif ":" in line and not line.startswith(" ") and "/" in line:
                frames.append(line.rsplit(":", 1)[0])
    return found


def pick(frames):
    for path in frames:
        if not path.startswith("/rustc/"):
            return path
    return "std/core/alloc"


def short(path):
    for marker in ("/crates/", "/vendor/"):
        if marker in path:
            return path.split(marker, 1)[1] if marker == "/crates/" else "vendor/" + path.split(marker, 1)[1]
    match = re.search(r"/registry/src/[^/]+/(.+)$", path)
    return match.group(1) if match else path


def main():
    data, binary = sys.argv[1], os.path.realpath(sys.argv[2])
    top = int(sys.argv[3]) if len(sys.argv) > 3 else 60
    maps, loads = mapping(data, binary), segments(binary)
    samples = Counter()
    others = Counter()
    for line in run("perf", "script", "-i", data, "-F", "ip,sym,dso").splitlines():
        fields = line.split(None, 1)
        if len(fields) < 2:
            continue
        ip, rest = int(fields[0], 16), fields[1]
        address = file_address(ip, maps, loads) if binary in rest else None
        if address is None:
            others[rest.strip()[:80]] += 1
        else:
            samples[address] += 1
    resolved = owners(binary, sorted(samples))
    by_file, by_crate = Counter(), Counter()
    for address, count in samples.items():
        path = short(pick(resolved.get(address, [])))
        by_file[path] += count
        by_crate["/".join(path.split("/")[:2]) if "/" in path else path] += count
    total = sum(samples.values()) + sum(others.values())
    print(f"== {os.path.basename(data)}: {total} samples")
    for title, counter in (("by crate directory", by_crate), ("by file", by_file), ("other objects", others)):
        print(f"-- {title}")
        for name, count in counter.most_common(top):
            print(f"{100 * count / total:6.2f}%  {name}")


if __name__ == "__main__":
    main()
