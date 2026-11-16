"""Two network namespaces joined by a veth pair with a shared, shaped bottleneck, all inside one unprivileged user
namespace: the servers live in the outer namespace, the clients in a nested one. Data leaves the server through a netem
delay line (half the round trip) and a tbf queue (the bottleneck); acknowledgements come back through a second delay
line. Per-flow packet counters sit before the queue (server output) and after it (client input)."""

import json
import os
import re
import subprocess
import sys

NAMESPACE_MARK = "BBR_FAIRNESS_IN_NAMESPACE"
SERVER_IP = "10.77.0.1"
CLIENT_IP = "10.77.0.2"
TBF_BURST_BYTES = 15000


def enter_namespace():
    if os.environ.get(NAMESPACE_MARK):
        return
    os.environ[NAMESPACE_MARK] = "1"
    unshare = ["unshare", "--user", "--map-root-user", "--net", "--pid", "--fork", "--kill-child", "--mount-proc"]
    os.execvp("unshare", [*unshare, sys.executable, *sys.argv])


def run(*command, **options):
    done = subprocess.run(command, capture_output=True, text=True, **options)
    if done.returncode:
        raise RuntimeError(f"{' '.join(command)}: {done.stderr}")
    return done.stdout


def start_client_namespace():
    holder = subprocess.Popen(["unshare", "--net", "sh", "-c", "echo ready; exec sleep infinity"],
                              stdout=subprocess.PIPE, text=True)
    holder.stdout.readline()
    return holder


def client_prefix(holder):
    return ["nsenter", f"--net=/proc/{holder.pid}/ns/net"]


def bdp_bytes(rate_mbit, rtt_ms):
    return int(rate_mbit * 1e6 / 8 * rtt_ms / 1000)


def configure_server_side(rtt_ms, rate_mbit, limit_bytes):
    run("ip", "link", "set", "lo", "up")
    run("ip", "addr", "add", f"{SERVER_IP}/24", "dev", "s0")
    run("ip", "link", "set", "s0", "up")
    run("tc", "qdisc", "add", "dev", "s0", "root", "handle", "1:", "netem", "delay", f"{rtt_ms / 2}ms",
        "limit", "100000")
    run("tc", "qdisc", "add", "dev", "s0", "parent", "1:1", "handle", "10:", "tbf", "rate", f"{rate_mbit}mbit",
        "burst", str(TBF_BURST_BYTES), "limit", str(limit_bytes))


def configure_client_side(holder, rtt_ms):
    prefix = client_prefix(holder)
    run(*prefix, "ip", "link", "set", "lo", "up")
    run(*prefix, "ip", "addr", "add", f"{CLIENT_IP}/24", "dev", "c0")
    run(*prefix, "ip", "link", "set", "c0", "up")
    run(*prefix, "tc", "qdisc", "add", "dev", "c0", "root", "netem", "delay", f"{rtt_ms / 2}ms", "limit", "100000")


def disable_offload(holder):
    run("ethtool", "-K", "s0", "tso", "off", "gso", "off", "gro", "off")
    run(*client_prefix(holder), "ethtool", "-K", "c0", "tso", "off", "gso", "off", "gro", "off")


def build_link(holder, rtt_ms, rate_mbit, limit_bytes, keep_offload):
    run("ip", "link", "add", "s0", "type", "veth", "peer", "name", "c0", "netns", str(holder.pid))
    if not keep_offload:
        disable_offload(holder)
    configure_server_side(rtt_ms, rate_mbit, limit_bytes)
    configure_client_side(holder, rtt_ms)


def counter_name(flow):
    return f"flow_{flow.name}"


def counter_script(flows, hook):
    lines = ["add table inet fair", f"add chain inet fair tally {{ type filter hook {hook} priority 0; }}"]
    for flow in flows:
        lines.append(f"add counter inet fair {counter_name(flow)}")
        lines.append(f"add rule inet fair tally {flow.transport} sport {flow.port} counter name {counter_name(flow)}")
    return "\n".join(lines) + "\n"


def install_counters(holder, flows):
    run("nft", "-f", "-", input=counter_script(flows, "output"))
    run(*client_prefix(holder), "nft", "-f", "-", input=counter_script(flows, "input"))


def read_counters(prefix=()):
    listing = json.loads(run(*prefix, "nft", "-j", "list", "counters"))["nftables"]
    return {item["counter"]["name"].removeprefix("flow_"): (item["counter"]["packets"], item["counter"]["bytes"])
            for item in listing if "counter" in item}


def read_bottleneck():
    qdiscs = json.loads(run("tc", "-j", "-s", "qdisc", "show", "dev", "s0"))
    tbf = next(item for item in qdiscs if item["kind"] == "tbf")
    return {"dropped_pkts": tbf["drops"], "sent_bytes": tbf["bytes"], "backlog_bytes": tbf["backlog"]}


def read_tcp_segments():
    text = run("nstat", "-az", "TcpOutSegs", "TcpRetransSegs")
    return {name: int(value) for name, value in re.findall(r"^(Tcp\w+)\s+(\d+)", text, re.M)}

