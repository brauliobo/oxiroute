#!/usr/bin/env python3
"""PSS of OxiRoute, nginx and HAProxy after configuration reloads, one worker, 1,000 exact routes plus a catch-all.

Each proxy starts on its own CPUs with one origin, settles for one second, reloads the configuration `--reloads`
times (OxiRoute: management API with the previous generation retired before measuring; nginx: SIGHUP; HAProxy
master-worker: SIGUSR2) and reports the summed PSS of the proxy's process tree in MB after the listed reload counts.

Usage: reload_memory.py --bin OXIROUTE --tool R2TOOL [--counts 0,1,10,50] [--proxy-cpus 8-9] [--origin-cpus 10-11]
"""

import argparse
import json
import os
import secrets
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from gen_config import lua  # noqa: E402

ROUTES = 1000


def free_port():
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def descendants(pid):
    found = [pid]
    for task in Path(f"/proc/{pid}/task").iterdir():
        for child in (task / "children").read_text().split():
            found += descendants(int(child))
    return found


def pss_mb(pid):
    return sum(int(line.split()[1]) for member in descendants(pid)
               for line in Path(f"/proc/{member}/smaps_rollup").read_text().splitlines() if line.startswith("Pss:")) / 1024


def stable(pid, timeout=60):
    deadline, last, since = time.monotonic() + timeout, None, time.monotonic()
    while time.monotonic() < deadline:
        now = sorted(descendants(pid))
        if now != last:
            last, since = now, time.monotonic()
        elif time.monotonic() - since >= 1:
            return
        time.sleep(0.1)
    raise RuntimeError("process tree did not settle")


def wait_port(port, process):
    while process.poll() is None:
        try:
            socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
            return
        except OSError:
            time.sleep(0.05)
    raise RuntimeError(f"process exited before listening on {port}")


def oxiroute_config(work, port, origin, management, generation):
    routes = [{"path": {"kind": "exact", "value": f"/unused-{index}"}, "methods": [],
               "action": {"type": "fixed_response", "status": 404, "body": f"not found {generation}"}} for index in range(ROUTES)]
    routes.append({"path": {"kind": "segment_prefix", "value": "/"}, "methods": [],
                   "action": {"type": "proxy", "upstream_pool": "origin", "policy": {
                       "upstream_host": {"type": "preserve_incoming"},
                       "retry": {"max_retries": 0, "triggers": ["connect_failure", "connect_timeout", "refused_stream"],
                                 "method_safety": "get_head", "body_safety": "empty"}}}})
    config = {
        "version": 1, "management": {"bind": f"127.0.0.1:{management}"}, "listener_workers": 1,
        "listeners": [{"name": "bench", "bind": {"type": "socket", "address": f"127.0.0.1:{port}"},
                       "protocol": "http", "service": "bench"}],
        "http_services": [{"name": "bench", "upstream_io_timeout_ms": 30000, "routes": routes}],
        "upstream_pools": [{"name": "origin", "algorithm": "round_robin", "http_versions": {"min": "1.1", "max": "1.1"}, "servers": [
            {"name": "origin-1", "endpoint": {"type": "socket", "address": f"127.0.0.1:{origin}"}}]}],
        "forward_proxy_services": [], "rtmp_services": [], "l4_services": [], "cache_stores": [],
    }
    (work / "oxiroute.lua").write_text("return " + lua(config) + "\n")


def nginx_config(work, port, origin, generation):
    locations = "\n".join(f'location = /unused-{index} {{ return 404 "not found {generation}"; }}' for index in range(ROUTES))
    (work / "nginx.conf").write_text(f"""worker_processes 1;
daemon off;
pid {work}/nginx.pid;
error_log {work}/nginx-error.log warn;
events {{ worker_connections 1024; }}
http {{
  access_log off;
  client_body_temp_path {work}/tmp-body;
  proxy_temp_path {work}/tmp-proxy;
  fastcgi_temp_path {work}/tmp-fastcgi;
  uwsgi_temp_path {work}/tmp-uwsgi;
  scgi_temp_path {work}/tmp-scgi;
  upstream origin {{ server 127.0.0.1:{origin}; keepalive 256; }}
  server {{
    listen 127.0.0.1:{port};
    location / {{ proxy_http_version 1.1; proxy_set_header Connection ''; proxy_pass http://origin; }}
{locations}
  }}
}}
""")


