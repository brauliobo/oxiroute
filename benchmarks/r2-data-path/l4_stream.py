#!/usr/bin/env python3
"""Bulk TCP relay cost per byte moved (user-space instructions and CPU time per MiB), through the L4 relay.

`run.py` measures requests; a request-bound lane parks the relay between messages, so its instructions per request
depend on how busy the host is. Here a few connections stream data through the relay to the r2tool echo upstream
without pause (each sends N MiB while reading the echo), so the relay is never idle and the cost per byte is stable.
One invocation holds the shared bench lock for a few seconds. Usage:
  OXIROUTE_BIN=... R2TOOL_BIN=... l4_stream.py [--conns 4] [--mib 128] [--chunk-kib 1024] [--reps 3] [--proxy-env K=V]
"""

import argparse
import os
import signal
import socket
import statistics
import subprocess
import sys
import threading
import time
import types
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run  # noqa: E402


def stream(port, mib, chunk_kib):
    """Send `mib` MiB and read the same amount back; returns the bytes read."""
    chunk = b"\xa5" * (chunk_kib * 1024)
    total = mib * 1024 * 1024
    sock = socket.create_connection(("127.0.0.1", port))
    received = [0]

    def sender():
        sent = 0
        while sent < total:
            sock.sendall(chunk[: min(len(chunk), total - sent)])
            sent += len(chunk)
        sock.shutdown(socket.SHUT_WR)

    thread = threading.Thread(target=sender)
    thread.start()
    buffer = bytearray(1 << 20)
    while received[0] < total:
        count = sock.recv_into(buffer)
        if count == 0:
            break
        received[0] += count
    thread.join()
    sock.close()
    return received[0]


def cpu_ticks(pid):
    rows = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return int(rows[11]), int(rows[12])


def switches(pid):
    text = "".join(path.read_text() for path in Path(f"/proc/{pid}/task").glob("*/status"))
    return sum(int(line.split()[1]) for line in text.splitlines() if line.startswith("voluntary_ctxt_switches"))


def one_rep(args, env):
    pid, port = env.proxy.pid, run.PROXY_PORT + 1
    csv = env.work / "perf.csv"
    perf = subprocess.Popen(["env", "LC_ALL=C", "perf", "stat", "-x,", "-e", "instructions:u,cycles:u", "-p", str(pid),
                             "-o", str(csv)], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    time.sleep(0.3)
    ticks, vol, started = cpu_ticks(pid), switches(pid), time.monotonic()
    moved = [0] * args.conns
    workers = [threading.Thread(target=lambda i=i: moved.__setitem__(i, stream(port, args.mib, args.chunk_kib)))
               for i in range(args.conns)]
    [worker.start() for worker in workers]
    [worker.join() for worker in workers]
    elapsed = time.monotonic() - started
    after, vol_after = cpu_ticks(pid), switches(pid)
    perf.send_signal(signal.SIGINT)
    perf.wait()
    counters = run.parse_perf(csv)
    hz = os.sysconf("SC_CLK_TCK")
    mib = sum(moved) / (1 << 20) * 2  # the relay carries the payload in both directions
    return {"mib": mib, "instr_per_mib": counters["instructions"] / mib, "ucycles_per_mib": counters["cycles"] / mib,
            "user_ms_per_gib": (after[0] - ticks[0]) * 1000 / hz / mib * 1024,
            "sys_ms_per_gib": (after[1] - ticks[1]) * 1000 / hz / mib * 1024, "mib_per_s": mib / elapsed,
            "vctxsw_per_mib": (vol_after - vol) / mib}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--conns", type=int, default=4)
    parser.add_argument("--mib", type=int, default=128, help="MiB each connection sends (and reads back)")
    parser.add_argument("--chunk-kib", type=int, default=1024)
    parser.add_argument("--reps", type=int, default=3)
    parser.add_argument("--proxy-env", action="append", default=[])
    cli = parser.parse_args()
    args = types.SimpleNamespace(bin=os.environ["OXIROUTE_BIN"], tool=os.environ["R2TOOL_BIN"], tmp="/var/tmp",
                                 proxy_env=cli.proxy_env, conns=cli.conns, mib=cli.mib, chunk_kib=cli.chunk_kib)
    lane = run.LANES["l4_echo_1k"]
    with run.bench_lock():
        run.pick_ports(lane)
        env = run.Env(args, lane)
        try:
            env.start()
            rows = [one_rep(args, env) for _ in range(cli.reps)]
        finally:
            env.close()
    keys = rows[0].keys()
    median = {key: statistics.median(row[key] for row in rows) for key in keys}
    spread = {key: 100 * (max(row[key] for row in rows) - min(row[key] for row in rows)) / (median[key] or 1) for key in keys}
    print(" ".join(f"{key}={median[key]:.1f}" for key in keys), "spread_instr_pct=%.1f" % spread["instr_per_mib"])


if __name__ == "__main__":
    main()
