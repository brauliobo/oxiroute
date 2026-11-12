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
sys.path.insert(0, str(HERE))
from gen_config import server_address  # noqa: E402
LOCK_FILE = "/tmp/oxiroute-bench.lock"
PERF_SUDO = ["sudo", "-n"] if os.environ.get("R2_PERF_SUDO") else []  # kernel-side events (e.g. instructions:k) need root here
PERF_EXTRA = [event for event in os.environ.get("R2_PERF_EXTRA", "").split(",") if event]  # e.g. cache-misses:u
LANE_TIMEOUT = 600  # seconds; a hung load run must never hold the shared bench lock
SIZES = {"b0": 0, "b1k": 1024, "b64k": 65536, "b1m": 1048576}

PORT_FIRST, PORT_LAST, PORT_SPAN = int(os.environ.get("R2_PORT_BASE", "20300")), 20499, 40
UPSTREAM_PORT = ECHO_PORT = PROXY_PORT = 0
PROXY_CPUS = os.environ.get("R2_PROXY_CPUS", "8-11")
UPSTREAM_CPUS = os.environ.get("R2_UPSTREAM_CPUS", "12-13")
LOAD_CPUS = os.environ.get("R2_LOAD_CPUS", "14-15")
STORM_CPUS = os.environ.get("R2_STORM_CPUS", "6-7")
BEARER_TOKEN = "r2-bench-token-0123456789abcdef-0123456789abcdef"
BASIC_CREDENTIAL = "YmVuY2g6YmVuY2hwYXNz"  # bench:benchpass
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
    handshake_threads: int = -1  # `tls_handshake_threads`; -1 leaves the setting out (automatic), 0 handshakes inline
    servers: int = 1
    options: tuple = ()  # root options of the generated configuration as KEY=JSON
    storm: int = 0  # TLS handshakes per second that new connections add during the measured window (`r2tool storm`)
    target_offset: int = 0  # proxy port offset the load goes to (forward listener: 3, TCP listener: 1)
    requires: tuple = ()  # (what the proxy must support, the `oxiroute config check` rejection of a binary that lacks it)
    note: str = ""


H3_ORIGIN = ("an http3 listener that proxies to an HTTP/1.1 origin pool",
             "HTTP/3 reverse routes require an exact HTTP/3 upstream pool")


