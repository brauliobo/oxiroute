#!/usr/bin/env python3
"""Competing downloads over a shared 100 Mbit bottleneck in an unprivileged user and network namespace.

Usage: lane.py --flows bbr,cubic [--rtt-ms 20] [--buffer-bdp 1] [--rate-mbit 100] [--warmup 15] [--measure 30]
               [--cpus 16-21] [--out FILE.jsonl] [--label NAME]

Flows: `bbr` and `cubic` are HTTP/3 listeners of separate OxiRoute processes (http3_congestion_control), `tcp` is an
HTTP/2 over TLS listener (kernel cubic). Each flow is one curl connection issuing sequential requests for a 48 MiB
file. After the warmup, a window of `--measure` seconds yields per flow the bytes delivered at the client side of
the bottleneck (IP bytes, retransmissions included), the packets that entered the queue and did not come out (loss), the
server CPU per MiB delivered, and the bottleneck queue backlog sampled every 100 ms. The record carries Jain's index
over the delivered rates and the `some avg10` CPU pressure before and after the run. Wrap every run in `timeout`.
"""

import argparse
import json
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))

import h3_impair
import topology
from flows import client_command, make_payload, pick_flows, write_config

TICK_S = 0.1
MIB = 1 << 20


def parse():
    parser = argparse.ArgumentParser()
    parser.add_argument("--flows", required=True)
    parser.add_argument("--rtt-ms", type=float, default=20)
    parser.add_argument("--buffer-bdp", type=float, default=1)
    parser.add_argument("--rate-mbit", type=int, default=100)
    parser.add_argument("--warmup", type=float, default=15)
    parser.add_argument("--measure", type=float, default=30)
    parser.add_argument("--cpus", default="16-21")
    parser.add_argument("--binary", default=os.environ.get("OXIROUTE_BIN", ""))
    parser.add_argument("--keep-offload", action="store_true")
    parser.add_argument("--label", default="")
    parser.add_argument("--out", default="")
    return parser.parse_args()


def cpu_list(text):
    first, _, last = text.partition("-")
    return list(range(int(first), int(last or first) + 1))


def start_servers(args, flows, work):
    cpus = cpu_list(args.cpus)
    servers = {}
    for index, flow in enumerate(flows):
        config = write_config(flow, work, int((args.warmup + args.measure + 60) * 1000))
        pair = f"{cpus[2 * index]}-{cpus[2 * index + 1]}"
        servers[flow.name] = h3_impair.spawn([args.binary, "serve", str(config)], pair, work / f"{flow.name}.log",
                                              {"OXIROUTE_INTERNAL_TEST_DIRECT_RUNTIME": "1", "RUST_LOG": "warn"})
    return servers


def wait_ready(flows, servers):
    for flow in flows:
        wait = h3_impair.wait_udp if flow.transport == "udp" else h3_impair.wait_tcp
        wait(flow.port, servers[flow.name], host=topology.SERVER_IP)


def start_clients(args, flows, holder, work):
    cpus = cpu_list(args.cpus)
    max_time = int(args.warmup + args.measure + 30)
    clients = {}
    for index, flow in enumerate(flows):
        command = client_command(flow, holder, cpus[2 * len(flows) + index], max_time)
        clients[flow.name] = subprocess.Popen(command, stdout=open(work / f"{flow.name}.curl", "w"),
                                           stderr=subprocess.STDOUT)
    return clients


def snapshot(holder, servers):
    return {"time": time.monotonic(), "tx": topology.read_counters(),
            "rx": topology.read_counters(topology.client_prefix(holder)),
            "cpu": {name: h3_impair.cpu_seconds(process.pid) for name, process in servers.items()},
            "bottleneck": topology.read_bottleneck(), "tcp": topology.read_tcp_segments()}


def sample_window(args, holder, servers):
    started = time.monotonic()
    while time.monotonic() - started < args.warmup:
        time.sleep(TICK_S)
    first, backlog = snapshot(holder, servers), []
    while time.monotonic() - started < args.warmup + args.measure:
        backlog.append(topology.read_bottleneck()["backlog_bytes"])
        time.sleep(TICK_S)
    return first, snapshot(holder, servers), backlog


