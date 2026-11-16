#!/usr/bin/env python3
"""HTTP/3 over an impaired link: OxiRoute and nginx, QUIC and TCP+TLS, in a private network namespace.

Usage: h3_impair.py {download|small} --server {oxiroute,nginx} --proto {h3,tcp} [--rtt-ms N] [--loss-pct P]
                    [--rate RATE] [--size-mib N] [--runs N] [--out FILE.json]

The script re-executes itself under `unshare --user --map-root-user --net`: no privileges and no host network change.
Inside the namespace the loopback device gets a netem qdisc (delay of RTT/2 and loss P in each direction, so the round
trip time is RTT and a data packet or its acknowledgement is lost with probability P). Both servers, the client and the
qdisc live and die with the namespace.

`download` fetches one static file with curl (ngtcp2 for HTTP/3, nghttp2 for TCP+TLS), one fresh connection per run, and
reports goodput, p50/p99 completion time and server CPU per MiB. `small` drives `r2tool load --proto h3` against a 1 KiB
body and reports the request rate, p50/p99 and server CPU per request; an unimpaired run holds /tmp/oxiroute-bench.lock while it measures.

UDP segmentation offload and TCP segmentation offload are off for impaired runs: netem drops a whole super-packet, so
with offload a loss event would remove up to 64 KiB at once instead of one packet.
"""

import argparse
import contextlib
import fcntl
import json
import os
import re
import shutil
import socket
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
NAMESPACE_MARK = "H3_IMPAIR_IN_NAMESPACE"
PORT = 18443
HOST = "bench.example.com"
LOCK = "/tmp/oxiroute-bench.lock"
NGINX_CONF = """\
user root;
worker_processes 1;
daemon off;
pid {work}/nginx.pid;
error_log {work}/nginx-error.log warn;
events {{ worker_connections 1024; }}
http {{
  access_log off;
  client_body_temp_path {work}/tmp/body;
  proxy_temp_path {work}/tmp/proxy;
  fastcgi_temp_path {work}/tmp/fastcgi;
  uwsgi_temp_path {work}/tmp/uwsgi;
  scgi_temp_path {work}/tmp/scgi;
  sendfile on;
  keepalive_requests 100000000;
  http3_stream_buffer_size {stream_buffer};
  {extra}
  ssl_certificate {work}/cert.pem;
  ssl_certificate_key {work}/key.pem;
  ssl_protocols TLSv1.3;
  server {{
    listen {port} quic reuseport;
    listen {port} ssl;
    http2 on;
    http3 on;
    server_name {host};
    root {work}/www;
    location = /fixed {{ return 200 "{fixed}"; }}
  }}
}}
"""


def parse():
    parser = argparse.ArgumentParser()
    parser.add_argument("scenario", choices=["download", "small"])
    parser.add_argument("--server", choices=["oxiroute", "nginx"], required=True)
    parser.add_argument("--proto", choices=["h3", "tcp"], default="h3")
    parser.add_argument("--rtt-ms", type=float, default=0)
    parser.add_argument("--loss-pct", type=float, default=0)
    parser.add_argument("--rate", default="")
    parser.add_argument("--size-mib", type=int, default=16)
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--max-time", type=int, default=240)
    parser.add_argument("--requests", type=int, default=20000)
    parser.add_argument("--warmup", type=int, default=2000)
    parser.add_argument("--server-cpus", default="24-25")
    parser.add_argument("--client-cpus", default="26-27")
    parser.add_argument("--binary", default=os.environ.get("OXIROUTE_BIN", ""))
    parser.add_argument("--tool", default=os.environ.get("R2TOOL_BIN", ""))
    parser.add_argument("--set", action="append", default=[], help="oxiroute root option KEY=JSON")
    parser.add_argument("--listener-set", action="append", default=[], help="oxiroute listener option KEY=JSON")
    parser.add_argument("--keep-offload", action="store_true", help="leave UDP/TCP segmentation offload on")
    parser.add_argument("--nginx-stream-buffer", default="64k", help="nginx http3_stream_buffer_size (default 64k)")
    parser.add_argument("--nginx-extra", default="", help="extra nginx http-level directives")
    parser.add_argument("--queue-pkts", type=int, default=100000, help="netem queue limit in packets")
    parser.add_argument("--lock-held", action="store_true", help="the caller holds the benchmark lock for a batch")
    parser.add_argument("--client", choices=["curl", "r2tool"], default="curl")
    parser.add_argument("--label", default="")
    parser.add_argument("--out", default="")
    return parser.parse_args()


def enter_namespace():
    if os.environ.get(NAMESPACE_MARK):
        return
    os.environ[NAMESPACE_MARK] = "1"
    os.execvp("unshare", ["unshare", "--user", "--map-root-user", "--net", sys.executable, *sys.argv])


def run(*command):
    subprocess.run(command, check=True)


