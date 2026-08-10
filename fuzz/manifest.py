#!/usr/bin/env python3

import json
import pathlib
import re
import sys


def main():
    if len(sys.argv) != 2:
        raise SystemExit(f"usage: {sys.argv[0]} MANIFEST")
    manifest = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
    if not isinstance(manifest, dict) or manifest.get("schema") != "oxiroute.fuzz-targets.v1":
        raise SystemExit("unsupported fuzz target manifest schema")
    targets = manifest.get("targets")
    if not isinstance(targets, list) or not targets:
        raise SystemExit("fuzz target manifest must contain targets")
    names = set()
    for target in targets:
        if not isinstance(target, dict) or set(target) != {"name", "max_input_bytes"}:
            raise SystemExit("fuzz target manifest entry has unexpected fields")
        name = target["name"]
        maximum = target["max_input_bytes"]
        if not isinstance(name, str) or re.fullmatch(r"[a-z][a-z0-9_]*", name) is None:
            raise SystemExit("fuzz target manifest entry has an invalid name")
        if name in names:
            raise SystemExit(f"duplicate fuzz target: {name}")
        if not isinstance(maximum, int) or isinstance(maximum, bool) or maximum <= 0:
            raise SystemExit(f"fuzz target {name} has an invalid input bound")
        names.add(name)
        print(f"{name}:{maximum}")


if __name__ == "__main__":
    main()
