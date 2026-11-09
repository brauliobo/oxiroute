#!/usr/bin/env python3
"""R2 data-path harness: per-request cost and scaling of the OxiRoute proxy path.

Primary metric: user-space instructions per request (`perf stat -e instructions:u`, attached to the
proxy for exactly the measured request window, idle baseline subtracted). Secondary: cycles, context
switches, wall-clock rps and latency percentiles, RSS, syscalls per request (strace slope) and
allocations per request (callgrind malloc call counts). See README.md.
"""

import argparse
import fcntl
import json
import os
import re
import resource
import shutil
import signal
import socket
import statistics
import subprocess
import sys
import tempfile
import time
from contextlib import contextmanager
from dataclasses import dataclass, field
from pathlib import Path

HERE = Path(__file__).resolve().parent
LOCK_FILE = "/tmp/oxiroute-bench.lock"
LANE_TIMEOUT = 600  # seconds; a hung load run must never hold the shared bench lock
SIZES = {"b0": 0, "b1k": 1024, "b64k": 65536, "b1m": 1048576}

PORT_FIRST, PORT_LAST, PORT_SPAN = int(os.environ.get("R2_PORT_BASE", "20300")), 20499, 40
UPSTREAM_PORT = ECHO_PORT = PROXY_PORT = 0
PROXY_CPUS = os.environ.get("R2_PROXY_CPUS", "8-11")
UPSTREAM_CPUS = os.environ.get("R2_UPSTREAM_CPUS", "12-13")
LOAD_CPUS = os.environ.get("R2_LOAD_CPUS", "14-15")
FEATURES_6_0 = ["access_log", "timeouts", "retry", "queue", "capacity", "passive", "breaker", "outlier"]


@dataclass
class Lane:
    name: str
    load: list = field(default_factory=list)
    features: list = field(default_factory=list)
    requests: int = 20000
    warmup: int = 1000
    listeners: int = 1
    workers: int = 0  # `listener_workers` of the generated configuration; 0 leaves the setting out
    servers: int = 1
    target_offset: int = 0  # proxy port offset the load goes to (forward listener: 3, TCP listener: 1)
    note: str = ""


def lane(name, load=(), features=(), **kwargs):
    return Lane(name, list(load), list(features), **kwargs)


