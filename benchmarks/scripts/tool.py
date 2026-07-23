#!/usr/bin/env python3

import argparse
import hashlib
import json
import os
import pathlib
import platform
import re
import shutil
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request


def write_json(path, value):
    destination = pathlib.Path(path)
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def command_version(command, arguments):
    path = shutil.which(command)
    if path is None:
        return {"available": False, "path": None, "version": None}
    try:
        result = subprocess.run(
            [path, *arguments], capture_output=True, check=False, text=True, timeout=10
        )
        text = (result.stdout + result.stderr).strip().splitlines()
        version = text[0] if text else None
    except (OSError, subprocess.SubprocessError) as error:
        version = f"version query failed: {error.__class__.__name__}"
    return {"available": True, "path": path, "version": version}


def render(args):
    if len(args.replacement) % 2:
        raise SystemExit("render replacements must be TOKEN VALUE pairs")
    source = pathlib.Path(args.input).read_text(encoding="utf-8")
    for token, value in zip(args.replacement[::2], args.replacement[1::2]):
        source = source.replace(f"@{token}@", value)
    unresolved = sorted(set(re.findall(r"@[A-Z][A-Z0-9_]*@", source)))
    if unresolved:
        raise SystemExit(f"unresolved template tokens: {', '.join(unresolved)}")
    destination = pathlib.Path(args.output)
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(source, encoding="utf-8")


def wait_http(args):
    deadline = time.monotonic() + args.timeout
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(args.url, timeout=0.5) as response:
                body = response.read()
                if response.status == 200 and (
                    args.expected_bytes is None or len(body) == args.expected_bytes
                ):
                    return
        except (OSError, urllib.error.URLError):
            time.sleep(0.05)
    raise SystemExit(1)


def port_available(port):
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try:
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.bind(("127.0.0.1", port))
        return True
    except OSError:
        return False
    finally:
        sock.close()


def preflight(args):
    tools = {
        "ab": command_version("ab", ["-V"]),
        "bash": command_version("bash", ["--version"]),
        "haproxy": command_version("haproxy", ["-v"]),
        "nginx": command_version("nginx", ["-v"]),
        "python3": command_version("python3", ["--version"]),
        "taskset": command_version("taskset", ["--version"]),
    }
    oxiroute = pathlib.Path(args.oxiroute_bin)
    tools["oxiroute"] = {
        "available": oxiroute.is_file() and os.access(oxiroute, os.X_OK),
        "path": str(oxiroute),
        "version": None,
    }
    required = {"ab", "bash", "nginx", "python3", "taskset", *args.implementations}
    ok = all(tools[name]["available"] for name in required)
    selected_cpus = [args.proxy_cpu, args.origin_cpu, args.load_cpu]
    online_cpus = sorted(os.sched_getaffinity(0))
    cpu_affinity_ok = len(set(selected_cpus)) == 3 and all(
        cpu in online_cpus for cpu in selected_cpus
    )
    ok = ok and cpu_affinity_ok
    ports = {
        str(args.origin_port): port_available(args.origin_port),
        str(args.proxy_port): port_available(args.proxy_port),
    }
    ok = ok and all(ports.values()) and sys.platform.startswith("linux") and pathlib.Path("/proc").is_dir()
    report = {
        "schema": "oxiroute.local-v1.preflight.v1",
        "ok": ok,
        "implementations": args.implementations,
        "linux": sys.platform.startswith("linux"),
        "proc_available": pathlib.Path("/proc").is_dir(),
        "ports_available": ports,
        "cpu_affinity": {
            "ok": cpu_affinity_ok,
            "online": online_cpus,
            "selected": {
                "proxy": args.proxy_cpu,
                "origin": args.origin_cpu,
                "load_generator": args.load_cpu,
            },
        },
        "tools": tools,
    }
    write_json(args.output, report)
    if not ok:
        raise SystemExit(1)


def read_text_command(arguments):
    try:
        result = subprocess.run(arguments, capture_output=True, check=False, text=True, timeout=10)
    except (OSError, subprocess.SubprocessError):
        return None
    return result.stdout.strip() or result.stderr.strip() or None


def sha256_file(path):
    digest = hashlib.sha256()
    with pathlib.Path(path).open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def environment(args):
    repository = pathlib.Path(args.repository)
    cpu = {}
    cpuinfo = pathlib.Path("/proc/cpuinfo")
    if cpuinfo.is_file():
        for line in cpuinfo.read_text(encoding="utf-8", errors="replace").splitlines():
            if ":" not in line:
                continue
            key, value = (item.strip() for item in line.split(":", 1))
            if key in {"model name", "Hardware"} and "model" not in cpu:
                cpu["model"] = value
    cpu["logical_count"] = os.cpu_count()
    git_head = read_text_command(["git", "-C", str(repository), "rev-parse", "HEAD"])
    git_status = read_text_command(["git", "-C", str(repository), "status", "--porcelain"])
    oxiroute = pathlib.Path(args.oxiroute_bin)
    tools = {
        "ab": command_version("ab", ["-V"]),
        "haproxy": command_version("haproxy", ["-v"]),
        "nginx": command_version("nginx", ["-v"]),
        "phoronix-test-suite": command_version("phoronix-test-suite", ["version"]),
        "python3": command_version("python3", ["--version"]),
        "taskset": command_version("taskset", ["--version"]),
    }
    report = {
        "schema": "oxiroute.local-v1.environment.v1",
        "captured_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "cpu": cpu,
        "git": {"head": git_head, "dirty": bool(git_status)},
        "oxiroute_binary": {
            "path": str(oxiroute),
            "sha256": sha256_file(oxiroute) if oxiroute.is_file() else None,
        },
        "platform": {
            "machine": platform.machine(),
            "release": platform.release(),
            "system": platform.system(),
        },
        "tools": tools,
    }
    write_json(args.output, report)