class Unsupported(Exception):
    """The proxy binary lacks a capability the lane needs: no number is better than a bogus one."""


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
    add(lane("h1_post1k", ["--method", "POST", "--path", "/sink", "--body-bytes", "1024"] + base,
             requests=20000, warmup=1000))
    for size, requests in ((1024, 20000), (65536, 5000)):
        add(lane(f"h1_post{size // 1024}k_chunked", ["--method", "POST", "--path", "/sink", "--body-bytes", str(size),
                                                    "--chunk-bytes", "16384" if size > 16384 else str(size)] + base,
                 requests=requests, warmup=min(requests // 10, 1000)))
    add(lane("h1_post64k_reqbuf", ["--method", "POST", "--path", "/sink", "--body-bytes", "65536"] + base,
             features=["reqbuf"], requests=5000, warmup=200))
    add(lane("h1_post64k_retrybuf", ["--method", "POST", "--path", "/sink", "--body-bytes", "65536"] + base,
             features=["reqbuf", "retry_buffered"], requests=5000, warmup=200))
    # Per-feature A/B at the same workload: 16 keep-alive connections, 1 KiB body.
    work = ["--path", "/b1k", "--expect-len", "1024"] + base
    add(lane("f_base", work))
    add(lane("f_fixed", ["--path", "/fixed"] + base, features=["fixed"]))
    add(lane("f_fixed_log", ["--path", "/fixed"] + base, features=["fixed", "access_log"]))
    for feature in FEATURES_6_0 + ["nginx_host"]:
        add(lane(f"f_{feature}", work, features=[feature]))
    add(lane("f_forwarded", work, features=["forwarded"]))
    add(lane("f_all_6_0", work, features=FEATURES_6_0))
    add(lane("f_prod", work, features=["forwarded", "access_log", "timeouts", "passive", "breaker", "outlier", "capacity"]))
    add(lane("gzip_html64k", ["--path", "/html64k", "--extra-header", "accept-encoding: gzip"] + base,
             features=["gzip"], requests=3000, warmup=100))
    add(lane("nogzip_html64k", ["--path", "/html64k"] + base, features=["gzip"], requests=3000, warmup=100))
    add(lane("cache_hit_b1k", work, features=["cache"]))
    add(lane("cache_hit_b64k", ["--path", "/b64k", "--expect-len", "65536"] + base, features=["cache"], requests=5000))
    for name, size in (("static_f1k", 1024), ("static_f64k", 65536), ("static_f1m", 1048576)):
        add(lane(name, ["--path", f"/static/{name[7:]}.bin", "--expect-len", str(size)] + base, features=["static"],
                 requests=20000 if size < 65536 else 5000 if size == 65536 else 1000))
    # Static files over TLS: user-space records against kernel TLS (`ktls = "auto"`, needs `modprobe tls`).
    for name, features in (("tls_static", ["tls", "static"]), ("tls_static_ktls", ["tls", "static", "ktls"])):
        for size, label in ((65536, "f64k"), (131072, "f128k"), (262144, "f256k"), (1048576, "f1m")):
            add(lane(f"{name}_{label}", ["--proto", "h1tls", "--path", f"/static/{label}.bin",
                                         "--expect-len", str(size)] + base,
                     features=features, requests=5000 if size < 262144 else 2000 if size < 1048576 else 1000, warmup=100))
    # Small answers on a connection that already moved a large file (what kernel TLS keeps for the connection's life),
    # and large files mixed into small requests at one per `every` requests of a connection.
    small = ["--proto", "h1tls", "--conns", "16", "--expect-len", "1024"]
    for name, features in (("user", ["tls", "static"]), ("ktls", ["tls", "static", "ktls"])):
        add(lane(f"tls_plain_{name}", small + ["--path", "/b1k"], features=features, requests=20000, warmup=1000))
        for label, path in (("b1k", "/b1k"), ("f1k", "/static/f1k.bin")):
            add(lane(f"tls_conv_{name}_{label}", small + ["--path", path, "--prime-path", "/static/f1m.bin"],
                     features=features, requests=20000, warmup=1000))
        for every in (10, 50, 200, 1000):
            add(lane(f"tls_mix{every}_{name}", small + ["--path", "/static/f1k.bin", "--mix-path", "/static/f1m.bin",
                                                         "--mix-len", "1048576", "--mix-every", str(every)],
                     features=features, requests=3000 if every <= 50 else 20000, warmup=200))
    add(lane("ws_echo_64", ["--proto", "ws", "--conns", "16", "--msg-bytes", "64"]))
    add(lane("ws_echo_16k", ["--proto", "ws", "--conns", "16", "--msg-bytes", "16384"], requests=10000))
    add(lane("ws_echo_1k", ["--proto", "ws", "--conns", "16", "--msg-bytes", "1024"]))
    add(lane("l4_echo_1k", ["--proto", "tcp", "--conns", "16", "--msg-bytes", "1024"], features=["l4"]))
    add(lane("l4_echo_16k", ["--proto", "tcp", "--conns", "16", "--msg-bytes", "16384"], features=["l4"],
             requests=10000))
    add(lane("l4_echo_64k", ["--proto", "tcp", "--conns", "16", "--msg-bytes", "65536"], features=["l4"],
             requests=5000))
    # Paced: the relay is idle between messages, so the cost per message does not depend on how busy the host is.
    for size in (16384, 65536, 262144):
        add(lane(f"l4_echo_{size // 1024}k_paced", ["--proto", "tcp", "--conns", "16", "--msg-bytes", str(size),
                                                    "--pace-ms", "2"], features=["l4"], requests=3000, warmup=100))
    add(lane("l4_echo_256k", ["--proto", "tcp", "--conns", "16", "--msg-bytes", "262144"], features=["l4"],
             requests=3000, warmup=100))
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
        ("tls_hs_browser_tls13", ["tls"], ["--tls-version", "1.3", "--tls-groups", "browser"]),
        ("tls_hs_browser_kx_tls13", ["tls", "tls_kx"], ["--tls-version", "1.3", "--tls-groups", "browser"]),
    ):
        add(lane(name, tls + ["--new-conn", "--conns", "16"] + extra, features=features, requests=3000, warmup=200))
    add(lane("tls_h1_b1m", ["--proto", "h1tls", "--path", "/b1m", "--expect-len", str(SIZES["b1m"])] + base,
             features=["tls"], requests=1500, warmup=100))
    add(lane("h2_b1k_c4s16", ["--proto", "h2tls", "--path", "/b1k", "--conns", "4", "--streams", "16",
                              "--expect-len", "1024"], features=["tls"], requests=20000))
    add(lane("h2_b1k_log_c4s16", ["--proto", "h2tls", "--path", "/b1k", "--conns", "4", "--streams", "16",
                                  "--expect-len", "1024"], features=["tls", "access_log"], requests=20000))
    add(lane("h2_fixed_b1k_c4s16", ["--proto", "h2tls", "--path", "/fixed", "--conns", "4", "--streams", "16",
                                    "--expect-len", "1024"], features=["tls", "fixed_b1k"], requests=20000))
    add(lane("h2_b64k_c4s16", ["--proto", "h2tls", "--path", "/b64k", "--conns", "4", "--streams", "16",
                               "--expect-len", "65536"], features=["tls"], requests=5000, warmup=200))
    add(lane("h2_post1k_c4s16", ["--proto", "h2tls", "--method", "POST", "--path", "/sink", "--body-bytes", "1024",
                                 "--conns", "4", "--streams", "16"], features=["tls"], requests=20000))
    # HTTP/2 uploads at the default windows (1 MiB stream, 8 MiB connection) and at the 65,535 of releases before 6.4.
    post1m = ["--proto", "h2tls", "--method", "POST", "--path", "/sink", "--body-bytes", "1048576", "--conns", "4",
              "--streams", "4"]
    add(lane("h2_post1m_c4s4", post1m, features=["tls"], requests=800, warmup=40))
    add(lane("h2_post1m_c4s4_oldwin", post1m, features=["tls", "h2_win_old"], requests=800, warmup=40))
    add(lane("h2_post1k_c4s16_oldwin", ["--proto", "h2tls", "--method", "POST", "--path", "/sink", "--body-bytes", "1024",
                                        "--conns", "4", "--streams", "16"], features=["tls", "h2_win_old"],
             requests=20000))
    # HTTP/3 listener (`r2tool load --proto h3`: quinn + h3 client, self-signed development certificate, no verification
    # as in the TLS lanes). `fixed_*` lanes answer from the listener itself; the others proxy to the HTTP/1.1 origin.
    h3 = ["--proto", "h3", "--conns", "8", "--streams", "4"]
    add(lane("h3_fixed_b1k_c8s4", h3 + ["--path", "/fixed", "--expect-len", "1024"], features=["h3", "fixed_b1k"],
             requests=16000))
    add(lane("h3_fixed_b1k_c1s1", ["--proto", "h3", "--path", "/fixed", "--expect-len", "1024"],
             features=["h3", "fixed_b1k"], requests=10000))
    add(lane("h3_fixed_b64k_c8s4", h3 + ["--path", "/fixed", "--expect-len", "65536"], features=["h3", "fixed_b64k"],
             requests=5000, warmup=200))
    add(lane("h3_b1k_c8s4", h3 + ["--path", "/b1k", "--expect-len", "1024"], features=["h3"], requests=16000,
             requires=H3_ORIGIN))
    add(lane("h3_post1k_c8s4", h3 + ["--method", "POST", "--path", "/sink", "--body-bytes", "1024"], features=["h3"],
             requests=16000, requires=H3_ORIGIN))
    # One listener over several workers (`listener_workers`; needs workers + 1 proxy CPUs): HTTP/3 against the HTTP/2 lane.
    for workers in (1, 4):
        wide = ["--conns", "32", "--streams", "4"]
        add(lane(f"h3_fixed_b1k_c32s4_w{workers}", ["--proto", "h3", "--path", "/fixed", "--expect-len", "1024"] + wide,
                 features=["h3", "fixed_b1k"], requests=32000, workers=workers))
        add(lane(f"h3_b1k_c32s4_w{workers}", ["--proto", "h3", "--path", "/b1k", "--expect-len", "1024"] + wide,
                 features=["h3"], requests=32000, workers=workers, requires=H3_ORIGIN))
        add(lane(f"h2_b1k_c32s4_w{workers}", ["--proto", "h2tls", "--path", "/b1k", "--expect-len", "1024"] + wide,
                 features=["tls"], requests=32000, workers=workers))
    add(lane("h3_newconn_fixed_b1k", ["--proto", "h3", "--new-conn", "--conns", "8", "--path", "/fixed",
                                      "--expect-len", "1024"], features=["h3", "fixed_b1k"], requests=2000, warmup=100))
    add(lane("h1_b1k_h2up", ["--path", "/b1k", "--expect-len", "1024"] + base, features=["upstream_h2"]))
    add(lane("h1_b1k_tlsup", ["--path", "/b1k", "--expect-len", "1024"] + base, features=["upstream_tls"]))
    add(lane("h1_b64k_h2up", ["--path", "/b64k", "--expect-len", "65536"] + base, features=["upstream_h2"],
             requests=5000, warmup=200))
    add(lane("h1_post1k_h2up", ["--method", "POST", "--path", "/sink", "--body-bytes", "1024"] + base,
             features=["upstream_h2"]))
    # Data-path latency of established connections while new ones arrive (open-loop handshakes per second).
    for rate in (1000, 3000):
        for name, threads in (("", -1), ("_inline", 0)):
            add(lane(f"tls_storm{rate}{name}_h2_c1s1", ["--proto", "h2tls", "--path", "/b1k", "--conns", "1",
                                                      "--streams", "1", "--expect-len", "1024"],
                     features=["tls"], requests=20000, storm=rate, handshake_threads=threads))
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
    # Many connections on a multi-thread listener: how evenly the workers share the connections.
    for workers, conns in ((2, 256), (2, 2048)):
        add(lane(f"scale_w{workers}_c{conns}", ["--path", "/b1k", "--expect-len", "1024", "--conns", str(conns)],
                 requests=60000, warmup=conns * 2, workers=workers))
    for algorithm in ("lc", "wrr"):
        for servers in (8, 64, 200):
            add(lane(f"pool_{algorithm}_s{servers}", work, features=[algorithm], servers=servers))
    for feature in FEATURES_6_0:
        add(lane(f"scale4_{feature}", ["--path", "/b1k", "--expect-len", "1024", "--conns", "128"], features=[feature],
                 requests=80000, warmup=4000, listeners=4))
    add(lane("fwd_h1_b1k", ["--path", "http://127.0.0.1:{UP}/b1k", "--expect-len", "1024"] + base, features=["forward"],
             target_offset=3))
    add(lane("fwd_connect_1k", ["--proto", "connect", "--connect-to", "127.0.0.1:{ECHO}", "--conns", "16", "--msg-bytes", "1024"],
             features=["forward", "forward_connect", "l4"], target_offset=3))
    add(lane("fwd_connect_newconn", ["--proto", "connect", "--connect-to", "127.0.0.1:{ECHO}", "--new-conn", "--conns", "16",
                                     "--msg-bytes", "1024"], features=["forward", "forward_connect", "l4"], target_offset=3, requests=10000,
             warmup=500))
    add(lane("l4_echo_newconn", ["--proto", "tcp", "--new-conn", "--conns", "16", "--msg-bytes", "1024"], features=["l4"],
             requests=10000, warmup=500))
    add(lane("pool_s2", work, servers=2))
    add(lane("pool_s64", work, servers=64))
    add(lane("pool_s64_passive", work, features=["passive", "breaker", "outlier"], servers=64))
    add(lane("pool_s1_passive", work, features=["passive", "breaker", "outlier"]))
    # Per-feature cost on top of the plain proxy route (6.5 feature cost study).
    add(lane("f_stats", work, features=["stats"]))
    bearer = ["--extra-header", f"authorization: Bearer {BEARER_TOKEN}"]
    basic = ["--extra-header", f"authorization: Basic {BASIC_CREDENTIAL}"]
    add(lane("f_auth_bearer", work + bearer, features=["auth_bearer"]))
    # At most four password checks run at once: more concurrent clients are answered 401, so these lanes use one.
    one = ["--path", "/b1k", "--expect-len", "1024", "--conns", "1"]
    add(lane("f_auth_basic_apr1", one + basic, features=["auth_basic_apr1"], requests=2000, warmup=100))
    for cost, requests in ((5, 400), (10, 60)):
        add(lane(f"f_auth_basic_bcrypt{cost}", one + basic,
                 features=["auth_basic_bcrypt", f"bcrypt_cost_{cost}"], requests=requests, warmup=10))
    add(lane("f_redirect", ["--path", "/redirect", "--expect-status", "301"] + base, features=["redirect"]))
    add(lane("f_path_rewrite", ["--path", "/app/b1k", "--expect-len", "1024"] + base, features=["path_rewrite"]))
    add(lane("f_cookie_plain", ["--path", "/cookie1k", "--expect-len", "1024"] + base))
    add(lane("f_cookie_rewrite", ["--path", "/cookie1k", "--expect-len", "1024"] + base, features=["cookie_rewrite"]))
    add(lane("f_select", work + ["--extra-header", "x-size: 50"], features=["select"]))
    for name, header in (("cookie", []), ("hash", []), ("header", ["--extra-header", "x-backend: origin-3"])):
        add(lane(f"f_affinity_{name}_s20", work + header, features=[f"affinity_{name}"], servers=20))
    add(lane("f_affinity_none_s20", work, servers=20))
    for level in (1, 4, 6, 9):
        add(lane(f"gzip_l{level}_html64k", ["--path", "/html64k", "--extra-header", "accept-encoding: gzip"] + base,
                 features=[f"gzip_l{level}"], requests=2000, warmup=100))
    add(lane("gzip_on_b1k", work, features=["gzip"]))  # gzip configured, response not compressible
    add(lane("gzip_on_b1k_nogzip_client", work, features=["gzip_l1"]))
    add(lane("cache_miss_b1k", ["--path", "/b1k", "--vary", "true", "--expect-len", "1024"] + base, features=["cache"]))
    add(lane("cache_nocache_vary_b1k", ["--path", "/b1k", "--vary", "true", "--expect-len", "1024"] + base))
    add(lane("cache_reval_b1k", ["--path", "/rv1k", "--expect-len", "1024"] + base, features=["cache_revalidate"]))
    for algorithm_name, features in (("rr", []), ("p2c", ["p2c"]), ("first", ["first"]), ("lc", ["lc"]), ("wrr", ["wrr"])):
        for servers in (2, 20, 250):
            add(lane(f"alg_{algorithm_name}_s{servers}", work, features=features, servers=servers,
                     requests=20000, warmup=max(2000, servers * 4)))
    idle = ["--path", "/b0", "--expect-len", "0", "--conns", "1"]
    for servers in (1, 20, 250):
        for name, features in (("none", []), ("hc_tcp", ["hc_tcp"]), ("hc_http", ["hc_http"])):
            add(lane(f"idle_{name}_s{servers}", idle, features=features, servers=servers, requests=50, warmup=10))
    add(lane("idle_canary_s1", idle, features=["canary"], requests=50, warmup=10))
    add(lane("idle_stats", idle, features=["stats"], requests=50, warmup=10))
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


def wait_udp(port, process, timeout=30.0):
    """A QUIC server answers a datagram of an unknown version with Version Negotiation: that proves it is bound."""
    probe = bytes([0xC0]) + bytes.fromhex("1a2a3a4a") + bytes([8]) + os.urandom(8) + bytes([8]) + os.urandom(8)
    probe += bytes(1200 - len(probe))
    deadline = time.monotonic() + timeout
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as udp:
        udp.settimeout(0.2)
        while time.monotonic() < deadline:
            if process.poll() is not None:
                raise RuntimeError(f"process exited before listening on udp {port}")
            try:
                udp.sendto(probe, ("127.0.0.1", port))
                udp.recv(2048)
                return
            except (socket.timeout, ConnectionRefusedError):
                time.sleep(0.05)
    raise RuntimeError(f"udp port {port} not ready")


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
        for name, size in (("f1k", 1024), ("f64k", 65536), ("f128k", 131072), ("f256k", 262144), ("f1m", 1048576)):
            (www / f"{name}.bin").write_bytes(b"x" * size)
        token = self.work / "token"
        token.write_text(BEARER_TOKEN + "\n")
        token.chmod(0o600)
        htpasswd = self.work / "htpasswd"
        cost = ["-B", "-C", str(next((int(name[12:]) for name in self.lane.features if name.startswith("bcrypt_cost_")), 5))]
        hashed = subprocess.run(["htpasswd", "-nb", *(cost if "auth_basic_bcrypt" in self.lane.features else ["-m"]), "bench", "benchpass"],
                                check=True, capture_output=True, text=True).stdout
        htpasswd.write_text(hashed)
        htpasswd.chmod(0o600)

    def handshake_threads(self):
        """`--tls-handshake-threads` overrides the lane's setting (-1: automatic, 0: handshakes inline)."""
        override = self.args.tls_handshake_threads
        return self.lane.handshake_threads if override is None else override

    def render_config(self):
        lane = self.lane
        self.prepare_files()
        config = self.work / "oxiroute.lua"
        generated = subprocess.run(
            [sys.executable, str(HERE / "gen_config.py"), str(config), "--proxy-port", str(PROXY_PORT),
             "--upstream-port", str(UPSTREAM_PORT), "--echo-port", str(ECHO_PORT), "--servers", str(lane.servers),
             "--listeners", str(lane.listeners), "--workers", str(lane.workers),
             "--tls-handshake-threads", str(self.handshake_threads()), "--run-dir", str(self.work)]
            + (["--classic"] if self.args.classic else [])
            + (["--baseline"] if self.args.baseline else [])
            + ["--tls-backend", self.args.tls_backend]
            + [item for option in lane.options for item in ("--set", option)]
            + [item for feature in lane.features for item in ("--feature", feature)],
            check=True, capture_output=True, text=True, timeout=LANE_TIMEOUT)
        self.generated = generated.stdout
        return config

    def start(self, wrap=(), proxy_env=None):
        lane = self.lane
        config = self.render_config()
        if lane.servers > 2:  # many servers share one port on 127.0.A.B
            listen = ",".join(server_address(index, UPSTREAM_PORT) for index in range(lane.servers))
        else:
            listen = ",".join(f"127.0.0.1:{UPSTREAM_PORT + index}" for index in range(lane.servers))
        tls = self.upstream_tls() if {"upstream_h2", "upstream_tls"} & set(lane.features) else []
        self.upstream = spawn([self.args.tool, "upstream", "--listen", listen,
                               "--echo-listen", f"127.0.0.1:{ECHO_PORT}", *tls], UPSTREAM_CPUS, self.work / "upstream.log")
        wait_port(UPSTREAM_PORT, self.upstream)
        # The direct runtime keeps the proxy in one process; the supervised worker adds no data-path code.
        self.proxy = spawn([self.binary, "serve", str(config)], PROXY_CPUS, self.work / "proxy.log", wrap,
                           {"OXIROUTE_INTERNAL_TEST_DIRECT_RUNTIME": "1", "RUST_LOG": "warn",
                            "R2_PROXY_PORT": str(PROXY_PORT), "R2_UPSTREAM_PORT": str(UPSTREAM_PORT),
                            **dict(item.split("=", 1) for item in self.args.proxy_env), **(proxy_env or {})})
        for port in self.http_ports():
            (wait_udp if "h3" in lane.features else wait_port)(port, self.proxy)

    def upstream_tls(self):
        """A self-signed upstream certificate that the generated pool trusts as its CA (`origin.pem`)."""
        cert, key = self.work / "origin.pem", self.work / "origin.key"
        subprocess.run(["openssl", "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:prime256v1", "-nodes",
                        "-days", "2", "-subj", "/CN=origin.example.com", "-addext", "subjectAltName=DNS:origin.example.com",
                        "-keyout", str(key), "-out", str(cert)], check=True, capture_output=True)
        return ["--tls-cert", str(cert), "--tls-key", str(key)]

    def stop(self):
        for process in (self.proxy, self.upstream):
            if process is not None:
                stop(process)

    def close(self):
        self.stop()
        shutil.rmtree(self.work, ignore_errors=True)

    def start_storm(self):
        storm = spawn([self.args.tool, "storm", "--target", f"127.0.0.1:{PROXY_PORT}", "--rate", str(self.lane.storm)],
                      STORM_CPUS, self.work / "storm.log")
        time.sleep(1.0)  # let the handshake rate settle before the measured window opens
        return storm

    def load_command(self, requests, warmup, hooks=()):
        lane = self.lane
        targets = ",".join(f"127.0.0.1:{port}" for port in self.http_ports())
        if "--proto" in lane.load and lane.load[lane.load.index("--proto") + 1] == "tcp":
            targets = f"127.0.0.1:{PROXY_PORT + 1}"
        if lane.target_offset:
            targets = f"127.0.0.1:{PROXY_PORT + lane.target_offset}"
        command = [self.args.tool, "load", "--target", targets, "--requests", str(requests), "--warmup", str(warmup)]
        command += [item.replace("{UP}", str(UPSTREAM_PORT)).replace("{ECHO}", str(ECHO_PORT)) for item in lane.load]
        for name, value in hooks:
            command += [f"--{name}", value]
        return ["taskset", "-c", LOAD_CPUS] + command


def parse_kv(text):
    return {key: value for key, value in (line.split("=", 1) for line in text.splitlines() if "=" in line)}


def context_switches(path):
    text = Path(path).read_text()
    return sum(int(value) for value in re.findall(r"^voluntary_ctxt_switches:\s+(\d+)", text, re.M)), \
        sum(int(value) for value in re.findall(r"^nonvoluntary_ctxt_switches:\s+(\d+)", text, re.M))


def thread_usage(before, after, elapsed):
    """(busiest-thread shares as percent of one CPU, kernel share of all proxy CPU time in percent, CPU seconds) over the window."""
    ticks = os.sysconf("SC_CLK_TCK")

    def usage(path):
        # /proc/PID/task/TID/stat: the command is parenthesised and may hold spaces; utime and stime are fields 14 and 15
        rows = (line.rsplit(")", 1) for line in Path(path).read_text().splitlines())
        return {line[0].split()[0]: tuple(int(value) for value in line[1].split()[11:13]) for line in rows}

    def names(path):
        rows = (line.rsplit(")", 1) for line in Path(path).read_text().splitlines())
        return {line[0].split()[0]: line[0].split("(", 1)[1] for line in rows}

    start, end, comm = usage(before), usage(after), names(after)
    spent = {tid: [now - was for now, was in zip(end[tid], start.get(tid, (0, 0)))] for tid in end}
    shares = sorted((((user + kernel) * 100 / ticks / elapsed, comm[tid]) for tid, (user, kernel) in spent.items()),
                    reverse=True)
    user, kernel = (sum(times[index] for times in spent.values()) for index in (0, 1))
    return ([f"{name}:{round(share)}" for share, name in shares[:4]], round(100 * kernel / max(user + kernel, 1)),
            (user + kernel) / ticks)


def event_key(event):
    """Counter name without its modifier; kernel-side events keep it (`instructions:k` -> `instructions_k`)."""
    name, _, modifier = event.partition(":")
    return f"{name}_k" if modifier == "k" else name


def parse_perf(path):
    counters = {}
    for line in Path(path).read_text().splitlines():
        fields = line.split(",")
        if len(fields) > 2 and (fields[0].replace(".", "", 1).isdigit() or fields[0] == "<not counted>"):
            counters[event_key(fields[2])] = float(fields[0]) if fields[0][0] != "<" else 0.0  # a thread that never ran
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
        [*PERF_SUDO, "env", "LC_ALL=C", "perf", "stat", "-D", "-1", "-x,", "-e", ",".join(["instructions:u", "cycles:u", *PERF_EXTRA]), "-p", str(env.proxy.pid),
         "--control", f"fifo:{ctl},{ack}", "-o", str(csv)], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    pid = env.proxy.pid
    switches = lambda tag: (f"cat /proc/{pid}/task/*/status > {env.work}/sw{rep}.{tag}; "
                            f"cat /proc/{pid}/task/*/stat > {env.work}/st{rep}.{tag}")
    hooks = [("hook-start", f"{switches('a')}; echo enable > {ctl}; read _ < {ack}"),
             ("hook-stop", f"echo disable > {ctl}; read _ < {ack}; {switches('b')}")]
    lane = env.lane
    storm = env.start_storm() if lane.storm else None
    try:
        result = subprocess.run(env.load_command(lane.requests, lane.warmup, hooks), capture_output=True, text=True,
                                timeout=LANE_TIMEOUT)
    finally:
        if storm is not None:
            stop(storm)
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
    busy, kernel_pct, cpu_seconds = thread_usage(env.work / f"st{rep}.a", env.work / f"st{rep}.b", elapsed)
    return {
        "busy": busy, "kernel_pct": kernel_pct, "cpu_us": cpu_seconds * 1e6 / requests,
        "instr": (counters["instructions"] - idle["instructions"] * elapsed) / requests,
        "cycles": (counters["cycles"] - idle["cycles"] * elapsed) / requests,
        "extra": {event_key(event): counters.get(event_key(event), 0) / requests for event in PERF_EXTRA},
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
        "idle_instr_per_s": round(idle["instructions"]),
        "instr_samples": "/".join(str(round(value)) for value in column("instr")),
        "instr_per_req": round(median(column("instr"))), "instr_spread_pct": round(spread(column("instr")), 2),
        "cycles_per_req": round(median(column("cycles"))), "cycles_spread_pct": round(spread(column("cycles")), 2),
        "vctxsw_per_req": round(median(column("ctxsw")), 3), "nvctxsw_per_req": round(median(column("nvctxsw")), 3),
        **{f"ev_{name}_per_req": round(median([sample["extra"][name] for sample in samples]), 1) for name in samples[0]["extra"]},
        "rps": round(median(column("rps"))), "rps_spread_pct": round(spread(column("rps")), 1),
        "p50_us": round(median(column("p50"))), "p99_us": round(median(column("p99"))),
        "p999_us": round(median(column("p999"))), "connect_p50_us": round(median(column("connect_p50"))),
        "connects": round(median(column("connects"))), "errors": sum(column("errors")), "rss_kb": status["rss_kb"], "hwm_kb": status["hwm_kb"],
        "threads": status["threads"], "fds": status["fds"], "busy_threads_pct": "/".join(map(str, samples[-1]["busy"])),
        "kernel_cpu_pct": round(median(column("kernel_pct"))), "cpu_us_per_req": round(median(column("cpu_us")), 1),
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
    top = sorted(per_request.items(), key=lambda item: -item[1])[: args.sys_top]
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
    """Allocator calls and requested bytes per request, counted between two snapshots by an LD_PRELOAD shim (libc
    allocations and `clock_gettime`) or, with `--alloc-internal`, by a binary built with `--features alloc-count`
    (its jemalloc; the shim cannot see it)."""
    env = Env(args, lane)
    snapshots = env.work / "alloc.txt"
    counted = ({"OXIROUTE_INTERNAL_TEST_ALLOC_FILE": str(snapshots)} if args.alloc_internal
               else {"LD_PRELOAD": str(build_alloc_shim(args)), "R2_ALLOC_FILE": str(snapshots)})
    try:
        env.start(proxy_env=counted)
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


def slab_bytes(slabinfo):
    """Kernel slab bytes by cache and their sum, without the ZFS caches that background disk activity grows."""
    caches = {}
    for line in slabinfo.splitlines()[2:]:
        fields = line.split()
        if not fields[0].startswith(("zio_", "zfs_", "sa_cache", "dnode", "dmu_", "arc_", "abd_", "zil_", "lz4")):
            caches[fields[0]] = int(fields[2]) * int(fields[3])
    return caches, sum(caches.values()) // 1024


def read_slabinfo():
    return subprocess.run(["sudo", "-n", "cat", "/proc/slabinfo"], capture_output=True, text=True, check=True).stdout


def measure_memory(args, lane):
    """RSS per idle keep-alive connection, sampled while the N connections are open and idle (one second after the last
    request), and again after the load tool has closed them: `rss_after_close_bytes_per_conn` is what the allocator
    keeps once the connections are gone, not their footprint. The live-byte delta of the allocator shim only sees libc
    allocations, so it is 0 while jemalloc is the global allocator."""
    env = Env(args, lane)
    snapshots = env.work / "mem.txt"
    connections = args.mem_conns
    try:
        env.start(proxy_env={"LD_PRELOAD": str(build_alloc_shim(args)), "R2_ALLOC_FILE": str(snapshots)})
        pid = env.proxy.pid
        rss_before = proc_status(pid)["rss_kb"]
        kernel_before = slab_bytes(read_slabinfo())
        subprocess.run(["kill", "-USR1", str(pid)], check=True)
        deadline = time.monotonic() + 5
        while not snapshots.exists() and time.monotonic() < deadline:
            time.sleep(0.02)
        open_status = env.work / "status.open"
        hook = (f"kill -USR1 {pid}; sleep 1; cp /proc/{pid}/status {open_status}; sudo -n cat /proc/slabinfo > {env.work}/slab.txt; "
                f"if [ -e /proc/net/tls_stat ]; then cp /proc/net/tls_stat {env.work}/tls_stat.txt; fi")
        requests = connections * args.mem_streams
        command = env.load_command(requests, requests, [("hook-start", hook)])
        command[command.index("--conns") + 1] = str(connections)
        subprocess.run(command, capture_output=True, text=True, check=True, timeout=LANE_TIMEOUT)
        deadline = time.monotonic() + 5
        while len(snapshots.read_text().splitlines()) < 2 and time.monotonic() < deadline:
            time.sleep(0.02)
        rss_closed = proc_status(pid)["rss_kb"]
        rss_open = int(re.search(r"^VmRSS:\s+(\d+)", open_status.read_text(), re.M).group(1))
        kernel_after = slab_bytes((env.work / "slab.txt").read_text())
        tls_stat = env.work / "tls_stat.txt"  # absent while the kernel tls module is not loaded: no socket can be converted
        converted = int(re.search(r"^TlsCurrTxSw\s+(\d+)", tls_stat.read_text(), re.M).group(1)) if tls_stat.exists() else 0
        first, second = [list(map(int, line.split())) for line in snapshots.read_text().splitlines()[:2]]
    finally:
        env.close()
    growth = sorted(((after - kernel_before[0].get(name, 0), name) for name, after in kernel_after[0].items()), reverse=True)
    return {"mem_conns": connections, "heap_live_bytes_per_conn": round((second[7] - first[7]) / connections),
            "rss_bytes_per_conn": round((rss_open - rss_before) * 1024 / connections),
            "rss_after_close_bytes_per_conn": round((rss_closed - rss_before) * 1024 / connections),
            "kernel_tls_tx_sockets": converted,
            "kernel_slab_bytes_per_conn": round((kernel_after[1] - kernel_before[1]) * 1024 / connections),
            "kernel_slab_caches_bytes_per_conn": " ".join(f"{name}:{round(grown / connections)}" for grown, name in growth[:12]
                                                         if grown / connections >= 24)}


def measure_profile(args, lane):
    """perf record sampled by user-space instructions (deterministic attribution), DWARF call graphs."""
    env = Env(args, lane, args.profile_bin)
    mode = args.profile_call_graph or ("fp" if args.profile_bin else "dwarf,16384")
    branches = mode == "branches"  # raw branch stacks for perf2bolt / function ordering, no report
    call_graph = ["-j", "any,u"] if branches else [] if args.flat else ["--user-callchains", "--call-graph", mode]
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
    if branches:
        return {"profile_requests": int(load.get("ok", 0))}
    views = [("self", ["--no-children", "-g", "none"])]
    if call_graph:
        views.append(("children", ["--children", "-g", "none"]))
    for label, flags in views:
        report = subprocess.run(["perf", "report", "--stdio", "-i", str(data), "--percent-limit", "0.3", *flags, "--sort", "sym"],
                                capture_output=True, text=True, env={**os.environ, "LC_ALL": "C"})
        (data.parent / f"{lane.name}.{label}.txt").write_text(report.stdout)
    return {"profile_requests": int(load.get("ok", 0))}


def train_lane(args, lane):
    """Replay a lane once with no measurement (profile-guided optimisation training): needs no `perf` and no hooks."""
    with bench_lock():
        pick_ports(lane)
        env = Env(args, lane)
        try:
            env.start()
            result = subprocess.run(env.load_command(lane.requests, lane.warmup), capture_output=True, text=True,
                                    timeout=LANE_TIMEOUT)
            load = parse_kv(result.stdout)
            if result.returncode != 0 or int(load.get("errors", -1)) != 0 or int(load.get("ok", 0)) != lane.requests:
                raise RuntimeError(f"training lane {lane.name} failed: {result.stdout} {result.stderr}")
            return {"ok": int(load["ok"]), "rps": float(load["rps"])}
        finally:
            env.close()


def check_support(args, lane):
    """Run `oxiroute config check` on the lane's configuration: a binary that predates what the lane exercises is skipped."""
    if not lane.requires:
        return
    needed, rejection = lane.requires
    with bench_lock():
        pick_ports(lane)
        env = Env(args, lane)
        try:
            checked = subprocess.run([env.binary, "config", "check", str(env.render_config())], capture_output=True, text=True)
        finally:
            env.close()
    output = checked.stdout + checked.stderr
    if checked.returncode != 0 and rejection in output:
        raise Unsupported(f"{env.binary} does not support {needed}")
    if checked.returncode != 0:
        raise RuntimeError(f"oxiroute config check rejected the {lane.name} configuration: {output}")


def run_lane(args, lane):
    check_support(args, lane)
    row = {"lane": lane.name, "features": ",".join(lane.features) or "-", "listeners": lane.listeners}
    if args.train:
        return {**row, **train_lane(args, lane)}
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
    if args.alloc or args.alloc_internal:
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
    parser.add_argument("--tmp", default=os.environ.get("R2_TMP", "/var/tmp"))
    parser.add_argument("--no-perf", dest="perf", action="store_false", help="skip the instruction/latency lane run")
    parser.add_argument("--train", action="store_true",
                        help="replay each lane once without any measurement or perf (pair with --proxy-env "
                             "LLVM_PROFILE_FILE=... on an instrumented binary); fails on any load error")
    parser.add_argument("--sys", action="store_true", help="also count syscalls per request (strace slope)")
    parser.add_argument("--alloc", action="store_true", help="also count allocator calls per request (LD_PRELOAD shim)")
    parser.add_argument("--alloc-internal", action="store_true",
                        help="also count allocator calls per request with the counting jemalloc of a binary built "
                             "with `--features alloc-count` (the shim cannot see jemalloc)")
    parser.add_argument("--mem", action="store_true", help="also measure heap/RSS per idle keep-alive connection")
    parser.add_argument("--mem-conns", type=int, default=2000)
    parser.add_argument("--mem-streams", type=int, default=1, help="requests per connection before RSS is sampled")
    parser.add_argument("--profile", action="store_true", help="also perf-record per-function instruction samples")
    parser.add_argument("--profile-period", type=int, default=20011)
    parser.add_argument("--profile-event", default="instructions:u",
                        help="sampled event; cycles:u attributes cache-line stalls that instruction sampling hides")
    parser.add_argument("--flat", action="store_true", help="--profile without call graphs")
    parser.add_argument("--profile-call-graph", default=None,
                        help="perf --call-graph mode of --profile (default fp with --profile-bin, else dwarf,16384; "
                             "lbr uses the CPU branch records)")
    parser.add_argument("--profile-bin", default=os.environ.get("R2_PROFILE_BIN"),
                        help="frame-pointer build used with --call-graph fp (default: --bin, flat profile)")
    parser.add_argument("--profile-requests", type=int, default=3000)
    parser.add_argument("--profile-warmup", type=int, default=500)
    parser.add_argument("--sys-top", type=int, default=8, help="how many syscall names --sys reports")
    parser.add_argument("--sys-low", type=int, default=1000)
    parser.add_argument("--sys-high", type=int, default=3000)
    parser.add_argument("--alloc-requests", type=int, default=4000)
    parser.add_argument("--alloc-warmup", type=int, default=1000)
    parser.add_argument("--scale", type=float, default=1.0, help="multiply every lane's request counts (smoke runs)")
    parser.add_argument("--tls-backend", choices=["openssl", "rustls"], default="openssl",
                        help="policy.backend of the generated TLS profile")
    parser.add_argument("--tls-handshake-threads", type=int, default=None,
                        help="tls_handshake_threads of every lane (-1: automatic, 0: handshakes inline on the worker)")
    parser.add_argument("--classic", action="store_true",
                        help="generate `http_fast_lane = false`: every request takes the classic path")
    parser.add_argument("--baseline", action="store_true",
                        help="the binary is an older release (6.3.x): generate no `backend` or `ktls` policy options")
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
                try:
                    row = run_lane(args, chosen)
                except Unsupported as unsupported:
                    row = {"lane": name, "skipped": str(unsupported)}
                rows.append(row)
                print(render(row), flush=True)
    (args.out / "results.json").write_text(json.dumps(rows, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
