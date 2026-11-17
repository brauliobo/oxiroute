# Benchmarks and evidence

Use these measurements to understand specific tradeoffs and to design your own evaluation.
They do not establish production capacity or a general ranking of proxies.

## Evidence status

The rows labelled **OxiRoute 7.0** come from a maintainer's re-measurement on 2026-11-17 of the final 7.0.0
tree, built with the release profile and without profile guidance (the shipped binaries are profile-guided and were
not measured). The remaining rows transcribe a maintainer's historical **integrated 6.5 performance summary**,
whose build preceded the 6.5.2 release; they were not re-measured and keep their original labels. The complete 7.0
tables, including the tokio and `io_uring` planes, 64 KiB, 1,024 connections, cache, HTTP/3 and memory, are in the
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
  3.3% in these rows and was never used to discard a run.
- Release builds with fat LTO and one codegen unit, without profile-guided optimization. 6.5 TLS rows use
  OxiRoute's OpenSSL backend, 7.0 TLS rows its default rustls backend. The comparison identifies nginx 1.30 and
  HAProxy 3.4, without complete patch-version, dependency and compiler provenance. The 7.0 harness is a local
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
across different builds is applied. Bodies are 1 KiB. H1 means HTTP/1.1; TLS keep-alive reuses an
established TLS 1.3 connection. The handshake row opens a new TLS 1.3 ECDSA connection per request. Rows not labelled 7.0 are from the 6.5 summary.

| Workload | Proxy | Requests/s, median (min–max) | p50, µs | p99, µs | CPU µs/request |
| --- | --- | ---: | ---: | ---: | ---: |
| H1 proxy, keep-alive | OxiRoute 7.0 | 169,706 (167,747–171,664) | 184 | 302 | 5.9 |
| H1 proxy, keep-alive | nginx (one clean run, indicative) | 134,205 | 230 | 403 | 7.4 |
| H1 proxy, keep-alive | HAProxy | 121,704 (121,154–122,253) | 252 | 368 | 8.2 |
| H1 proxy, new connection | OxiRoute 7.0 | 88,643 (88,118–89,168) | 340 | 639 | 11.1 |
| H1 proxy, new connection | nginx | 74,708 (74,429–74,987) | 406 | 702 | 13.2 |
| H1 proxy, new connection | HAProxy | 67,016 (66,047–67,986) | 456 | 683 | 14.8 |
| H1 POST, 1 KiB body | OxiRoute 6.5 integrated | 127,491 (126,404–129,164) | 255 | 392 | 5.5 |
| H1 POST, 1 KiB body | nginx | 126,375 (125,847–139,378) | 238 | 714 | 5.3 |
| H1 POST, 1 KiB body | HAProxy | 116,524 (109,215–118,762) | 267 | 701 | 6.3 |
| TLS keep-alive | OxiRoute 7.0 (one clean run, indicative) | 108,694 | 295 | 492 | 9.2 |
| TLS keep-alive | nginx | 101,283 (100,827–101,739) | 308 | 504 | 9.9 |
| TLS keep-alive | HAProxy (6.5 integrated) | 88,547 (70,982–88,908) | 335 | 1,490 | 8.6 |
| H2 over TLS | OxiRoute 6.5 integrated | 86,098 (75,895–88,535) | 294 | 2,296 | 7.8 |
| H2 over TLS | nginx | 87,138 (81,224–91,768) | 294 | 1,995 | 7.1 |
| H2 over TLS | HAProxy | 78,729 (66,757–82,785) | 344 | 1,786 | 8.6 |
| TLS full handshake | OxiRoute 7.0 | 16,108 (16,035–16,180) | 1,949 | 2,778 | 93.1 |
| TLS full handshake | nginx | 5,397 (5,336–5,458) | 5,873 | 6,613 | 185.3 |
| TLS full handshake | HAProxy | 4,751 (4,599–4,903) | 6,631 | 8,332 | 210.4 |

In the 7.0 pass the plain keep-alive and new-connection rows favor OxiRoute in throughput and CPU cost, the nginx
keep-alive row being a single clean run. The 6.5 POST and H2 rows were not re-measured; established TLS medians are
close. A full TLS handshake costs substantially more CPU than an established connection; the 7.0 default rustls
backend completes about three times as many per second as nginx here, where the 6.5 OpenSSL row was lower. All HAProxy
TLS keep-alive attempts of the 7.0 pass were discarded, so that row keeps its 6.5 value. These observations do not
support a claim that OxiRoute is generally faster or cheaper.

A later recheck in the maintainer summary did not reproduce an apparent handshake regression against
6.4.0; this page therefore makes no regression claim. A later H2 rerun recorded zero accepted clean
runs and is excluded. The optional OpenSSL backend's 7.0 results are in the
[7.0 report](../benchmarks/reports/7.0/README.md).

## Idle connection memory

The memory evaluation used fresh processes, one worker per proxy, three runs, and page-cache
normalization before starting the OxiRoute builds. Values are **incremental worker RSS bytes per
connection**: resident memory with idle connections minus empty-worker RSS, divided by connection
count. They exclude kernel socket memory and other processes and are not total memory budgets.
The first four workloads used 5,000 connections; H2 used 1,000 connections after eight closed streams each.
TLS rows use OpenSSL. Values are medians with min–max.

| Idle workload | OxiRoute 6.5 integrated, bytes | nginx, bytes | HAProxy, bytes |
| --- | ---: | ---: | ---: |
| WebSocket | 13,115 (13,114–13,116) | 18,426 (18,409–18,439) | 6,065 (5,659–6,141) |
| WebSocket over TLS | 35,087 (35,073–35,116) | 60,071 (60,069–60,073) | 31,533 (28,801–32,421) |
| H1 keep-alive | 8,699 (7,832–9,238) | 9,578 (8,720–9,708) | 11,605 (9,384–11,692) |
| H1 keep-alive over TLS | 29,931 (28,982–29,932) | 34,637 (34,016–34,795) | 35,590 (35,394–35,804) |
| H2 over TLS | 41,468 (41,402–42,611) | 50,291 (49,443–50,643) | 45,982 (45,859–47,624) |

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