def impair(args):
    run("ip", "link", "set", "lo", "mtu", "1500", "up")
    if not args.keep_offload and (args.rtt_ms or args.loss_pct):
        run("ethtool", "-K", "lo", "tso", "off", "gso", "off", "gro", "off")
    if args.rtt_ms or args.loss_pct or args.rate:
        netem = ["tc", "qdisc", "add", "dev", "lo", "root", "netem", "limit", str(args.queue_pkts)]
        if args.rtt_ms:
            netem += ["delay", f"{args.rtt_ms / 2}ms"]
        if args.loss_pct:
            netem += ["loss", f"{args.loss_pct}%"]
        if args.rate:
            netem += ["rate", args.rate]
        run(*netem)


def cpu_pressure():
    some = Path("/proc/pressure/cpu").read_text().splitlines()[0]
    return float(re.search(r"avg10=([\d.]+)", some).group(1))


def qdisc_stats():
    text = subprocess.run(["tc", "-s", "qdisc", "show", "dev", "lo"], capture_output=True, text=True).stdout
    sent = re.search(r"Sent (\d+) bytes (\d+) pkt \(dropped (\d+)", text)
    return tuple(int(value) for value in sent.groups()) if sent else (0, 0, 0)


def spawn(command, cpus, log, env=None):
    return subprocess.Popen(["taskset", "-c", cpus, *command], stdout=open(log, "w"), stderr=subprocess.STDOUT,
                            env={**os.environ, **(env or {})})


def wait_udp(port, process, timeout=30.0):
    probe = bytes([0xC0]) + bytes.fromhex("1a2a3a4a") + bytes([8]) + os.urandom(8) + bytes([8]) + os.urandom(8)
    probe += bytes(1200 - len(probe))
    deadline = time.monotonic() + timeout
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as udp:
        udp.settimeout(0.2)
        while time.monotonic() < deadline:
            if process.poll() is not None:
                raise RuntimeError("server exited before listening")
            try:
                udp.sendto(probe, ("127.0.0.1", port))
                udp.recv(2048)
                return
            except (socket.timeout, ConnectionRefusedError):
                pass
    raise RuntimeError("udp port not ready")


def wait_tcp(port, process, timeout=30.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError("server exited before listening")
        try:
            socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
            return
        except OSError:
            pass
    raise RuntimeError("tcp port not ready")


def process_tree(pid):
    found = [pid]
    for task in Path(f"/proc/{pid}/task").iterdir():
        for child in (task / "children").read_text().split():
            found += process_tree(int(child))
    return found


def cpu_seconds(pid):
    ticks = os.sysconf("SC_CLK_TCK")
    total = 0
    for member in process_tree(pid):
        fields = Path(f"/proc/{member}/stat").read_text().rsplit(")", 1)[1].split()
        total += int(fields[11]) + int(fields[12])
    return total / ticks


def make_certificate(work):
    subprocess.run(["openssl", "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:prime256v1", "-nodes",
                    "-keyout", f"{work}/key.pem", "-out", f"{work}/cert.pem", "-days", "2", "-subj", f"/CN={HOST}",
                    "-addext", f"subjectAltName=DNS:{HOST}"], check=True, capture_output=True)


def make_payload(work, size_mib):
    (work / "www" / "static").mkdir(parents=True)
    block = os.urandom(1 << 20)
    with open(work / "www" / "static" / "f", "wb") as handle:
        for _ in range(size_mib):
            handle.write(block)


def start_nginx(args, work):
    (work / "tmp").mkdir()
    config = work / "nginx.conf"
    config.write_text(NGINX_CONF.format(work=work, port=PORT, host=HOST, fixed="x" * 1024,
                                           stream_buffer=args.nginx_stream_buffer,
                                           extra=args.nginx_extra))
    return spawn(["nginx", "-c", str(config), "-e", f"{work}/nginx-error.log"], args.server_cpus, work / "server.log")


def start_oxiroute(args, work):
    features = ["h3", "fixed_b1k", "static"] if args.proto == "h3" else ["tls", "fixed_b1k", "static"]
    command = [sys.executable, str(HERE / "gen_config.py"), str(work / "config.lua"), "--proxy-port", str(PORT),
               "--upstream-port", str(PORT + 100), "--run-dir", str(work), "--io-timeout-ms", str(args.max_time * 1000)]
    for feature in features:
        command += ["--feature", feature]
    for item in args.set:
        command += ["--set", item]
    for item in args.listener_set:
        command += ["--listener-set", item]
    run(*command)
    return spawn([args.binary, "serve", str(work / "config.lua")], args.server_cpus, work / "server.log",
                 {"OXIROUTE_INTERNAL_TEST_DIRECT_RUNTIME": "1", "RUST_LOG": "warn"})


def curl_once(args, work, path):
    protocol = "--http3-only" if args.proto == "h3" else "--http2"
    command = ["taskset", "-c", args.client_cpus, "curl", "-sS", "-k", protocol, "--max-time", str(args.max_time),
               "--resolve", f"{HOST}:{PORT}:127.0.0.1", "-o", "/dev/null",
               "-w", "%{time_total} %{size_download} %{http_version} %{exitcode}", f"https://{HOST}:{PORT}{path}"]
    out = subprocess.run(command, capture_output=True, text=True)
    seconds, size, version, code = (out.stdout.split() + ["0", "0", "", "-1"])[:4]
    return {"seconds": float(seconds), "bytes": int(size), "http": version, "exit": int(code)}


def r2tool_once(args, path):
    command = ["taskset", "-c", args.client_cpus, args.tool, "load", "--proto", "h3", "--conns", "1", "--streams", "1",
               "--path", path, "--requests", "1", "--warmup", "0", "--host", HOST, "--target", f"127.0.0.1:{PORT}",
               "--expect-status", "200"]
    out = subprocess.run(command, capture_output=True, text=True, timeout=args.max_time)
    fields = dict(line.split("=", 1) for line in out.stdout.splitlines() if "=" in line)
    failed = int(fields.get("errors", "1")) or out.returncode
    return {"seconds": int(fields.get("p50_us", "0")) / 1e6, "bytes": args.size_mib << 20, "http": "3", "exit": failed}


def percentile(values, fraction):
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, max(0, round(fraction * len(ordered) + 0.5) - 1))]


