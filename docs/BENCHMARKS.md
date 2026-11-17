# Benchmarks and evidence

Use these measurements to understand specific tradeoffs and to design your own evaluation.
They do not establish production capacity or a general ranking of proxies.

## Evidence status

The tables below are the HTTP/1.1, cache, TLS and HTTP/3 measurements of the 7.0.0 release, copied from
[benchmarks/reports/7.0](../benchmarks/reports/7.0/README.md), which holds the complete tables, run counts, ranges
and sources. They were taken on the integration build of the release (release profile, fat LTO, no profile-guided
optimization, version string `6.5.2`), not on the shipped profile-guided binaries. The raw run records and the
nginx/HAProxy harness (`benchmarks/scripts/vs_nginx_haproxy.py`) are local development material and are not part
of this checkout, so the comparison columns cannot be reproduced from the repository alone.

Figures that appeared on this page for the 6.5 line were a maintainer's historical summary without recoverable
provenance. They are superseded; the 6.5.0 values remain in the [6.5.0 release notes](RELEASE_NOTES_6.5.0.md).

## Workload and method

- One machine of the class "16-core (32 logical CPU) desktop CPU", Linux 7.3, shared with unrelated jobs.
  `/proc/pressure/cpu` was recorded around every cell (mostly below 3%) and never used to wait or retry.
- One proxy worker pinned to 2 CPUs, the origin to 2 and the load generator to 4; loopback; closed loop; 32
  connections unless stated. nginx 1.30 and HAProxy 3.4 with minimal proxy configurations, one worker each.
- Load generator and origin: `r2tool` ([benchmarks/r2-data-path/tool](../benchmarks/r2-data-path/tool)). Up to
  three runs of 4 s after a 2 s warm-up per cell; a run is discarded when the scheduler records more than 1.5 CPUs of
  foreign work on the pinned CPUs. The `clean` column is runs kept of 3 attempts; a cell with fewer than 2 clean runs
  is **indicative**.
- `instr/req` is user-space instructions per request attached to the proxy process (`perf stat`); kernel work is
  excluded. Latencies come from a closed-loop generator and carry coordinated omission.
- `epoll` is the default HTTP/1 data plane, `tokio` the same binary with `http_data_plane = "tokio"` (the 6.5 path).

## HTTP/1.1 reverse proxy, 1 KiB, keep-alive

| Connections | Proxy | req/s median (min-max) | p99 µs | instr/req | clean |
| ---: | --- | ---: | ---: | ---: | ---: |
| 32 | OxiRoute epoll | 173,925 (169,253-174,339) | 216 | 14,651 | 3/3 |
| 32 | OxiRoute tokio plane | 127,338 (126,202-128,474) | 294 | 23,864 | 2/3 |
| 32 | nginx | 138,608 (single run) | 410 | 19,918 | 1/3, indicative |
| 256 | OxiRoute epoll | 171,902 (171,743-173,296) | 1,565 | 14,650 | 3/3 |
| 256 | OxiRoute tokio plane | 125,220 (125,005-125,435) | 2,206 | 23,788 | 2/3 |
| 256 | nginx | 137,712 (136,539-137,740) | 1,987 | 20,297 | 3/3 |
| 1,024 | OxiRoute epoll | 169,040 (168,964-169,116) | 6,403 | 14,652 | 2/3 |
| 1,024 | OxiRoute tokio plane | 118,891 (118,222-119,061) | 168,924 | 23,809 | 3/3 |
| 1,024 | nginx | 108,036 (106,930-109,306) | 10,339 | 20,474 | 3/3 |

A repeat on a busier host gave 163,952 (epoll), 116,855 (tokio plane) and 132,472 (nginx, indicative) at 32
connections. The epoll plane spends about 14.7k user instructions per request against 23.9k on the tokio plane and
25.7k in 6.5.2, and 19.9k to 20.5k for nginx. Before the start-pacing fix, 1,024 connections ran at 110k requests per
second (164k after, nginx 85k in that invocation); the before and after table is in the report.

## Other HTTP/1.1 workloads

| Workload | OxiRoute epoll | OxiRoute tokio plane | nginx | clean (OxiRoute / nginx) |
| --- | ---: | ---: | ---: | ---: |
| 64 KiB response, req/s | 88,879 | 49,791 | 36,195 | 3/3 / 3/3 |
| 64 KiB response, instr/req | 15,228 | 26,513 | 37,740 | |
| New connection per request, req/s | 89,608 | 71,537 | 72,726 | 3/3 / 2/3 |
| New connection per request, instr/req | 16,513 | 28,498 | 21,464 | |
| Cache hit 1 KiB, req/s | 238,757 | n/a | 127,842 | 3/3 / 2/3 |
| Cache hit 64 KiB, req/s | 101,260 | n/a | 100,708 | 3/3 / 2/3 |

The 64 KiB rows use `splice(2)` for the response body on the epoll plane. The 64 KiB cache hit is level with nginx;
the 1 KiB cache hit is about twice nginx's rate on this host. Compressed cache hits and the io_uring plane are in the
report; the io_uring plane matches epoll at 1 KiB but is slower at 64 KiB and stays opt-in.

## TLS

| Lane (one worker, 32 connections) | OxiRoute rustls (default) | OxiRoute openssl | nginx |
| --- | ---: | ---: | ---: |
| Full TLS 1.3 handshake per request, req/s | 14,865 | 5,821 | 5,098 |
| Full TLS 1.3 handshake, instr/req | 664,806 | 1,650,828 | 1,559,000 |
| TLS 1.3 keep-alive 1 KiB, req/s | 105,040 | 91,429 | 96,362 |
| HTTP/2 over TLS 1 KiB, req/s | 107,835 | 92,863 | 94,012 (indicative) |
| 1 MiB static file, req/s, kernel TLS and `sendfile` | 4,169 | n/a | 2,195 (user-space TLS) |

Rows are medians of up to three 4 s runs; the first rows differ only in the `backend` setting of one binary (2 or 3
clean runs each; the ranges are in the report). An idle TLS connection holds 15.0 kB with rustls, 32.4 kB with OpenSSL and
34.8 kB with nginx.

## HTTP/3

8 QUIC connections of 4 streams, 1 KiB, one worker: OxiRoute `bbr` 78,255 requests per second, `cubic` 79,174, nginx
68,726 (3/3 clean each); OxiRoute used 1.26 CPUs for QUIC. The default `bbr` controller finishes a 10 MiB download in
0.37 s against 15.0 s for `cubic` at a 40 ms round trip with 0.5% loss; on a loss-free LAN a 48 MiB download runs at
226 MiB/s against 1,100 MiB/s, and `bbr` takes 99.3% to 99.7% of a bottleneck shared with `cubic` or TCP flows.
The method and records are checked in: [benchmarks/r2-data-path/README.md](../benchmarks/r2-data-path/README.md) (BBR
measurements) and [fairness results](../benchmarks/r2-data-path/fairness/results/summary.md).

## Memory and idle cost

PSS of the proxy process with the binary on disk: 13.0 MB idle, 14.8 MB with 32 connections held and 24.2 MB with
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
