#!/usr/bin/env python3
"""Mixed-traffic soak of one OxiRoute process, with a configuration reload in the middle.

Keep-alive, new-connection, POST, WebSocket-style upgrade, TLS HTTP/1.1, HTTP/2 over TLS and a TCP echo run
at a low, paced rate against one proxy for `--duration` seconds. The proxy's RSS, descriptors, threads and the
connections it holds towards the origin are sampled every `--interval` seconds. At `--reload-at` the
configuration changes and a new generation is activated through the management API (a token created for this run,
loopback only) while requests are in flight. The run fails on any load error, a descriptor count that does not
return to its pre-load level, or a proxy that does not exit cleanly on SIGTERM.

Usage: soak.py [--bin BIN] [--tool R2TOOL] [--duration 60] [--interval 5] [--reload-at 30] [--pace-ms 20]
"""

import argparse
import json
import os
import secrets
import shutil
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
import gen_config  # noqa: E402


def free_ports(count, pairs=()):
    """`count` free loopback ports, and for each index in `pairs` also the port right after it (gen_config
    puts a second origin and the TCP echo listener at `port + 1`)."""
    while True:
        holders = []
        try:
            ports = []
            for index in range(count):
                holder = socket.socket()
                holder.bind(("127.0.0.1", 0))
                holders.append(holder)
                ports.append(holder.getsockname()[1])
                if index in pairs:
                    neighbour = socket.socket()
                    neighbour.bind(("127.0.0.1", ports[-1] + 1))
                    holders.append(neighbour)
            return ports
        except OSError:
            continue
        finally:
            for holder in holders:
                holder.close()


def build_config(work, ports):
    upstream, echo, proxy, tls, management = ports
    args = argparse.Namespace(
        proxy_port=proxy, upstream_port=upstream, echo_port=echo, servers=2, listeners=1, workers=0,
        run_dir=str(work), feature=["access_log", "timeouts", "retry", "fixed", "static", "l4", "tls"])
    config = gen_config.build(args)
    secure = config["listeners"][0]
    secure["bind"] = {"type": "socket", "address": f"127.0.0.1:{tls}"}
    secure["name"] = "bench-tls"
    config["listeners"].append({
        "name": "bench", "bind": {"type": "socket", "address": f"127.0.0.1:{proxy}"},
        "protocol": "http", "service": "bench"})
    config["management"] = {"bind": f"127.0.0.1:{management}"}
    return config


def write_config(path, config):
    path.write_text("return " + gen_config.lua(config) + "\n", encoding="utf-8", newline="\n")


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
        return self.call("POST", "/api/v1/generations/reload", {"expectedActiveRevision": revision},
                         {"If-Generation-Revision": revision})