def build_lanes():
    lanes = []
    add = lanes.append
    base = ["--conns", "16"]
    for body in ("b0", "b1k", "b64k"):
        add(lane(f"h1_{body}", ["--path", f"/{body}", "--expect-len", str(SIZES[body])] + base))
    add(lane("h1_b1m", ["--path", "/b1m", "--expect-len", str(SIZES["b1m"])] + base, requests=2000, warmup=100))
    add(lane("h1_stream", ["--path", "/stream"] + base, requests=5000, warmup=200))
    add(lane("h1_slow", ["--path", "/slow"] + base, requests=600, warmup=48))
    for conns, requests in ((1, 10000), (16, 20000), (256, 30000), (512, 30000), (1024, 30000), (2048, 40000), (4096, 40000)):
        add(lane(f"h1_b1k_c{conns}", ["--path", "/b1k", "--conns", str(conns), "--expect-len", "1024"],
                 requests=requests, warmup=max(conns * 2, 1000)))
    add(lane("h1_newconn_b1k", ["--path", "/b1k", "--new-conn", "--conns", "16", "--expect-len", "1024"],
             requests=10000, warmup=500))
    for header in (2048, 8192):
        add(lane(f"h1_hdr{header}", ["--path", "/b1k", "--hdr-bytes", str(header), "--expect-len", "1024"] + base,
                 requests=10000))
    for size in (4096, 65536, 1048576):
        requests = 20000 if size < 65536 else 5000 if size == 65536 else 600
        add(lane(f"h1_post{size // 1024}k", ["--method", "POST", "--path", "/sink", "--body-bytes", str(size)] + base,
                 requests=requests, warmup=min(requests // 10, 200)))
    add(lane("h1_post64k_reqbuf", ["--method", "POST", "--path", "/sink", "--body-bytes", "65536"] + base,
             features=["reqbuf"], requests=5000, warmup=200))
    add(lane("h1_post64k_retrybuf", ["--method", "POST", "--path", "/sink", "--body-bytes", "65536"] + base,
             features=["reqbuf", "retry_buffered"], requests=5000, warmup=200))
    # Per-feature A/B at the same workload: 16 keep-alive connections, 1 KiB body.
    work = ["--path", "/b1k", "--expect-len", "1024"] + base
    add(lane("f_base", work))
    add(lane("f_fixed", ["--path", "/fixed"] + base, features=["fixed"]))
    for feature in FEATURES_6_0 + ["nginx_host"]:
        add(lane(f"f_{feature}", work, features=[feature]))
    add(lane("f_all_6_0", work, features=FEATURES_6_0))
    add(lane("gzip_html64k", ["--path", "/html64k", "--extra-header", "accept-encoding: gzip"] + base,
             features=["gzip"], requests=3000, warmup=100))
    add(lane("nogzip_html64k", ["--path", "/html64k"] + base, features=["gzip"], requests=3000, warmup=100))
    add(lane("cache_hit_b1k", work, features=["cache"]))
    add(lane("cache_hit_b64k", ["--path", "/b64k", "--expect-len", "65536"] + base, features=["cache"], requests=5000))
    for name, size in (("static_f1k", 1024), ("static_f64k", 65536), ("static_f1m", 1048576)):
        add(lane(name, ["--path", f"/static/{name[7:]}.bin", "--expect-len", str(size)] + base, features=["static"],
                 requests=20000 if size < 65536 else 5000 if size == 65536 else 1000))
    add(lane("ws_echo_1k", ["--proto", "ws", "--conns", "16", "--msg-bytes", "1024"]))
    add(lane("l4_echo_1k", ["--proto", "tcp", "--conns", "16", "--msg-bytes", "1024"], features=["l4"]))
    # TLS termination (OpenSSL via Pingora).
    tls = ["--proto", "h1tls", "--path", "/b1k", "--expect-len", "1024"]
    add(lane("tls_h1_keepalive", tls + base, features=["tls"]))
    for name, features, extra in (
        ("tls_hs_full_tls13", ["tls"], ["--tls-version", "1.3"]),
        ("tls_hs_full_tls12", ["tls"], ["--tls-version", "1.2"]),
        ("tls_hs_full_rsa2048", ["tls", "tls_rsa"], ["--tls-version", "1.3"]),
        ("tls_hs_resume_cache_tls12", ["tls", "tls_cache"], ["--tls-version", "1.2", "--resume"]),
        ("tls_hs_resume_cache_tls13", ["tls", "tls_cache"], ["--tls-version", "1.3", "--resume"]),
        ("tls_hs_resume_tickets_tls13", ["tls", "tls_tickets"], ["--tls-version", "1.3", "--resume"]),
        ("tls_hs_resume_client_only", ["tls"], ["--tls-version", "1.3", "--resume"]),
    ):
        add(lane(name, tls + ["--new-conn", "--conns", "16"] + extra, features=features, requests=3000, warmup=200))
    add(lane("tls_h1_b1m", ["--proto", "h1tls", "--path", "/b1m", "--expect-len", str(SIZES["b1m"])] + base,
             features=["tls"], requests=1500, warmup=100))
    add(lane("h2_b1k_c4s16", ["--proto", "h2tls", "--path", "/b1k", "--conns", "4", "--streams", "16",
                              "--expect-len", "1024"], features=["tls"], requests=20000))
    add(lane("h2_b64k_c4s16", ["--proto", "h2tls", "--path", "/b64k", "--conns", "4", "--streams", "16",
                               "--expect-len", "65536"], features=["tls"], requests=5000, warmup=200))
    add(lane("h2_b0_c1s1", ["--proto", "h2tls", "--path", "/b0", "--conns", "1", "--streams", "1",
                            "--expect-len", "0"], features=["tls"], requests=10000))
    # Scaling over listeners (each listener is one Pingora service with one worker thread).
    for listeners in (1, 2, 4, 8):
        add(lane(f"scale_l{listeners}", ["--path", "/b1k", "--expect-len", "1024", "--conns", str(32 * listeners)],
                 requests=20000 * listeners, warmup=2000, listeners=listeners))
        add(lane(f"scale_all_l{listeners}", ["--path", "/b1k", "--expect-len", "1024", "--conns", str(32 * listeners)],
                 features=FEATURES_6_0, requests=20000 * listeners, warmup=2000, listeners=listeners))
    # Scaling over worker threads of one listener (`listener_workers`); needs workers + 1 proxy CPUs.
    for workers in (1, 2, 4, 8):
        for name, features in (("scale_w", []), ("scale_all_w", FEATURES_6_0)):
            add(lane(f"{name}{workers}", ["--path", "/b1k", "--expect-len", "1024", "--conns", str(32 * workers)],
                     features=features, requests=20000 * workers, warmup=2000, workers=workers))
    for algorithm in ("lc", "wrr"):
        for servers in (8, 64, 256):
            add(lane(f"pool_{algorithm}_s{servers}", work, features=[algorithm], servers=servers))
    for feature in FEATURES_6_0:
        add(lane(f"scale4_{feature}", ["--path", "/b1k", "--expect-len", "1024", "--conns", "128"], features=[feature],
                 requests=80000, warmup=4000, listeners=4))
    add(lane("fwd_h1_b1k", ["--path", "http://127.0.0.1:{UP}/b1k", "--expect-len", "1024"] + base, features=["forward"],
             target_offset=3))
    add(lane("pool_s2", work, servers=2))
    add(lane("pool_s64", work, servers=64))
    add(lane("pool_s64_passive", work, features=["passive", "breaker", "outlier"], servers=64))
    add(lane("pool_s1_passive", work, features=["passive", "breaker", "outlier"]))
    add(lane("dns_on_connect", work, features=["dns"]))
    add(lane("dns_startup", work, features=["dns_startup"]))
    return {item.name: item for item in lanes}


LANES = build_lanes()
DEFAULT_LANES = [
    "h1_b0", "h1_b1k", "h1_b64k", "h1_b1m", "h1_b1k_c1", "h1_b1k_c256", "h1_newconn_b1k",
    "f_base", "f_fixed", "f_access_log", "f_timeouts", "f_retry", "f_queue", "f_capacity", "f_passive",
    "f_breaker", "f_outlier", "f_all_6_0", "tls_h1_keepalive", "tls_hs_full_tls13", "tls_hs_resume_cache_tls12",
    "h2_b1k_c4s16", "gzip_html64k", "cache_hit_b1k", "static_f1k", "ws_echo_1k", "l4_echo_1k",
]


_LOCK_DEPTH = 0


@contextmanager
def bench_lock():
    """Exclusive host lock; re-entrant within this process so --lock-batch can hold it across a few lanes."""
    global _LOCK_DEPTH
    if _LOCK_DEPTH:
        yield
        return
    with open(LOCK_FILE, "w", encoding="utf-8") as handle:
        fcntl.flock(handle, fcntl.LOCK_EX)
        _LOCK_DEPTH += 1
        try:
            yield
        finally:
            _LOCK_DEPTH -= 1
            fcntl.flock(handle, fcntl.LOCK_UN)


def port_offsets(lane):
    return [*range(1 if lane.servers > 2 else lane.servers), 10, 20, 21, 22, *(30 + index for index in range(1, lane.listeners))]


def pick_ports(lane):
    """Fixed ports sit inside the ephemeral range: take a block whose ports are bindable now (retrying while
    other processes' TIME_WAIT sockets expire)."""
    global UPSTREAM_PORT, ECHO_PORT, PROXY_PORT
    deadline = time.monotonic() + 150
    while time.monotonic() < deadline:
        for base in range(PORT_FIRST, PORT_LAST - PORT_SPAN, 3):
            probes = []
            try:
                for offset in port_offsets(lane):
                    probe = socket.socket()
                    probes.append(probe)
                    probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                    probe.bind(("127.0.0.1", base + offset))
            except OSError:
                continue
            finally:
                for probe in probes:
                    probe.close()
            UPSTREAM_PORT, ECHO_PORT, PROXY_PORT = base, base + 10, base + 20
            return
        time.sleep(5)
    raise RuntimeError("no free port block in the allotted range")


def raise_nofile():
    soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
    resource.setrlimit(resource.RLIMIT_NOFILE, (min(hard, 200000), hard))


def spawn(command, cpus, log, wrap=(), env=None):
    argv = ["taskset", "-c", cpus, *wrap, *command]
    return subprocess.Popen(argv, stdout=open(log, "w"), stderr=subprocess.STDOUT, preexec_fn=raise_nofile,
                            env={**os.environ, **(env or {})})


def wait_port(port, process, timeout=30.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"process exited before listening on {port}")
        try:
            socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
            return
        except OSError:
            time.sleep(0.05)
    raise RuntimeError(f"port {port} not ready")


def children(pid):
    path = Path(f"/proc/{pid}/task/{pid}/children")
    return [int(child) for child in path.read_text().split()] if path.exists() else []


def stop(process):
    """SIGTERM the traced child first (strace/valgrind wrappers), then the process itself."""
    if process.poll() is None:
        for child in children(process.pid):
            os.kill(child, signal.SIGTERM)
        if not children(process.pid):
            process.send_signal(signal.SIGTERM)
        try:
            process.wait(timeout=20)
        except subprocess.TimeoutExpired:
            for child in children(process.pid):
                os.kill(child, signal.SIGKILL)
            process.kill()
            process.wait()


def proc_status(pid):
    status = Path(f"/proc/{pid}/status").read_text()
    pick = lambda key: int(re.search(rf"^{key}:\s+(\d+)", status, re.M).group(1))
    return {"rss_kb": pick("VmRSS"), "hwm_kb": pick("VmHWM"), "threads": pick("Threads"),
            "fds": len(os.listdir(f"/proc/{pid}/fd"))}


class Env:
    def __init__(self, args, lane, binary=None):
        self.binary = binary or args.bin
        self.args = args
        self.lane = lane
        self.work = Path(tempfile.mkdtemp(prefix="r2-", dir=args.tmp))
        self.upstream = None
        self.proxy = None

    def http_ports(self):
        return [PROXY_PORT] + [PROXY_PORT + 10 + index for index in range(1, self.lane.listeners)]

    def prepare_files(self):
        (self.work / "logs").mkdir()
        www = self.work / "www" / "static"
        www.mkdir(parents=True)
        for name, size in (("f1k", 1024), ("f64k", 65536), ("f1m", 1048576)):
            (www / f"{name}.bin").write_bytes(b"x" * size)

    def start(self, wrap=(), proxy_env=None):
        lane = self.lane
        self.prepare_files()
        config = self.work / "oxiroute.lua"
        generated = subprocess.run(
            [sys.executable, str(HERE / "gen_config.py"), str(config), "--proxy-port", str(PROXY_PORT),
             "--upstream-port", str(UPSTREAM_PORT), "--echo-port", str(ECHO_PORT), "--servers", str(lane.servers),
             "--listeners", str(lane.listeners), "--workers", str(lane.workers), "--run-dir", str(self.work)]
            + [item for feature in lane.features for item in ("--feature", feature)],
            check=True, capture_output=True, text=True, timeout=LANE_TIMEOUT)
        self.generated = generated.stdout
        if lane.servers > 2:  # many servers share one port on 127.0.0.1 .. 127.0.0.N
            listen = ",".join(f"127.0.0.{index + 1}:{UPSTREAM_PORT}" for index in range(lane.servers))
        else:
            listen = ",".join(f"127.0.0.1:{UPSTREAM_PORT + index}" for index in range(lane.servers))
        self.upstream = spawn([self.args.tool, "upstream", "--listen", listen,
                               "--echo-listen", f"127.0.0.1:{ECHO_PORT}"], UPSTREAM_CPUS, self.work / "upstream.log")
        wait_port(UPSTREAM_PORT, self.upstream)
        # The direct runtime keeps the proxy in one process; the supervised worker adds no data-path code.
        self.proxy = spawn([self.binary, "serve", str(config)], PROXY_CPUS, self.work / "proxy.log", wrap,
                           {"OXIROUTE_INTERNAL_TEST_DIRECT_RUNTIME": "1", "RUST_LOG": "warn",
                            **dict(item.split("=", 1) for item in self.args.proxy_env), **(proxy_env or {})})
        for port in self.http_ports():
            wait_port(port, self.proxy)

    def stop(self):
        for process in (self.proxy, self.upstream):
            if process is not None:
                stop(process)

    def close(self):
        self.stop()
        shutil.rmtree(self.work, ignore_errors=True)

    def load_command(self, requests, warmup, hooks=()):
        lane = self.lane
        targets = ",".join(f"127.0.0.1:{port}" for port in self.http_ports())
        if "--proto" in lane.load and lane.load[lane.load.index("--proto") + 1] == "tcp":
            targets = f"127.0.0.1:{PROXY_PORT + 1}"
        if lane.target_offset:
            targets = f"127.0.0.1:{PROXY_PORT + lane.target_offset}"
        command = [self.args.tool, "load", "--target", targets, "--requests", str(requests), "--warmup", str(warmup)]
        command += [item.replace("{UP}", str(UPSTREAM_PORT)) for item in lane.load]
        for name, value in hooks:
            command += [f"--{name}", value]
        return ["taskset", "-c", LOAD_CPUS] + command


def parse_kv(text):
    return {key: value for key, value in (line.split("=", 1) for line in text.splitlines() if "=" in line)}


def context_switches(path):
    text = Path(path).read_text()
    return sum(int(value) for value in re.findall(r"^voluntary_ctxt_switches:\s+(\d+)", text, re.M)), \
        sum(int(value) for value in re.findall(r"^nonvoluntary_ctxt_switches:\s+(\d+)", text, re.M))


def parse_perf(path):
    counters = {}
    for line in Path(path).read_text().splitlines():
        fields = line.split(",")
        if len(fields) > 2 and fields[0].replace(".", "", 1).isdigit():
            counters[fields[2].split(":")[0]] = float(fields[0])
    return counters


def idle_rate(env, seconds=2):
    out = env.work / "idle.csv"
    subprocess.run(["env", "LC_ALL=C", "perf", "stat", "-x,", "-e", "instructions:u,cycles:u", "-p", str(env.proxy.pid), "-o", str(out),
                    "--", "sleep", str(seconds)], check=True, capture_output=True)
    counters = parse_perf(out)
    return {key: value / seconds for key, value in counters.items()}


def measured_rep(env, rep, idle):
    ctl, ack, csv = (env.work / name for name in (f"ctl{rep}", f"ack{rep}", f"perf{rep}.csv"))
    os.mkfifo(ctl)
    os.mkfifo(ack)
    perf = subprocess.Popen(
        ["env", "LC_ALL=C", "perf", "stat", "-D", "-1", "-x,", "-e", "instructions:u,cycles:u", "-p", str(env.proxy.pid),
         "--control", f"fifo:{ctl},{ack}", "-o", str(csv)], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    pid = env.proxy.pid
    switches = lambda tag: f"cat /proc/{pid}/task/*/status > {env.work}/sw{rep}.{tag}"
    hooks = [("hook-start", f"{switches('a')}; echo enable > {ctl}; read _ < {ack}"),
             ("hook-stop", f"echo disable > {ctl}; read _ < {ack}; {switches('b')}")]
    lane = env.lane
    result = subprocess.run(env.load_command(lane.requests, lane.warmup, hooks), capture_output=True, text=True, timeout=LANE_TIMEOUT)
    perf.send_signal(signal.SIGINT)
    perf.wait()
    load = parse_kv(result.stdout)
    if result.returncode != 0 or not load:
        raise RuntimeError(f"load generator failed: {result.stdout} {result.stderr}")
    counters = parse_perf(csv)
    switched_before = context_switches(env.work / f"sw{rep}.a")
    switched = context_switches(env.work / f"sw{rep}.b")
    elapsed = float(load["elapsed_ms"]) / 1000
    requests = int(load["ok"])
    return {
        "instr": (counters["instructions"] - idle["instructions"] * elapsed) / requests,
        "cycles": (counters["cycles"] - idle["cycles"] * elapsed) / requests,
        "ctxsw": (switched[0] - switched_before[0]) / requests,
        "nvctxsw": (switched[1] - switched_before[1]) / requests,
        "rps": float(load["rps"]), "p50": int(load["p50_us"]), "p99": int(load["p99_us"]),
        "p999": int(load["p999_us"]), "errors": int(load["errors"]), "connect_p50": int(load["connect_p50_us"]),
        "connects": int(load["connects"]), "ok": requests,
    }


def median(values):
    return statistics.median(values)


def spread(values):
    middle = median(values)
    return 100.0 * (max(values) - min(values)) / middle if middle else 0.0


def measure_perf(env, reps):
    idle = idle_rate(env)
    samples = [measured_rep(env, rep, idle) for rep in range(reps)]
    status = proc_status(env.proxy.pid)
    column = lambda key: [sample[key] for sample in samples]
    return {
        "instr_per_req": round(median(column("instr"))), "instr_spread_pct": round(spread(column("instr")), 2),
        "cycles_per_req": round(median(column("cycles"))), "cycles_spread_pct": round(spread(column("cycles")), 2),
        "vctxsw_per_req": round(median(column("ctxsw")), 3), "nvctxsw_per_req": round(median(column("nvctxsw")), 3),
        "rps": round(median(column("rps"))), "rps_spread_pct": round(spread(column("rps")), 1),
        "p50_us": round(median(column("p50"))), "p99_us": round(median(column("p99"))),
        "p999_us": round(median(column("p999"))), "connect_p50_us": round(median(column("connect_p50"))),
        "connects": round(median(column("connects"))), "errors": sum(column("errors")), "rss_kb": status["rss_kb"], "hwm_kb": status["hwm_kb"],
        "threads": status["threads"], "fds": status["fds"],
    }


def strace_counts(path):
    counts = {}
    for line in Path(path).read_text().splitlines():
        match = re.match(r"\s*[\d.]+\s+[\d.]+\s+\d+\s+(\d+)\s+(?:\d+\s+)?([a-z_0-9]+)\s*$", line)
        if match and match.group(2) != "total":
            counts[match.group(2)] = int(match.group(1))
    return counts


def measure_syscalls(args, lane):
    """Slope of strace -c totals between two request counts: syscalls per request, by name."""
    totals = []
    for requests in (args.sys_low, args.sys_high):
        env = Env(args, lane)
        summary = env.work / "strace.txt"
        try:
            env.start(wrap=["env", "LC_ALL=C", "strace", "-f", "-c", "-o", str(summary)])
            subprocess.run(env.load_command(requests, lane.warmup), capture_output=True, text=True, check=True, timeout=LANE_TIMEOUT)
        finally:
            env.stop()
        totals.append(strace_counts(summary))
        env.close()
    span = args.sys_high - args.sys_low
    per_request = {name: (totals[1].get(name, 0) - totals[0].get(name, 0)) / span for name in totals[1]}
    top = sorted(per_request.items(), key=lambda item: -item[1])[:8]
    result = {"syscalls_per_req": round(sum(per_request.values()), 2)}
    result.update({f"sys_{name}": round(value, 3) for name, value in top if value > 0.01})
    return result


def build_alloc_shim(args):
    shim = Path(args.tmp) / f"r2-alloc-count-{os.getuid()}.so"
    source = HERE / "tool/alloc_count.c"
    if not shim.exists() or shim.stat().st_mtime < source.stat().st_mtime:
        subprocess.run(["gcc", "-O2", "-shared", "-fPIC", "-o", str(shim), str(source), "-ldl"], check=True)
    return shim


def measure_allocs(args, lane):
    """Allocator calls and requested bytes per request, counted by an LD_PRELOAD shim between two snapshots."""
    env = Env(args, lane)
    snapshots = env.work / "alloc.txt"
    try:
        env.start(proxy_env={"LD_PRELOAD": str(build_alloc_shim(args)), "R2_ALLOC_FILE": str(snapshots)})
        pid = env.proxy.pid
        hooks = [("hook-start", f"kill -USR1 {pid}"), ("hook-stop", f"kill -USR1 {pid}")]
        result = subprocess.run(env.load_command(args.alloc_requests, args.alloc_warmup, hooks), capture_output=True, text=True, timeout=LANE_TIMEOUT)
        load = parse_kv(result.stdout)
        if not load:
            raise RuntimeError(f"alloc load failed: {result.stderr}")
        # The shim signal handler runs asynchronously, so wait for both snapshot lines.
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline and len(snapshots.read_text().splitlines() if snapshots.exists() else []) < 2:
            time.sleep(0.02)
        first, second = [list(map(int, line.split())) for line in snapshots.read_text().splitlines()[:2]]
    finally:
        env.close()
    requests = int(load["ok"])
    delta = [after - before for before, after in zip(first, second)]
    return {"mallocs_per_req": round(sum(delta[0:4]) / requests, 1), "frees_per_req": round(delta[4] / requests, 1),
            "alloc_bytes_per_req": round(delta[5] / requests),
            "clock_gettime_per_req": round(delta[6] / requests, 1), "alloc_requests": requests}


def measure_memory(args, lane):
    """Heap and RSS per idle keep-alive connection: live-byte delta of the allocator shim between 0 and N connections."""
    env = Env(args, lane)
    snapshots = env.work / "mem.txt"
    connections = args.mem_conns
    try:
        env.start(proxy_env={"LD_PRELOAD": str(build_alloc_shim(args)), "R2_ALLOC_FILE": str(snapshots)})
        pid = env.proxy.pid
        rss_before = proc_status(pid)["rss_kb"]
        subprocess.run(["kill", "-USR1", str(pid)], check=True)
        deadline = time.monotonic() + 5
        while not snapshots.exists() and time.monotonic() < deadline:
            time.sleep(0.02)
        command = env.load_command(connections, connections, [("hook-start", f"kill -USR1 {pid}")])
        command[command.index("--conns") + 1] = str(connections)
        subprocess.run(command, capture_output=True, text=True, check=True, timeout=LANE_TIMEOUT)
        deadline = time.monotonic() + 5
        while len(snapshots.read_text().splitlines()) < 2 and time.monotonic() < deadline:
            time.sleep(0.02)
        rss_after = proc_status(pid)["rss_kb"]
        first, second = [list(map(int, line.split())) for line in snapshots.read_text().splitlines()[:2]]
    finally:
        env.close()
    return {"mem_conns": connections, "heap_live_bytes_per_conn": round((second[7] - first[7]) / connections),
            "rss_bytes_per_conn": round((rss_after - rss_before) * 1024 / connections)}


def measure_profile(args, lane):
    """perf record sampled by user-space instructions (deterministic attribution), DWARF call graphs."""
    env = Env(args, lane, args.profile_bin)
    call_graph = [] if args.flat else ["--user-callchains", "--call-graph", "fp" if args.profile_bin else "dwarf,16384"]
    ctl, ack = env.work / "ctl", env.work / "ack"
    os.mkfifo(ctl)
    os.mkfifo(ack)
    data = Path(args.out) / "profile" / f"{lane.name}.perf.data"
    data.parent.mkdir(parents=True, exist_ok=True)
    try:
        env.start()
        recorder = subprocess.Popen(
            ["perf", "record", "-D", "-1", "-e", args.profile_event, "-c", str(args.profile_period), *call_graph, "-p", str(env.proxy.pid), "--control", f"fifo:{ctl},{ack}", "-o", str(data)],
            stdout=subprocess.DEVNULL, stderr=open(env.work / "record.log", "w"))
        hooks = [("hook-start", f"echo enable > {ctl}; read _ < {ack}"), ("hook-stop", f"echo disable > {ctl}; read _ < {ack}")]
        result = subprocess.run(env.load_command(args.profile_requests, args.profile_warmup, hooks), capture_output=True, text=True, timeout=LANE_TIMEOUT)
        recorder.send_signal(signal.SIGINT)
        recorder.wait()
        load = parse_kv(result.stdout)
        shutil.copy(env.work / "record.log", data.with_suffix(".log"))
    finally:
        env.close()
    views = [("self", ["--no-children", "-g", "none"])]
    if call_graph:
        views.append(("children", ["--children", "-g", "none"]))
    for label, flags in views:
        report = subprocess.run(["perf", "report", "--stdio", "-i", str(data), "--percent-limit", "0.3", *flags, "--sort", "sym"],
                                capture_output=True, text=True, env={**os.environ, "LC_ALL": "C"})
        (data.parent / f"{lane.name}.{label}.txt").write_text(report.stdout)
    return {"profile_requests": int(load.get("ok", 0))}


def run_lane(args, lane):
    row = {"lane": lane.name, "features": ",".join(lane.features) or "-", "listeners": lane.listeners}
    if args.perf:
        with bench_lock():
            pick_ports(lane)
            env = Env(args, lane)
            try:
                env.start()
                row.update(measure_perf(env, args.reps))
                row["loadavg"] = os.getloadavg()[0]
            finally:
                env.close()
    if args.sys:
        with bench_lock():
            pick_ports(lane)
            row.update(measure_syscalls(args, lane))
    if args.alloc:
        with bench_lock():
            pick_ports(lane)
            row.update(measure_allocs(args, lane))
    if args.mem:
        with bench_lock():
            pick_ports(lane)
            row.update(measure_memory(args, lane))
    if args.profile:
        with bench_lock():
            pick_ports(lane)
            row.update(measure_profile(args, lane))
    return row


def render(row):
    return " ".join(f"{key}={value}" for key, value in row.items())


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--bin", default=os.environ.get("OXIROUTE_BIN", "target/release/oxiroute"))
    parser.add_argument("--tool", default=os.environ.get("R2TOOL_BIN", str(HERE / "tool/target/release/r2tool")))
    parser.add_argument("--lanes", default="default", help="comma list, 'default', 'all' or a prefix ending in '*'")
    parser.add_argument("--reps", type=int, default=5)
    parser.add_argument("--out", type=Path, default=Path(os.environ.get("R2_OUT", "/tmp/r2-out")))
    parser.add_argument("--tmp", default=os.environ.get("R2_TMP", "/dev/shm"))
    parser.add_argument("--no-perf", dest="perf", action="store_false", help="skip the instruction/latency lane run")
    parser.add_argument("--sys", action="store_true", help="also count syscalls per request (strace slope)")
    parser.add_argument("--alloc", action="store_true", help="also count allocator calls per request (LD_PRELOAD shim)")
    parser.add_argument("--mem", action="store_true", help="also measure heap/RSS per idle keep-alive connection")
    parser.add_argument("--mem-conns", type=int, default=2000)
    parser.add_argument("--profile", action="store_true", help="also perf-record per-function instruction samples")
    parser.add_argument("--profile-period", type=int, default=20011)
    parser.add_argument("--profile-event", default="instructions:u",
                        help="sampled event; cycles:u attributes cache-line stalls that instruction sampling hides")
    parser.add_argument("--flat", action="store_true", help="--profile without call graphs")
    parser.add_argument("--profile-bin", default=os.environ.get("R2_PROFILE_BIN"),
                        help="frame-pointer build used with --call-graph fp (default: --bin, flat profile)")
    parser.add_argument("--profile-requests", type=int, default=3000)
    parser.add_argument("--profile-warmup", type=int, default=500)
    parser.add_argument("--sys-low", type=int, default=1000)
    parser.add_argument("--sys-high", type=int, default=3000)
    parser.add_argument("--alloc-requests", type=int, default=4000)
    parser.add_argument("--alloc-warmup", type=int, default=1000)
    parser.add_argument("--scale", type=float, default=1.0, help="multiply every lane's request counts (smoke runs)")
    parser.add_argument("--proxy-env", action="append", default=[], metavar="KEY=VALUE",
                        help="extra proxy environment, e.g. LD_PRELOAD=/usr/lib/libjemalloc.so.2 for allocator A/B")
    parser.add_argument("--lock-batch", type=int, default=1,
                        help="hold the shared bench lock across this many consecutive lanes (keep batches to a few minutes)")
    parser.add_argument("--list", action="store_true")
    args = parser.parse_args()
    if args.list:
        for item in LANES.values():
            print(f"{item.name} features={','.join(item.features) or '-'} listeners={item.listeners} load={' '.join(item.load)}")
        return 0
    selected = []
    for name in args.lanes.split(","):
        if name == "default":
            selected += DEFAULT_LANES
        elif name == "all":
            selected += list(LANES)
        elif name.endswith("*"):
            selected += [lane_name for lane_name in LANES if lane_name.startswith(name[:-1])]
        else:
            selected.append(name)
    args.out.mkdir(parents=True, exist_ok=True)
    rows = []
    for start in range(0, len(selected), args.lock_batch):
        with bench_lock():
            for name in selected[start:start + args.lock_batch]:
                chosen = LANES[name]
                chosen.requests = max(50, int(chosen.requests * args.scale))
                chosen.warmup = max(20, int(chosen.warmup * args.scale))
                row = run_lane(args, chosen)
                rows.append(row)
                print(render(row), flush=True)
    (args.out / "results.json").write_text(json.dumps(rows, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