def haproxy_config(work, port, origin, generation):
    (work / "unused-paths.txt").write_text("".join(f"/unused-{index}\n" for index in range(ROUTES)))
    (work / "haproxy.cfg").write_text(f"""global
  nbthread 1
  maxconn 10000
defaults
  mode http
  timeout connect 5s
  timeout client 30s
  timeout server 30s
frontend bench
  bind 127.0.0.1:{port}
  http-request return status 404 content-type text/plain string 'not found {generation}' if {{ path -f {work}/unused-paths.txt }}
  default_backend origin
backend origin
  server origin1 127.0.0.1:{origin}
""")


class Management:
    def __init__(self, port, token):
        self.base, self.token = f"http://127.0.0.1:{port}", token

    def call(self, method, path, body=None, headers=None):
        request = urllib.request.Request(
            self.base + path, method=method, data=None if body is None else json.dumps(body).encode(),
            headers={"Authorization": f"Bearer {self.token}", "Content-Type": "application/json", **(headers or {})})
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.loads(response.read() or b"{}")

    def reload(self):
        revision = self.call("GET", "/api/v1/generations")["generation"]["activeRevision"]
        self.call("POST", "/api/v1/generations/reload", {"expectedActiveRevision": revision}, {"If-Generation-Revision": revision})

    def retired(self):
        return self.call("GET", "/api/v1/generations")["generation"]["retiredGenerations"]


def measure(args, name, work, origin):
    port, management = free_port(), free_port()
    token = "reload-" + secrets.token_hex(16)
    token_file = work / "management.token"
    token_file.write_text(token + "\n")
    token_file.chmod(0o600)
    env = {**os.environ, "OXIROUTE_MANAGEMENT_TOKEN_FILE": str(token_file), "RUST_LOG": "warn"}
    write = {"oxiroute": lambda generation: oxiroute_config(work, port, origin, management, generation),
             "nginx": lambda generation: nginx_config(work, port, origin, generation),
             "haproxy": lambda generation: haproxy_config(work, port, origin, generation)}[name]
    command = {"oxiroute": [args.bin, "serve", str(work / "oxiroute.lua")],
               "nginx": ["nginx", "-c", str(work / "nginx.conf"), "-p", str(work)],
               "haproxy": ["haproxy", "-W", "-db", "-f", str(work / "haproxy.cfg")]}[name]
    write(0)
    process = subprocess.Popen(["taskset", "-c", args.proxy_cpus, *command], env=env,
                               stdout=open(work / f"{name}.log", "w"), stderr=subprocess.STDOUT)
    try:
        wait_port(port, process)
        manager = Management(management, token) if name == "oxiroute" else None
        stable(process.pid)
        result = {0: round(pss_mb(process.pid), 1)}
        for count in range(1, max(args.counts) + 1):
            write(count)
            if manager:
                manager.reload()
                while manager.retired():
                    time.sleep(0.1)
            else:
                os.kill(process.pid, signal.SIGHUP if name == "nginx" else signal.SIGUSR2)
            stable(process.pid)
            if count in args.counts:
                result[count] = round(pss_mb(process.pid), 1)
        return result
    finally:
        process.terminate()
        process.wait(timeout=30)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--bin", required=True)
    parser.add_argument("--tool", required=True)
    parser.add_argument("--counts", type=lambda text: [int(item) for item in text.split(",")], default=[0, 1, 10, 50])
    parser.add_argument("--proxy-cpus", default="8-9")
    parser.add_argument("--origin-cpus", default="10-11")
    parser.add_argument("--proxies", default="oxiroute,nginx,haproxy")
    args = parser.parse_args()
    results = {}
    for name in args.proxies.split(","):
        with tempfile.TemporaryDirectory(prefix="reload-memory-") as scratch:
            work, origin = Path(scratch), free_port()
            server = subprocess.Popen(["taskset", "-c", args.origin_cpus, args.tool, "upstream", "--listen",
                                       f"127.0.0.1:{origin}", "--threads", "1"], stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
            try:
                wait_port(origin, server)
                results[name] = measure(args, name, work, origin)
            finally:
                server.terminate()
                server.wait(timeout=30)
        print(json.dumps({name: results[name]}), flush=True)
    print(json.dumps(results))


if __name__ == "__main__":
    main()
