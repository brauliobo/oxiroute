# Benchmarks and evidence

Use these measurements to understand specific tradeoffs and to design your own evaluation.
They do not establish production capacity or a general ranking of proxies.

## Evidence status

The rows labelled **OxiRoute 7.1** come from a maintainer's measurement on 2026-11-19 of the published 7.1.0 x86_64
release binary (profile-guided; tarball SHA-256 `b002f507...1d651`), run with the nginx 1.30.7 and HAProxy 3.4.8 rows in the
same pass. The complete 7.1 tables, including the tokio and `io_uring` planes, 64 KiB, 1 MiB, 1,024 and 2,048
connections, cache, compression, HTTP/2, HTTP/3, two and four workers, memory and a 7.1 against 7.0 comparison with its
regressions, are in the [7.1 report](../benchmarks/reports/7.1/README.md). The [7.0 report](../benchmarks/reports/7.0/README.md)
holds the earlier pass on the 7.0.0 binary.

OxiRoute 7.1.1 changes no hot path beyond per-client tracking, which costs about 1.9% of the connect-per-request
throughput (HTTP/1 keep-alive and HTTP/2 are unchanged within noise), so these 7.1.0 figures were not repeated for it.
OxiRoute 7.2.0 changes no data-plane hot path, so the 7.1 figures stay.
OxiRoute 7.3.0 changes no data-plane hot path, so the 7.1 figures stay.

The underlying research notes, raw run records and harness are local development material, not part of this public
checkout. Treat the tables as **reported observations**, not an accepted release baseline.

## Workload and method

The 7.1 measurement records:

- A shared 16-core (32-logical-CPU) host with unrelated work running; the complete hardware and kernel provenance is
  not published with this page.
- One proxy worker, with the proxy assigned two CPUs and the origin and load generator assigned separate CPUs; loopback
  traffic; 32 connections; interleaved turns for each proxy.
- At least two clean 4-second runs after a 2-second warm-up for every cell except the single-run cells (HTTP/2 over TLS
  POST 1 KiB, TCP relay echo of 1 KiB and 64 KiB, two workers with 256 connections, and every row of the later quick pass:
  eight feature lanes, the four HTTP/3 lanes with HAProxy, four instruction-count harness lanes, reload memory and the
  lossy-path download), which have
  exactly one. **Clean**
  means `/proc/pressure/cpu` `some` avg10 below 10% before and after the run, no request errors, and a valid scheduler
  trace. Foreign CPU work on the pinned CPUs was recorded but, unlike in the 7.0 pass (more than 1.5 CPUs discarded), not
  used to separate runs, so the 7.1 rows are not strictly comparable with the 7.0 rows.
- The published profile-guided binary (build record: commit `f4d62d78`, `pgo: true`, `bolt: false`) with OxiRoute's
  default rustls backend for TLS, against nginx 1.30.7 and HAProxy 3.4.8 with minimal proxy configurations. The harness
  is a local script that is not part of this checkout.
- Medians and observed minimum-maximum ranges, rather than confidence intervals. Latencies come from a closed-loop load
  generator; they do not model a production arrival rate or remove coordinated omission. A p99 here is meaningful only
  at the stated concurrency and workload.

`CPU µs/request` is the scheduler-accounted CPU time of the proxy process divided by completed requests. It excludes
loopback softirq work. It is neither elapsed latency nor a universal hardware-independent cost. Keep these metrics
separate when comparing runs.

## Throughput, latency and CPU

Each row is the median of the clean runs of one invocation of that workload; no best-run selection or pooling across
different builds is applied. Bodies are 1 KiB. H1 means HTTP/1.1; TLS keep-alive reuses an established TLS 1.3
connection. The handshake row opens a new TLS 1.3 ECDSA connection per request. The H2 rows use 8 connections of 4 streams.

| Workload | Proxy | Requests/s, median (min–max) | p50, µs | p99, µs | CPU µs/request |
| --- | --- | ---: | ---: | ---: | ---: |
| H1 proxy, keep-alive | OxiRoute 7.1 | 182,364 (177,516–187,212) | 168 | 265 | 5.4 |
| H1 proxy, keep-alive | nginx | 134,242 (132,958–135,527) | 228 | 402 | 7.3 |
| H1 proxy, keep-alive | HAProxy | 118,436 (117,985–118,888) | 258 | 388 | 8.4 |
| H1 proxy, new connection | OxiRoute 7.1 | 91,826 (89,183–94,470) | 322 | 676 | 10.6 |
| H1 proxy, new connection | nginx | 65,484 (65,138–65,830) | 448 | 868 | 14.6 |
| H1 proxy, new connection | HAProxy | 61,530 (59,151–63,910) | 484 | 875 | 15.9 |
| H1 POST, 1 KiB body | OxiRoute 7.1 | 172,832 (171,449–174,215) | 170 | 336 | 5.6 |
| H1 POST, 1 KiB body | nginx | 121,477 (121,273–121,681) | 248 | 506 | 8.1 |
| H1 POST, 1 KiB body | HAProxy | 115,117 (115,065–115,169) | 261 | 468 | 8.6 |
| TLS keep-alive | OxiRoute 7.1 | 127,919 (127,597–128,241) | 247 | 449 | 7.8 |
| TLS keep-alive | nginx | 94,787 (92,640–96,934) | 324 | 524 | 10.3 |
| TLS keep-alive | HAProxy | 84,884 (83,087–86,681) | 356 | 712 | 11.6 |
| H2 over TLS | OxiRoute 7.1 | 130,661 (128,520–132,802) | 240 | 486 | 7.6 |
| H2 over TLS | nginx | 98,260 (97,871–98,650) | 308 | 608 | 10.0 |
| H2 over TLS | HAProxy | 80,351 (78,528–82,174) | 367 | 906 | 12.1 |
| TLS full handshake | OxiRoute 7.1 | 14,960 (14,876–15,044) | 1,994 | 6,352 | 92.9 |
| TLS full handshake | nginx | 4,939 (4,862–5,016) | 6,167 | 13,716 | 197.6 |
| TLS full handshake | HAProxy | 4,394 (4,277–4,511) | 6,867 | 9,062 | 218.6 |

