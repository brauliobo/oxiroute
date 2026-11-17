# Benchmarks and evidence

Use these measurements to understand specific tradeoffs and to design your own evaluation.
They do not establish production capacity or a general ranking of proxies.

## Evidence status

The HTTP/1.1, cache, TLS handshake and HTTP/3 headline rows below were re-measured on 2026-11-17 on the final 7.0.0
tree (`1d39e69d`, release profile, fat LTO, no profile-guided optimization), two 4 s runs per row. Rows that were not
re-measured (OpenSSL, TLS 1.2, resumption, kernel TLS static file, compressed cache hits, memory, HTTP/3 congestion
control comparison) keep their 2026-11-16/17 figures from the integration build (`c204c924` plus the integration
changes, version string `6.5.2`) and are labelled so. The complete tables, run counts, ranges, per-row CPU pressure
and sources are in [benchmarks/reports/7.0](../benchmarks/reports/7.0/README.md). The raw run records and the
nginx/HAProxy harness (`benchmarks/scripts/vs_nginx_haproxy.py`) are local development material and are not part
of this checkout, so the comparison columns cannot be reproduced from the repository alone.

Figures that appeared on this page for the 6.5 line were a maintainer's historical summary without recoverable
provenance. They are superseded; the 6.5.0 values remain in the [6.5.0 release notes](RELEASE_NOTES_6.5.0.md).

## Workload and method

- One machine of the class "16-core (32 logical CPU) desktop CPU", Linux 7.3, shared with unrelated jobs.
  `/proc/pressure/cpu` was recorded around every row (below 3%) and never used to wait or retry.
- One proxy worker pinned to 2 CPUs, the origin to 2 and the load generator to 4; loopback; closed loop; 32
  connections unless stated. nginx 1.30 and HAProxy 3.4 with minimal proxy configurations, one worker each.
- Load generator and origin: `r2tool` ([benchmarks/r2-data-path/tool](../benchmarks/r2-data-path/tool)). Two
  runs of 4 s after a 2 s warm-up per row (up to three in the earlier pass); a run is discarded when the scheduler records more than 1.5 CPUs of
  foreign work on the pinned CPUs. A row with fewer than 2 clean runs is **indicative**.
- `instr/req` is user-space instructions per request attached to the proxy process (`perf stat`); kernel work is
  excluded. Latencies come from a closed-loop generator and carry coordinated omission.
- `epoll` is the default HTTP/1 data plane, `tokio` the same binary with `http_data_plane = "tokio"` (the 6.5 path).

## HTTP/1.1 reverse proxy, 1 KiB, keep-alive (2026-11-17, final tree)

| Connections | Proxy | req/s median (min-max) | p99 µs | instr/req | clean |
| ---: | --- | ---: | ---: | ---: | ---: |
| 32 | OxiRoute epoll | 169,706 (167,747-171,664) | 302 | 14,712 | 2/2 |
| 32 | OxiRoute tokio plane | 128,043 (127,413-128,673) | 303 | 23,862 | 2/2 |
| 32 | nginx | 134,205 (single run) | 403 | 19,877 | 1/2, indicative |
| 32 | HAProxy | 121,704 (121,154-122,253) | 368 | 35,245 | 2/2 |
| 256 | OxiRoute epoll | 169,498 (168,632-170,364) | 2,278 | 14,714 | 2/2 |
| 256 | OxiRoute tokio plane | 124,830 (124,611-125,048) | 2,808 | 23,801 | 2/2 |
| 256 | nginx | 135,754 (134,371-137,138) | 2,311 | 20,286 | 2/2 |
| 1,024 | OxiRoute epoll | 170,641 (169,272-172,010) | 8,318 | 14,710 | 2/2 |
| 1,024 | OxiRoute tokio plane | 116,555 (115,937-117,173) | 165,548 | 23,822 | 2/2 |
| 1,024 | nginx | 99,152 (96,039-102,266) | 16,843 | 20,455 | 2/2 |

The epoll plane spends about 14.7k user instructions per request against 23.8k on the tokio plane, 25.7k in 6.5.2,
19.9k to 20.5k for nginx and 35k to 40k for HAProxy. Before the start-pacing fix (earlier pass), 1,024 connections ran
at 110k requests per second against 164k after, with nginx at 85k in that invocation; the before and after table is in
the report.

## Other HTTP/1.1 workloads (2026-11-17, final tree)

| Workload | OxiRoute epoll | OxiRoute tokio plane | nginx | clean (OxiRoute / nginx) |
| --- | ---: | ---: | ---: | ---: |
| 64 KiB response, req/s | 87,002 | 48,338 | 35,542 | 2/2 / 2/2 |
| 64 KiB response, instr/req | 15,242 | 26,520 | 37,730 | |
| New connection per request, req/s | 88,643 | 71,366 | 74,708 | 2/2 / 2/2 |
| New connection per request, instr/req | 16,256 | 28,666 | 21,460 | |
| Cache hit 1 KiB, req/s | 231,274 | n/a | 128,691 | 2/2 / 1/2, indicative |
| Cache hit 64 KiB, req/s | 97,425 | n/a | 99,966 | 2/2 / 2/2 |