def run_metadata(args):
    write_json(
        args.output,
        {
            "schema": "oxiroute.local-v1.run.v1",
            "implementation": args.implementation,
            "origin_port": args.origin_port,
            "proxy_port": args.proxy_port,
            "load_generator": "ab",
            "connections": args.connections,
            "warmup_seconds": args.warmup_seconds,
            "duration_seconds": args.duration_seconds,
            "cpu_affinity": {
                "proxy": args.proxy_cpu,
                "origin": args.origin_cpu,
                "load_generator": args.load_cpu,
            },
        },
    )


def summarize_ab(args):
    text = pathlib.Path(args.input).read_text(encoding="utf-8", errors="replace")
    requests_per_second = re.search(
        r"^Requests per second:\s+([0-9]+(?:\.[0-9]+)?)\s+\[#/sec\]", text, re.MULTILINE
    )
    completed = re.search(r"^Complete requests:\s+([0-9]+)\s*$", text, re.MULTILINE)
    failed = re.search(r"^Failed requests:\s+([0-9]+)\s*$", text, re.MULTILINE)
    elapsed = re.search(r"^Time taken for tests:\s+([0-9]+(?:\.[0-9]+)?) seconds\s*$", text, re.MULTILINE)
    transfer = re.search(r"^Transfer rate:\s+(.+?)\s*$", text, re.MULTILINE)
    non_success = re.search(r"^Non-2xx responses:\s+([0-9]+)\s*$", text, re.MULTILINE)
    if requests_per_second is None or completed is None or failed is None or elapsed is None:
        raise SystemExit("ApacheBench output did not contain a complete result")
    report = {
        "schema": "oxiroute.local-v1.result.v1",
        "implementation": args.implementation,
        "requests_per_second": float(requests_per_second.group(1)),
        "requests": int(completed.group(1)),
        "elapsed_seconds": float(elapsed.group(1)),
        "transfer_per_second": transfer.group(1) if transfer else None,
        "non_2xx_or_3xx": int(non_success.group(1)) if non_success else 0,
        "failed_requests": int(failed.group(1)),
    }
    write_json(args.output, report)
    if report["non_2xx_or_3xx"] or report["failed_requests"]:
        raise SystemExit("ApacheBench reported HTTP or request failures")


def skipped_lanes(args):
    manifest = json.loads(pathlib.Path(args.input).read_text(encoding="utf-8"))
    skipped = [lane for lane in manifest["lanes"] if lane["status"] == "skipped"]
    write_json(args.output, {"schema": manifest["schema"], "lanes": skipped})


def result_value(args):
    report = json.loads(pathlib.Path(args.input).read_text(encoding="utf-8"))
    print(report["requests_per_second"])


def build_parser():
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)

    command = commands.add_parser("render")
    command.add_argument("input")
    command.add_argument("output")
    command.add_argument("replacement", nargs="*")
    command.set_defaults(function=render)

    command = commands.add_parser("wait-http")
    command.add_argument("url")
    command.add_argument("timeout", type=float)
    command.add_argument("expected_bytes", type=int, nargs="?")
    command.set_defaults(function=wait_http)

    command = commands.add_parser("preflight")
    command.add_argument("output")
    command.add_argument("oxiroute_bin")
    command.add_argument("origin_port", type=int)
    command.add_argument("proxy_port", type=int)
    command.add_argument("proxy_cpu", type=int)
    command.add_argument("origin_cpu", type=int)
    command.add_argument("load_cpu", type=int)
    command.add_argument("implementations", nargs="+")
    command.set_defaults(function=preflight)

    command = commands.add_parser("environment")
    command.add_argument("output")
    command.add_argument("repository")
    command.add_argument("oxiroute_bin")
    command.set_defaults(function=environment)

    command = commands.add_parser("run-metadata")
    command.add_argument("output")
    command.add_argument("implementation")
    command.add_argument("origin_port", type=int)
    command.add_argument("proxy_port", type=int)
    command.add_argument("connections", type=int)
    command.add_argument("warmup_seconds", type=int)
    command.add_argument("duration_seconds", type=int)
    command.add_argument("proxy_cpu", type=int)
    command.add_argument("origin_cpu", type=int)
    command.add_argument("load_cpu", type=int)
    command.set_defaults(function=run_metadata)

    command = commands.add_parser("summarize-ab")
    command.add_argument("implementation")
    command.add_argument("input")
    command.add_argument("output")
    command.set_defaults(function=summarize_ab)

    command = commands.add_parser("skipped-lanes")
    command.add_argument("input")
    command.add_argument("output")
    command.set_defaults(function=skipped_lanes)

    command = commands.add_parser("result-value")
    command.add_argument("input")
    command.set_defaults(function=result_value)
    return parser


if __name__ == "__main__":
    parsed = build_parser().parse_args()
    parsed.function(parsed)
