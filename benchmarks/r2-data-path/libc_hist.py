#!/usr/bin/env python3
"""Calls of memcpy, memmove, memset, memcmp, bcmp, memchr and strlen per request of a lane, by size class.

Usage: libc_hist.py BIN LANE [--requests N]

Runs the lane's proxy with tool/libc_hist.c preloaded and prints, per function, the calls per request and the share of
calls by size (0..16, 17..64, 65..256, larger). The binary must link libc dynamically.
"""

import argparse
import collections
import importlib.util
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("r2run", HERE / "run.py")
r2 = importlib.util.module_from_spec(spec)
sys.modules["r2run"] = r2
spec.loader.exec_module(r2)
CLASSES = [(16, "0-16"), (64, "17-64"), (256, "65-256"), (4096, "257-4K"), (1 << 40, ">4K")]


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("bin")
    parser.add_argument("lane")
    parser.add_argument("--tool", default=str(HERE / "tool/target/release/r2tool"))
    parser.add_argument("--tmp", default="/var/tmp")
    parser.add_argument("--requests", type=int, default=4000)
    parser.add_argument("--warmup", type=int, default=500)
    args = parser.parse_args()
    shim = Path(args.tmp) / f"r2-libc-hist-{r2.os.getuid()}.so"
    subprocess.run(["gcc", "-O2", "-shared", "-fPIC", "-o", str(shim), str(HERE / "tool/libc_hist.c"), "-ldl"], check=True)
    lane = r2.LANES[args.lane]
    run_args = argparse.Namespace(bin=args.bin, tool=args.tool, tmp=args.tmp, proxy_env=[])
    out = Path(args.tmp) / f"r2-libc-hist-{r2.os.getpid()}.txt"
    with r2.bench_lock():
        r2.pick_ports(lane)
        env = r2.Env(run_args, lane)
        try:
            env.start(proxy_env={"LD_PRELOAD": str(shim), "LIBCHIST_FILE": str(out)})
            pid = env.proxy.pid
            hooks = [("hook-start", f"kill -USR1 {pid}"), ("hook-stop", f"kill -USR1 {pid}")]
            result = subprocess.run(env.load_command(args.requests, args.warmup, hooks), capture_output=True, text=True,
                                    timeout=r2.LANE_TIMEOUT)
            time.sleep(1)
        finally:
            env.close()
    requests = int(r2.parse_kv(result.stdout)["ok"])
    table = collections.defaultdict(lambda: collections.defaultdict(int))
    for line in out.read_text().splitlines():
        name, size, count = line.split()
        size = int(size)
        label = next(label for limit, label in CLASSES if size <= limit)
        table[name][label] += int(count)
    out.unlink()
    print(f"requests={requests} lane={args.lane}")
    print(f"{'function':8} {'calls/req':>9}  " + " ".join(f"{label:>7}" for _, label in CLASSES))
    for name, row in table.items():
        total = sum(row.values())
        print(f"{name:8} {total / requests:9.1f}  " + " ".join(f"{row[label] / requests:7.1f}" for _, label in CLASSES))


main()
