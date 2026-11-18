# Benchmarks and evidence

Use these measurements to understand specific tradeoffs and to design your own evaluation.
They do not establish production capacity or a general ranking of proxies.

## Evidence status

The rows labelled **OxiRoute 7.0** come from a maintainer's measurement on 2026-11-17 of the published 7.0.0
x86_64 release binary (profile-guided; tarball SHA-256 `5081e6aa...a506c`), run with the nginx and HAProxy rows in the
same invocation. The remaining rows transcribe a maintainer's historical **integrated 6.5 performance summary**,
whose build preceded the 6.5.2 release; they were not re-measured and keep their original labels. The complete 7.0
tables, including the tokio and `io_uring` planes, 64 KiB, 1 MiB, 1,024 connections, cache, compression, HTTP/2,
HTTP/3, two workers and memory, are in the
[7.0 report](../benchmarks/reports/7.0/README.md).

The underlying research notes and harness are local development material, not part of this public
checkout. The 6.5 summary does not supply a recoverable exact source SHA, binary hashes, dated run IDs,
complete machine description, or the raw samples needed to independently validate these tables.
Treat them as **reported observations**, not an accepted release baseline. Version-family
labels identify what the summary reports; they are insufficient to reconstruct the exact binaries.

## Workload and method

The 7.0 re-measurement and the historical 6.5 summary record:

- A shared 16-core (32-logical-CPU) host with unrelated work running. Absolute throughput varied by up to 35%
  between invocations of the 6.5 summary. The complete hardware and kernel provenance is not published with this page.
- One proxy worker, with the proxy assigned two CPUs and the origin and load generator assigned
  separate CPUs; loopback traffic; 32 connections; interleaved turns for each proxy.
- 6.5 rows: three accepted 8-second measured runs per cell after a 3-second warm-up. Disturbed runs were
  discarded and repeated. The summary's rejection rule and all rejected samples are not available here.
- 7.0 rows: two 4-second runs after a 2-second warm-up, at most two attempts per row; an attempt is discarded when
  the scheduler records more than 1.5 CPUs of foreign work on the pinned CPUs. A row with one clean run is marked
  indicative and shows that single sample. CPU pressure (`/proc/pressure/cpu`, `some` avg10) stayed at or below
  7.9% in these rows and was never used to discard a run.
- 6.5 rows: release builds with fat LTO and one codegen unit, without profile-guided optimization. 7.0 rows: the
  published profile-guided binary (build record: tree `c01445fe`, `pgo: true`, `bolt: false`). 7.0 TLS rows use
  OxiRoute's default rustls backend. The 7.0 comparison is nginx 1.30.6 and
  HAProxy 3.4.7; the 6.5 summary has no complete patch-version, dependency and compiler provenance. The 7.0 harness is a local
  script that is not part of this checkout.
- Medians and observed minimum–maximum ranges, rather than confidence intervals. Latencies come
  from a closed-loop load generator; they do not model a production arrival rate or remove coordinated
  omission. A p99 here is meaningful only at the stated concurrency and workload.

`CPU µs/request` is proxy process CPU time divided by completed requests: user plus system time in the 6.5
rows, scheduler-accounted CPU time of the proxy process in the 7.0 rows, so the two groups are not strictly
comparable. It excludes loopback softirq work, which the original summary records separately. It is neither elapsed latency
nor a universal hardware-independent cost. Keep these metrics separate when comparing runs.

## Throughput, latency and CPU

Each row comes from one invocation of that workload; no best-run selection or pooling
across different builds is applied (the 7.0 keep-alive row is the second invocation of its lane after a first with
one clean run for two proxies). Bodies are 1 KiB. H1 means HTTP/1.1; TLS keep-alive reuses an
established TLS 1.3 connection. The handshake row opens a new TLS 1.3 ECDSA connection per request. Rows labelled 6.5 are from the 6.5 summary; the other rows are 7.0 measurements.

| Workload | Proxy | Requests/s, median (min–max) | p50, µs | p99, µs | CPU µs/request |
| --- | --- | ---: | ---: | ---: | ---: |
| H1 proxy, keep-alive | OxiRoute 7.0 | 178,396 (177,389–179,402) | 174 | 254 | 5.5 |
| H1 proxy, keep-alive | nginx | 134,061 (131,708–136,414) | 232 | 350 | 7.4 |
| H1 proxy, keep-alive | HAProxy | 114,860 (114,644–115,076) | 267 | 390 | 8.7 |
| H1 proxy, new connection | OxiRoute 7.0 | 91,432 (88,800–94,065) | 322 | 752 | 10.5 |
| H1 proxy, new connection | nginx (earlier non-PGO pass) | 74,708 (74,429–74,987) | 406 | 702 | 13.2 |
| H1 proxy, new connection | HAProxy (one clean run, indicative) | 62,840 | 481 | 741 | 15.7 |
| H1 POST, 1 KiB body | OxiRoute 7.0 (one clean run, indicative) | 180,102 | 171 | 314 | 5.5 |
| H1 POST, 1 KiB body | nginx (6.5) | 126,375 (125,847–139,378) | 238 | 714 | 5.3 |
| H1 POST, 1 KiB body | HAProxy | 114,382 (113,089–115,676) | 266 | 398 | 8.6 |
| TLS keep-alive | OxiRoute 7.0 | 128,113 (126,656–129,570) | 247 | 416 | 7.7 |
| TLS keep-alive | nginx | 96,906 (95,519–98,293) | 324 | 442 | 10.2 |
| TLS keep-alive | HAProxy | 84,794 (81,487–88,101) | 360 | 636 | 11.6 |
| H2 over TLS | OxiRoute 7.0 | 132,781 (131,965–133,597) | 240 | 374 | 7.5 |
| H2 over TLS | nginx (one clean run, indicative) | 105,442 | 301 | 350 | 9.5 |
| H2 over TLS | HAProxy | 85,054 (85,030–85,079) | 369 | 546 | 11.6 |
| TLS full handshake | OxiRoute 7.0 | 16,092 (16,043–16,141) | 1,970 | 2,248 | 86.8 |
| TLS full handshake | nginx | 5,131 (5,102–5,160) | 6,208 | 6,797 | 194.5 |
| TLS full handshake | HAProxy | 4,674 (4,666–4,681) | 6,756 | 9,654 | 213.2 |