The 64 KiB rows use `splice(2)` for the response body on the epoll plane. The 64 KiB cache hit is level with nginx
(2.5% below, inside the run range); the 1 KiB cache hit is about 1.8 times nginx's rate on this host, with a single
clean nginx run. Compressed cache hits (earlier pass) and the io_uring plane are in the report; the io_uring plane
matches epoll at 1 KiB with 32 connections, is 12% below it at 1,024 connections and slower at 64 KiB, and stays
opt-in.

## TLS

| Lane (one worker, 32 connections) | OxiRoute rustls (default) | OxiRoute openssl | nginx |
| --- | ---: | ---: | ---: |
| Full TLS 1.3 handshake per request, req/s (2026-11-17) | 16,108 | n/a | 5,397 (HAProxy 4,751) |
| Full TLS 1.3 handshake, instr/req (2026-11-17) | 665,649 | n/a | 1,559,140 |
| TLS 1.3 keep-alive 1 KiB, req/s (2026-11-17) | 108,694 (indicative) | n/a | 101,283 |
| Full TLS 1.3 handshake, req/s (earlier) | 14,865 | 5,821 | 5,098 |
| Full TLS 1.3 handshake, instr/req (earlier) | 664,806 | 1,650,828 | 1,559,000 |
| TLS 1.3 keep-alive 1 KiB, req/s (earlier) | 105,040 | 91,429 | 96,362 |
| HTTP/2 over TLS 1 KiB, req/s (earlier) | 107,835 | 92,863 | 94,012 (indicative) |
| 1 MiB static file, req/s, kernel TLS and `sendfile` (earlier) | 4,169 | n/a | 2,195 (user-space TLS) |

The 2026-11-17 rows are two 4 s runs on the final tree (the OxiRoute keep-alive row has one clean run). The earlier
rows are medians of up to three runs of the integration build; the first rows differ only in the `backend` setting of
one binary (2 or 3 clean runs each; the ranges are in the report). An idle TLS connection holds 15.0 kB with rustls,
32.4 kB with OpenSSL and 34.8 kB with nginx (earlier pass).

## HTTP/3

8 QUIC connections of 4 streams, 1 KiB, one worker. Final tree (2026-11-17, one clean run each, indicative): OxiRoute
`bbr` 71,034 requests per second, nginx 67,133; the `cubic` row produced no accepted run. Earlier pass (3/3 clean each):
OxiRoute `bbr` 78,255, `cubic` 79,174, nginx 68,726; OxiRoute used 1.26 CPUs for QUIC. The default `bbr` controller
finishes a 10 MiB download in 0.37 s against 15.0 s for `cubic` at a 40 ms round trip with 0.5% loss; on a loss-free
LAN a 48 MiB download runs at 226 MiB/s against 1,100 MiB/s, and `bbr` takes 99.3% to 99.7% of a bottleneck shared with
`cubic` or TCP flows.
The method and records are checked in: [benchmarks/r2-data-path/README.md](../benchmarks/r2-data-path/README.md) (BBR
measurements) and [fairness results](../benchmarks/r2-data-path/fairness/results/summary.md).

## Memory and idle cost

PSS of the proxy process (earlier pass, not re-measured) with the binary on disk: 13.0 MB idle, 14.8 MB with 32 connections held and 24.2 MB with
1,024 (63.8, 69.2 and 82.1 MB before; single runs). The idle reduction is mostly `thp:never` in the embedded
allocator configuration. An idle direct-runtime process wakes about 0.1 times per second where it woke about 100
times. The binary's file-backed pages add 10 to 12 MB when it runs from tmpfs.

## What these results do not cover

No conclusion follows here about WAN traffic, open-loop tail latency, UDP, RTMP, forward proxying, cache hit rates,
upstream TLS, many-worker scaling, failure recovery, long-lived mixed traffic or production sizing. The shipped
release binaries (profile-guided, built on an `almalinux:9` container) were not measured, and nginx and HAProxy
were not tuned. Feature support belongs in the [compatibility matrix](COMPATIBILITY.md), not in a performance
table. Results from different hosts, builds or load models must remain separate.

## Reproduce and publish a new baseline

The checked-in [R2 data-path harness](../benchmarks/r2-data-path/README.md) measures user-space instructions per
request for the OxiRoute lanes (`run.sh --lanes default`), HTTP/3 over impaired links (`h3_impair.py`) and the BBR
fairness matrix. It does not run nginx or HAProxy. A publishable replacement of the comparison columns should include:

1. Exact clean source revision, binary hashes, dependency versions, compiler flags and build profile.
2. CPU model/topology, RAM, OS/kernel, governor, affinity and interference measurements, with generic
   machine labels and redacted environment records suitable for publication.
3. Checked-in proxy configurations and a versioned load-generator/harness entry point; equal worker
   counts, matched connection reuse, TLS and logging policy, and origin/load-generator capacity checks.
4. Explicit request/response sizes, duration, warm-up, concurrency, arrival model, correctness checks,
   error counts, rejection rules, every raw run and a deterministic aggregation command.
5. Throughput, p50/p95/p99, errors, CPU/request and clearly scoped memory measurements. Keep request
   latency and CPU time distinct; record whether softirq and kernel socket memory are included.
6. Interleaved repetitions with ranges, a dedicated low-noise rerun where practical, and an explanation
   of what the workload cannot establish. Retain disappointing and inconclusive results.
