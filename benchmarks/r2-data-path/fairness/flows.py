"""The competing flows of the fairness lane: one OxiRoute process per flow, each downloading a static file from a
single curl client (sequential requests on one connection), pinned to its own CPUs."""

import json
import os
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

from topology import SERVER_IP, client_prefix

HERE = Path(__file__).resolve().parent
HOST = "bench.example.com"
PAYLOAD_MIB = 48
REQUESTS_PER_CLIENT = 20
CONTROL_BBR = 'http3_congestion_control="bbr"'
CONTROL_CUBIC = 'http3_congestion_control="cubic"'
NO_OFFLOAD = "http3_segmentation_offload=false"


@dataclass(frozen=True)
class Flow:
    name: str
    transport: str
    port: int
    curl_protocol: str
    settings: tuple = ()
    features: tuple = ("fixed_b1k", "static")


FLOWS = {
    "bbr": Flow("bbr", "udp", 18443, "--http3-only", (CONTROL_BBR, NO_OFFLOAD), ("h3", "fixed_b1k", "static")),
    "cubic": Flow("cubic", "udp", 18444, "--http3-only", (CONTROL_CUBIC, NO_OFFLOAD),
                  ("h3", "fixed_b1k", "static")),
    "tcp": Flow("tcp", "tcp", 18445, "--http2", (), ("tls", "fixed_b1k", "static")),
}


def pick_flows(names):
    return [FLOWS[name] for name in names.split(",")]


def listener_bind(flow):
    kind = "udp" if flow.transport == "udp" else "socket"
    return json.dumps({"type": kind, "address": f"{SERVER_IP}:{flow.port}"})


def write_config(flow, work, io_timeout_ms):
    command = [sys.executable, str(HERE.parent / "gen_config.py"), str(work / f"{flow.name}.lua"),
               "--proxy-port", str(flow.port), "--upstream-port", str(flow.port + 100), "--run-dir", str(work),
               "--io-timeout-ms", str(io_timeout_ms), "--listener-set", f"bind={listener_bind(flow)}"]
    for feature in flow.features:
        command += ["--feature", feature]
    for setting in flow.settings:
        command += ["--set", setting]
    subprocess.run(command, check=True)
    return work / f"{flow.name}.lua"


def make_payload(work):
    (work / "www" / "static").mkdir(parents=True)
    block = os.urandom(1 << 20)
    with open(work / "www" / "static" / "f", "wb") as handle:
        for _ in range(PAYLOAD_MIB):
            handle.write(block)


def client_command(flow, holder, cpu, max_time_s):
    command = [*client_prefix(holder), "taskset", "-c", str(cpu), "curl", "-sS", "-k", flow.curl_protocol,
               "--max-time", str(max_time_s), "--resolve", f"{HOST}:{flow.port}:{SERVER_IP}"]
    for _ in range(REQUESTS_PER_CLIENT):
        command += ["-o", "/dev/null", f"https://{HOST}:{flow.port}/static/f"]
    return command