Where all three proxies were measured in the 7.0 pass (plain keep-alive, established TLS, HTTP/2, TLS handshake)
OxiRoute is ahead of nginx and HAProxy in throughput and CPU cost, by 33% over nginx on plain keep-alive, 32% on
established TLS and 26% on HTTP/2 (one clean nginx run). The nginx new-connection row has no clean run in the
shipped-binary pass and the 6.5 nginx POST row was not re-measured. A
full TLS handshake costs substantially more CPU than an established connection; the 7.0 default rustls
backend completes about three times as many per second as nginx here, where the optional OpenSSL backend was lower in the earlier 7.0 pass (see the report). The H2
rows use 8 connections of 4 streams. These observations do not support a claim that OxiRoute is generally faster or
cheaper.

A later recheck in the maintainer summary did not reproduce an apparent handshake regression against
6.4.0; this page therefore makes no regression claim. A later H2 rerun recorded zero accepted clean
runs and is excluded. The optional OpenSSL backend's 7.0 results are in the
[7.0 report](../benchmarks/reports/7.0/README.md).

## Idle connection memory

The memory evaluation (7.0 binary, measured 2026-11-17) used fresh processes, one worker per proxy, two runs, and
page-cache normalization before starting the OxiRoute processes. Values are **incremental worker RSS bytes per
connection**: resident memory with idle connections minus empty-worker RSS, divided by connection
count. They exclude kernel socket memory and other processes and are not total memory budgets.
The first four workloads used 5,000 connections; H2 used 1,000 connections after eight closed streams each.
OxiRoute uses the default rustls backend. Values are medians with min–max over the two runs. The 6.5 summary's
OpenSSL-backend figures (for example 8,699 bytes for H1 keep-alive and 29,931 over TLS) were replaced by these.

| Idle workload | OxiRoute 7.0, bytes | nginx, bytes | HAProxy, bytes |
| --- | ---: | ---: | ---: |
| WebSocket | 12,526 (12,526–12,526) | 18,364 (18,358–18,371) | 5,693 (5,106–6,280) |
| WebSocket over TLS | 21,634 (21,633–21,635) | 59,832 (59,829–59,835) | 31,188 (30,071–32,304) |
| H1 keep-alive | 1,749 (1,747–1,751) | 9,989 (9,980–9,998) | 11,728 (11,669–11,787) |
| H1 keep-alive over TLS | 16,749 (16,472–17,026) | 34,784 (34,759–34,809) | 35,762 (35,662–35,863) |
| H2 over TLS | 29,864 (29,426–30,302) | 48,792 (48,472–49,111) | 47,074 (46,928–47,219) |

The same invocation reports lower incremental RSS than nginx for these workloads; HAProxy is lower
for idle WebSocket connections. Busy connections, request bodies, caches, queues, retained generations,
and total process memory require separate measurements.

## What these results do not cover

No conclusion follows here about WAN traffic, open-loop tail latency, UDP, RTMP, H3, forward proxying,
cache hit rates, upstream TLS, many-core scaling, failure recovery, long-lived mixed traffic, or
production sizing. Feature support belongs in the [compatibility matrix](COMPATIBILITY.md), not in a
performance table. Results from different hosts, builds or load models must remain separate.

## Reproduce and publish a new baseline

This checkout does not include the complete historical harness or raw evidence. There is no honest
one-command reproduction of the tables above. A publishable replacement should include:

1. Exact clean source revision, binary hashes, dependency versions, compiler flags and build profile.
2. CPU model/topology, RAM, OS/kernel, governor, affinity and interference measurements; generic
   machine labels and redacted environment records suitable for publication.
3. Checked-in proxy configurations and a versioned load-generator/harness entry point; equal worker
   counts, matched connection reuse, TLS and logging policy, and origin/load-generator capacity checks.
4. Explicit request/response sizes, duration, warm-up, concurrency, arrival model, correctness checks,
   error counts, rejection rules, every raw run and a deterministic aggregation command.
5. Throughput, p50/p95/p99, errors, CPU/request and clearly scoped memory measurements. Keep request
   latency and CPU time distinct; record whether softirq and kernel socket memory are included.
6. Interleaved repetitions with ranges, a dedicated low-noise rerun where practical, and an explanation
   of what the workload cannot establish. Retain disappointing and inconclusive results.

Build compilation should run separately from measurement, with `nice` and at most four build jobs.
Verify response correctness before measuring speed. Never turn a failed or incomplete measurement
into a successful-looking fallback value.

Return to the [README](../README.md) or [developer guide](developer/README.md).