def wait_port(port, process, timeout=30.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        assert process.poll() is None, f"process exited before listening on {port}"
        try:
            socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
            return
        except OSError:
            time.sleep(0.1)
    raise RuntimeError(f"port {port} not ready")


def sample(pid, upstream_ports):
    status = Path(f"/proc/{pid}/status").read_text()
    field = lambda name: int(next(line for line in status.splitlines() if line.startswith(name)).split()[1])
    peers = subprocess.run(["ss", "-Htn", "state", "established"], capture_output=True, text=True).stdout
    wanted = {str(port) for port in upstream_ports}
    held = sum(1 for line in peers.splitlines() if line.split()[-1].rsplit(":", 1)[1] in wanted)
    return {"rss_kb": field("VmRSS"), "threads": field("Threads"), "fds": len(os.listdir(f"/proc/{pid}/fd")),
            "to_origin": held}


def diagnose(manager, ports):
    """What a stuck load run looks like from the outside: socket queues towards the proxy and the generations."""
    filters = " or ".join(f"sport = :{port} or dport = :{port}" for port in ports)
    sockets = subprocess.run(["ss", "-tnie", f"( {filters} )"], capture_output=True, text=True).stdout
    return {"sockets": [line[:200] for line in sockets.splitlines()],
            "generations": manager.call("GET", "/api/v1/generations"),
            "requests": manager.call("GET", "/api/v1/requests")}


def request_id_probes(proxy, tls):
    """Every response class carries `x-request-id`, over HTTP/1.1, TLS and HTTP/2."""
    checks = {
        "proxied": ["curl", "-s", "-D-", "-o/dev/null", f"http://127.0.0.1:{proxy}/b1k"],
        "fixed": ["curl", "-s", "-D-", "-o/dev/null", f"http://127.0.0.1:{proxy}/fixed"],
        "static": ["curl", "-s", "-D-", "-o/dev/null", f"http://127.0.0.1:{proxy}/static/f1k.bin"],
        "static_missing": ["curl", "-s", "-D-", "-o/dev/null", f"http://127.0.0.1:{proxy}/static/none.bin"],
        "post_sink": ["curl", "-s", "-D-", "-o/dev/null", "-d", "x", f"http://127.0.0.1:{proxy}/sink"],
        "h2_proxied": ["curl", "-sk", "--http2", "-D-", "-o/dev/null", "--resolve", f"bench.example.com:{tls}:127.0.0.1",
                       f"https://bench.example.com:{tls}/b1k"],
        "h2_fixed": ["curl", "-sk", "--http2", "-D-", "-o/dev/null", "--resolve", f"bench.example.com:{tls}:127.0.0.1",
                     f"https://bench.example.com:{tls}/fixed"],
        "tls_h1_fixed": ["curl", "-sk", "--http1.1", "-D-", "-o/dev/null", "--resolve", f"bench.example.com:{tls}:127.0.0.1",
                         f"https://bench.example.com:{tls}/fixed"],
    }
    missing = []
    for name, command in checks.items():
        head = subprocess.run(command, capture_output=True, text=True, timeout=20).stdout.lower()
        if head.count("\nx-request-id:") + head.startswith("x-request-id:") != 1:
            missing.append(name)
    return missing


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bin", default=os.environ.get("OXIROUTE_BIN", "target/release/oxiroute"))
    parser.add_argument("--tool", default=os.environ.get("R2TOOL_BIN", str(HERE / "tool/target/release/r2tool")))
    parser.add_argument("--duration", type=int, default=60)
    parser.add_argument("--interval", type=int, default=5)
    parser.add_argument("--reload-at", type=int, default=30)
    parser.add_argument("--reload-every", type=int, default=0, help="reload again every N seconds after the first")
    parser.add_argument("--mix", default="", help="comma list of load names to run (default all)")
    parser.add_argument("--pace-ms", type=int, default=20)
    parser.add_argument("--tmp", default="/var/tmp")
    args = parser.parse_args()

    work = Path(tempfile.mkdtemp(prefix="soak-", dir=args.tmp))
    (work / "logs").mkdir()
    (work / "conf").mkdir()
    www = work / "www" / "static"
    www.mkdir(parents=True)
    (www / "f1k.bin").write_bytes(b"x" * 1024)
    token = "soak-" + secrets.token_hex(16)
    token_file = work / "conf" / "management.token"
    token_file.write_text(token + "\n")
    token_file.chmod(0o600)
    ports = free_ports(5, pairs=(0, 2))
    upstream, echo, proxy, tls, management = ports
    config = build_config(work, ports)
    config_path = work / "conf" / "oxiroute.lua"
    write_config(config_path, config)

    processes, loads, result = [], [], {"samples": []}
    try:
        origin = subprocess.Popen(
            [args.tool, "upstream", "--listen", f"127.0.0.1:{upstream},127.0.0.1:{upstream + 1}",
             "--echo-listen", f"127.0.0.1:{echo}"], stdout=open(work / "upstream.log", "w"), stderr=subprocess.STDOUT)
        processes.append(origin)
        wait_port(upstream, origin)
        server = subprocess.Popen(
            [args.bin, "serve", str(config_path)], stdout=open(work / "proxy.log", "w"), stderr=subprocess.STDOUT,
            env={**os.environ, "OXIROUTE_INTERNAL_TEST_DIRECT_RUNTIME": "1", "RUST_LOG": "warn",
                 "OXIROUTE_MANAGEMENT_TOKEN_FILE": str(token_file)})
        processes.append(server)
        for port in (proxy, tls, management):
            wait_port(port, server)
        manager = Management(management, token)
        result["request_id_missing_before"] = request_id_probes(proxy, tls)
        time.sleep(1)
        result["baseline"] = sample(server.pid, (upstream, upstream + 1))

        base = [args.tool, "load", "--requests", "1", "--warmup", "1", "--warmup-duration", "1",
                "--duration", str(args.duration), "--pace-ms", str(args.pace_ms),
                "--reconnect-closed"]
        target = lambda port: ["--target", f"127.0.0.1:{port}"]
        mixes = {
            "h1_keepalive": ["--conns", "4", "--path", "/b1k"] + target(proxy),
            "h1_newconn": ["--conns", "2", "--new-conn", "--path", "/b1k"] + target(proxy),
            "h1_post": ["--conns", "2", "--method", "POST", "--path", "/sink", "--body-bytes", "4096"] + target(proxy),
            # Requests that are still in flight when the generation is replaced.
            "h1_slow": ["--conns", "4", "--path", "/slow", "--pace-ms", "0"] + target(proxy),
            "h1_upload_1m": ["--conns", "2", "--method", "POST", "--path", "/sink", "--body-bytes", "1048576",
                             "--pace-ms", "0"] + target(proxy),
            "ws": ["--conns", "2", "--proto", "ws", "--path", "/ws", "--msg-bytes", "1024"] + target(proxy),
            "ws_tls": ["--conns", "2", "--proto", "wstls", "--msg-bytes", "1024"] + target(tls),
            "tls_h1": ["--conns", "2", "--proto", "h1tls", "--path", "/b1k"] + target(tls),
            "h2": ["--conns", "2", "--streams", "4", "--proto", "h2tls", "--path", "/b1k"] + target(tls),
            # The TCP echo is relayed by the l4 listener gen_config binds at proxy_port + 1.
            "l4": ["--conns", "2", "--proto", "tcp", "--msg-bytes", "1024"] + target(proxy + 1),
        }
        for name, flags in mixes.items():
            if args.mix and name not in args.mix.split(","):
                continue
            out = open(work / f"load-{name}.log", "w")
            loads.append((name, subprocess.Popen(base + flags, stdout=out, stderr=subprocess.STDOUT)))

        started = time.monotonic()
        next_reload = args.reload_at
        next_sample = started
        while time.monotonic() - started < args.duration + 2:
            now = time.monotonic()
            if now >= next_sample:
                taken = sample(server.pid, (upstream, upstream + 1))
                taken["t"] = round(now - started)
                result["samples"].append(taken)
                next_sample += args.interval
            if next_reload is not None and now - started >= next_reload:
                config["http_services"][0]["upstream_io_timeout_ms"] += 1
                write_config(config_path, config)
                result["reload"] = manager.reload()
                result.setdefault("reloads_at_s", []).append(round(time.monotonic() - started))
                next_reload = next_reload + args.reload_every if args.reload_every else None
            time.sleep(0.5)

        result["loads"] = {}
        for name, process in loads:
            try:
                process.wait(timeout=30)
            except subprocess.TimeoutExpired:
                result.setdefault("hang_diag", {})[name] = diagnose(manager, ports[2:4])
                process.kill()
                process.wait()
                result.setdefault("hung", []).append(name)
            text = (work / f"load-{name}.log").read_text()
            values = dict(line.split("=", 1) for line in text.splitlines() if "=" in line)
            result["loads"][name] = {"exit": process.returncode, "ok": values.get("ok"), "errors": values.get("errors"),
                                     "connects": values.get("connects"),
                                     "reconnects": values.get("reconnects"),
                                     "first_error": values.get("first_error")}
        time.sleep(12)
        result["idle_after"] = sample(server.pid, (upstream, upstream + 1))
        result["retired_after"] = manager.call("GET", "/api/v1/generations")["generation"]["retiredGenerations"]
        # The origin goes away: the pool must notice and close what it kept idle, without a request to find out.
        origin.kill()
        origin.wait()
        time.sleep(2)
        result["origin_stopped"] = sample(server.pid, (upstream, upstream + 1))
        result["request_id_missing_after"] = request_id_probes(proxy, tls)
        server.send_signal(signal.SIGTERM)
        result["exit_code"] = server.wait(timeout=60)
        result["proxy_log_tail"] = (work / "proxy.log").read_text().splitlines()[-5:]
    finally:
        for _, process in loads:
            if process.poll() is None:
                process.kill()
        for process in processes:
            if process.poll() is None:
                process.kill()
        shutil.rmtree(work, ignore_errors=True)

    failed = [name for name, load in result.get("loads", {}).items() if load["exit"] != 0 or load["errors"] != "0"]
    failed += result.get("hung", [])
    reloads = len(result.get("reloads_at_s", []))
    workers = lambda flags: int(flags[flags.index("--conns") + 1]) * int(flags[flags.index("--streams") + 1] if "--streams" in flags else 1)
    # A reload ends each connection at most once (the retired generation closes it); more is a connection lost for another reason.
    excess = [name for name, load in result.get("loads", {}).items()
              if int(load["reconnects"] or 0) > workers(mixes[name]) * reloads]
    verdict = {
        "load_failures": failed,
        "reconnects_above_one_per_reload": excess,
        "fd_growth": (result["idle_after"]["fds"] - result["idle_after"]["to_origin"])
        - (result["baseline"]["fds"] - result["baseline"]["to_origin"]),
        "retired_generations_left": len(result["retired_after"]),
        "origin_connections_left_after_origin_stop": result["origin_stopped"]["to_origin"],
        "clean_exit": result.get("exit_code") == 0,
        "request_id_missing": result["request_id_missing_before"] + result["request_id_missing_after"],
    }
    print(json.dumps({**result, "verdict": verdict}, indent=1))
    failed += excess
    clean = verdict["clean_exit"] and not verdict["request_id_missing"] and not verdict["retired_generations_left"] \
        and not verdict["origin_connections_left_after_origin_stop"]
    return 0 if not failed and clean else 1


if __name__ == "__main__":
    sys.exit(main())