def percentile(values, fraction):
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(fraction * len(ordered)))]


def jain(rates):
    return sum(rates) ** 2 / (len(rates) * sum(rate * rate for rate in rates))


def loss_record(flow, first, last):
    if flow.transport == "tcp":
        segments = {key: last["tcp"][key] - first["tcp"][key] for key in last["tcp"]}
        return {"tcp_retrans_pct": round(100 * segments["TcpRetransSegs"] / segments["TcpOutSegs"], 3)}
    sent_pkts = last["tx"][flow.name][0] - first["tx"][flow.name][0]
    delivered_pkts = last["rx"][flow.name][0] - first["rx"][flow.name][0]
    return {"path_loss_pct": round(100 * (sent_pkts - delivered_pkts) / sent_pkts, 3)}


def flow_record(flow, first, last):
    seconds = last["time"] - first["time"]
    delivered_bytes = last["rx"][flow.name][1] - first["rx"][flow.name][1]
    cpu_s = last["cpu"][flow.name] - first["cpu"][flow.name]
    return {"delivered_mbit_s": round(delivered_bytes * 8 / seconds / 1e6, 3),
            "cpu_ms_per_mib": round(cpu_s * 1000 / (delivered_bytes / MIB), 3), "delivered_mib": delivered_bytes / MIB,
            **loss_record(flow, first, last)}


def queue_record(first, last, backlog, limit_bytes):
    return {"limit_bytes": limit_bytes, "mean_bytes": round(statistics.mean(backlog)),
            "p95_bytes": percentile(backlog, 0.95), "max_bytes": max(backlog),
            "dropped_pkts": last["bottleneck"]["dropped_pkts"] - first["bottleneck"]["dropped_pkts"],
            "sent_pkts_server": sum(last["tx"][name][0] - first["tx"][name][0] for name in last["tx"])}


def assert_clients_running(clients, work):
    for name, client in clients.items():
        if client.poll() is not None:
            raise RuntimeError(f"client {name} exited {client.returncode}: {(work / f'{name}.curl').read_text()}")


def measure(args, flows, work):
    holder = topology.start_client_namespace()
    servers, clients = {}, {}
    try:
        limit = int(topology.bdp_bytes(args.rate_mbit, args.rtt_ms) * args.buffer_bdp)
        topology.build_link(holder, args.rtt_ms, args.rate_mbit, limit, args.keep_offload)
        topology.install_counters(holder, flows)
        make_payload(work)
        servers = start_servers(args, flows, work)
        wait_ready(flows, servers)
        pressure = h3_impair.cpu_pressure()
        clients = start_clients(args, flows, holder, work)
        first, last, backlog = sample_window(args, holder, servers)
        assert_clients_running(clients, work)
        return record(args, flows, first, last, backlog, limit, pressure)
    finally:
        for process in [*clients.values(), *servers.values(), holder]:
            process.terminate()
            process.wait(timeout=30)


def record(args, flows, first, last, backlog, limit, pressure):
    per_flow = {flow.name: flow_record(flow, first, last) for flow in flows}
    rates = [item["delivered_mbit_s"] for item in per_flow.values()]
    total = sum(rates)
    for item in per_flow.values():
        item["share"] = round(item["delivered_mbit_s"] / total, 4)
    return {"label": args.label, "flows": per_flow, "jain": round(jain(rates), 4), "total_mbit_s": round(total, 3),
            "rtt_ms": args.rtt_ms, "buffer_bdp": args.buffer_bdp, "rate_mbit": args.rate_mbit,
            "queue": queue_record(first, last, backlog, limit),
            "psi_cpu_some_avg10_before": pressure, "psi_cpu_some_avg10_after": h3_impair.cpu_pressure()}


def main():
    args = parse()
    topology.enter_namespace()
    work = Path(tempfile.mkdtemp(prefix="fairness-"))
    try:
        result = measure(args, pick_flows(args.flows), work)
    finally:
        shutil.rmtree(work, ignore_errors=True)
    text = json.dumps(result)
    print(text)
    if args.out:
        with open(args.out, "a", encoding="utf-8", newline="\n") as handle:
            handle.write(text + "\n")


if __name__ == "__main__":
    main()