OxiRoute 7.1 is ahead of nginx and HAProxy in throughput and CPU cost on these six lanes: by 36% over nginx on plain
keep-alive, 35% on established TLS and 33% on HTTP/2; 40% on new connections and 42% on 1 KiB POST. A full TLS handshake
costs substantially more CPU than an established connection; the default rustls backend completes about three times as
many per second as nginx here. These observations do not support a claim that OxiRoute is generally faster or cheaper.

Against the 7.0.0 binary measured earlier, 7.1 is within 7% on these lanes (plain keep-alive 182,364 against 178,396,
TLS keep-alive 127,919 against 128,113, full handshake 14,960 against 16,092); with two workers and default scheduling it
is far higher (324,356 against 177,640 at 64 connections). The [7.1 report](../benchmarks/reports/7.1/README.md) lists the
lanes that fell, including two workers at 2,048 connections (-18%), 64 KiB POST, uncached compression and HTTP/3 `cubic`
POST, and the lanes where OxiRoute trails nginx or HAProxy: four workers, HTTP/2 upstreams, WebSocket echo and HTTP/3
through a proxy. nginx and HAProxy moved between the two passes by similar amounts, so differences of a few percent
between 7.1 and 7.0 are not a change of the proxy.

## Idle connection memory

The memory evaluation (7.1 binary, measured 2026-11-19) used fresh processes, one worker per proxy, two runs, and
page-cache normalization before starting the OxiRoute processes. Values are **incremental worker RSS bytes per
connection**: resident memory with idle connections minus empty-worker RSS, divided by connection
count. They exclude kernel socket memory and other processes and are not total memory budgets.
The first four workloads used 5,000 connections; H2 used 1,000 connections after eight closed streams each.
OxiRoute uses the default rustls backend. Values are medians with min-max over the two runs.

| Idle workload | OxiRoute 7.1, bytes | nginx, bytes | HAProxy, bytes |
| --- | ---: | ---: | ---: |
| WebSocket | 12,520 (12,519–12,520) | 18,372 (18,371–18,372) | 6,484 (6,337–6,632) |
| WebSocket over TLS | 21,642 (21,641–21,642) | 59,830 (59,813–59,848) | 30,178 (30,118–30,237) |
| H1 keep-alive | 1,777 (1,774–1,780) | 9,903 (9,813–9,993) | 10,944 (9,713–12,176) |
| H1 keep-alive over TLS | 18,326 (17,323–19,328) | 34,805 (34,802–34,808) | 36,070 (35,813–36,326) |
| H2 over TLS | 27,852 (27,619–28,086) | 48,758 (48,267–49,250) | 47,796 (47,591–48,001) |

The same pass reports lower incremental RSS than nginx for these workloads; HAProxy is lower for idle WebSocket
connections. Process PSS in 7.1 is 11.5 MB idle and 13.8 MB with 1,024 connections held for OxiRoute, against 11.0 and
20.4 MB for nginx and 31.2 and 41.8 MB for HAProxy; OxiRoute's idle figure is above the 9.9 MB of the 7.0 pass. Memory
after 50 configuration reloads with 1,001 routes (one run, new script, one worker) is 33.8 MB for OxiRoute, 21.3 MB for
nginx and 25.0 MB for HAProxy; the 7.0 pass used another script. Busy connections, request bodies, caches, queues, retained
generations, and total process memory require separate measurements.

## What these results do not cover

No conclusion follows here about WAN traffic, open-loop tail latency, UDP, RTMP, arm64, forward proxying
against nginx or HAProxy, cache hit rates, many-core scaling beyond four workers, failure recovery,
long-lived mixed traffic, or production sizing. HTTP/3, HTTP/2 upstreams, the tokio and `io_uring` data planes,
two and four workers, 512 to 2,048 connections, compression and idle memory are measured in the
[7.1 report](../benchmarks/reports/7.1/README.md). The eight feature lanes (2 KiB request headers, access
log, request id, header rules, 1,000 routes, 4 origin servers, literal redirect, basic authentication with bcrypt), the
HTTP/3 lanes with HAProxy, the memory after configuration reloads and the lossy-path HTTP/3 download are single-run rows
of a quick pass in the same report. The seven-feature reverse proxy and the forward-proxy lanes run only on OxiRoute (no
comparator can run them) and are listed apart from the comparison tables. The [7.0 report](../benchmarks/reports/7.0/README.md)
has the earlier figures for the older lanes. Feature support belongs in the [compatibility matrix](COMPATIBILITY.md), not
in a performance table. Results from different hosts, builds or load models must remain separate.

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
