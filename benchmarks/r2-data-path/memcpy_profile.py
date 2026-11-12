#!/usr/bin/env python3
"""Large memcpy/memmove calls per request of a lane, attributed to the caller with the binary's line tables.

Usage: memcpy_profile.py BIN LANE [--requests N] [--top N]

Builds tool/memcpy_profile.c, runs the lane's proxy with it preloaded, counts the copies of 48 bytes or more between
the start and the end of the measured requests and prints the biggest callers (addr2line, inlined frames included).
The binary needs line tables (CARGO_PROFILE_RELEASE_DEBUG=line-tables-only) and must link libc dynamically.
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


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("bin")
    parser.add_argument("lane")
    parser.add_argument("--tool", default=str(HERE / "tool/target/release/r2tool"))
    parser.add_argument("--tmp", default="/var/tmp")
    parser.add_argument("--requests", type=int, default=4000)
    parser.add_argument("--warmup", type=int, default=500)
    parser.add_argument("--top", type=int, default=25)
    args = parser.parse_args()
    shim = Path(args.tmp) / f"r2-memcpy-profile-{r2.os.getuid()}.so"
    subprocess.run(["gcc", "-O2", "-shared", "-fPIC", "-o", str(shim), str(HERE / "tool/memcpy_profile.c"), "-ldl"], check=True)
    lane = r2.LANES[args.lane]
    run_args = argparse.Namespace(bin=args.bin, tool=args.tool, tmp=args.tmp, proxy_env=[], classic=False, baseline=False, tls_backend="openssl")
    out = Path(args.tmp) / f"r2-memcpy-{r2.os.getpid()}.txt"
    with r2.bench_lock():
        r2.pick_ports(lane)
        env = r2.Env(run_args, lane)
        try:
            env.start(proxy_env={"LD_PRELOAD": str(shim), "MEMPROF_FILE": str(out)})
            pid = env.proxy.pid
            hooks = [("hook-start", f"kill -USR1 {pid}"), ("hook-stop", f"kill -USR1 {pid}")]
            result = subprocess.run(env.load_command(args.requests, args.warmup, hooks), capture_output=True, text=True,
                                    timeout=r2.LANE_TIMEOUT)
            time.sleep(1)
            maps = Path(f"/proc/{pid}/maps").read_text()
            base = min(int(line.split("-")[0], 16) for line in maps.splitlines() if args.bin in line)
        finally:
            env.close()
    requests = int(r2.parse_kv(result.stdout)["ok"])
    rows = []
    stacks = []
    for line in out.read_text().splitlines():
        ret, size, count, *frames = line.split()
        if int(size) == 0:
            stacks.append((int(count), [int(frame, 16) - base for frame in frames]))
        else:
            rows.append((int(ret, 16) - base, int(size), int(count)))
    out.unlink()
    by_caller = collections.defaultdict(lambda: [0, 0])
    for ret, size, count in rows:
        by_caller[ret][0] += count
        by_caller[ret][1] += count * size
    calls = sum(count for _, _, count in rows) / requests
    copied = sum(size * count for _, size, count in rows) / requests
    print(f"requests={requests} copies_of_48B_or_more/req={calls:.1f} bytes/req={copied:.0f} "
          f"clock_gettime/req={sum(count for count, _ in stacks) / requests:.1f}")
    report(args, by_caller, rows, requests, "B/req", args.top)
    print("-- clock_gettime call chains (innermost first, std frames skipped)")
    for count, frames in sorted(stacks, key=lambda item: -item[0])[: args.top]:
        names = subprocess.run(["addr2line", "-e", args.bin, "-f", "-C", "-i", "-p"] + [hex(frame - 1) for frame in frames],
                               capture_output=True, text=True, env={"LC_ALL": "C"}).stdout.splitlines()
        chain = [name.replace(" (inlined by) ", "").split(" at ")[0][:90] for name in names if "/rustc/" not in name
                 or "inlined" not in name]
        print(f"{count / requests:5.2f}/req  " + "  <-  ".join(chain[:4]))
    return 0


def report(args, by_caller, rows, requests, unit, limit):
    top = sorted(by_caller, key=lambda address: -by_caller[address][1])[:limit]
    if not top:
        return
    lines = subprocess.run(["addr2line", "-e", args.bin, "-f", "-C", "-i", "-p"] + [hex(address - 1) for address in top],
                           capture_output=True, text=True, env={"LC_ALL": "C"}).stdout.splitlines()
    frames = []
    for line in lines:
        if line.startswith(" (inlined by)") and frames:
            frames[-1].append(line.strip())
        else:
            frames.append([line])
    for address, frame in zip(top, frames):
        count, size = by_caller[address]
        print(f"{count / requests:6.2f} calls {size / requests:8.0f} {unit}  {frame[0][:170]}")
        for extra in frame[1:4]:
            print(f"{'':27}{extra[:150]}")


if __name__ == "__main__":
    sys.exit(main())