def summarize_download(args, samples, cpu, qdisc):
    good = [sample for sample in samples if sample["exit"] == 0 and sample["bytes"] == args.size_mib << 20]
    times = [sample["seconds"] for sample in good]
    total = sum(sample["bytes"] for sample in good)
    result = {"ok_runs": len(good), "runs": len(samples), "failures": [s["exit"] for s in samples if s not in good],
              "http": sorted({sample["http"] for sample in good}), "cpu_s": round(cpu, 3),
              "qdisc_bytes_pkts_dropped": qdisc}
    if good:
        result.update(p50_s=round(statistics.median(times), 3), p99_s=round(percentile(times, 0.99), 3),
                      min_s=round(min(times), 3), goodput_mib_s=round(args.size_mib / statistics.median(times), 3),
                      cpu_ms_per_mib=round(cpu * 1000 / (total / (1 << 20)), 3))
    return result


def measure_download(args, server, work):
    curl_once(args, work, "/fixed")
    before_cpu, before_net = cpu_seconds(server.pid), qdisc_stats()
    fetch = curl_once if args.client == "curl" else lambda args, work, path: r2tool_once(args, path)
    samples = [fetch(args, work, "/static/f") for _ in range(args.runs)]
    after_net = qdisc_stats()
    delta = tuple(after - before for after, before in zip(after_net, before_net))
    return summarize_download(args, samples, cpu_seconds(server.pid) - before_cpu, delta)


def measure_small(args, server, work):
    command = [args.tool, "load", "--proto", "h3", "--conns", "8", "--streams", "4", "--path", "/fixed",
               "--expect-len", "1024", "--requests", str(args.requests), "--warmup", str(args.warmup), "--host", HOST,
               "--target", f"127.0.0.1:{PORT}"]
    before = cpu_seconds(server.pid)
    out = subprocess.run(["taskset", "-c", args.client_cpus, *command], capture_output=True, text=True, timeout=300)
    used = cpu_seconds(server.pid) - before
    fields = dict(line.split("=", 1) for line in out.stdout.splitlines() if "=" in line)
    fields["server_cpu_us_per_req"] = round(used * 1e6 / (args.requests + args.warmup), 2)
    return fields


@contextlib.contextmanager
def measuring(args):
    if args.rtt_ms or args.loss_pct or args.rate or args.lock_held:
        yield
        return
    with open(LOCK, "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        yield


def serve(args, work):
    make_certificate(work)
    make_payload(work, args.size_mib)
    server = start_nginx(args, work) if args.server == "nginx" else start_oxiroute(args, work)
    try:
        (wait_udp if args.proto == "h3" else wait_tcp)(PORT, server)
        measure = measure_download if args.scenario == "download" else measure_small
        before = cpu_pressure()
        with measuring(args):
            result = measure(args, server, work)
        return {**result, "psi_cpu_some_avg10_before": before, "psi_cpu_some_avg10_after": cpu_pressure()}
    finally:
        server.terminate()
        server.wait(timeout=30)


def main():
    args = parse()
    enter_namespace()
    impair(args)
    work = Path(tempfile.mkdtemp(prefix="h3impair-"))
    try:
        result = {"label": args.label, "scenario": args.scenario, "server": args.server, "proto": args.proto,
                  "rtt_ms": args.rtt_ms, "loss_pct": args.loss_pct, "rate": args.rate, "size_mib": args.size_mib,
                  **serve(args, work)}
    finally:
        shutil.rmtree(work, ignore_errors=True)
    text = json.dumps(result)
    print(text)
    if args.out:
        with open(args.out, "a", encoding="utf-8", newline="\n") as handle:
            handle.write(text + "\n")


if __name__ == "__main__":
    main()
