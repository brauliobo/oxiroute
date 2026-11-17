# OxiRoute 7.0.0 performance report

Summary of the measurements behind the 7.0.0 claims. Each table is copied from the harness output of one
invocation; nothing is pooled across invocations unless a table says so. The raw run records, traces and logs are
local development material and are not published; this page keeps the medians, ranges and run counts. The `Source`
labels name those local output sets; they are not paths in this repository.

The headline lanes were re-measured on 2026-11-17 against the final tree (the next section). Lanes that were not
re-measured keep their earlier figures under "Earlier measurements", labelled with their date and build.

## Final-tree re-measurement (2026-11-17)

Conditions:

- Tree: `1d39e69d` (clean, version string `7.0.0`), release profile, fat LTO, no profile-guided optimization, binary
  SHA-256 prefix `f5974fd3`. The shipped 7.0.0 binaries are profile-guided and were not measured.
- Machine class: 16-core (32 logical CPU) desktop CPU, Linux 7.3, shared with unrelated jobs. Proxy on 2 CPUs (8-9),
  origin on 2 (10-11), load generator on 4 (12-15), one proxy worker (`listener_workers = 1`, nginx
  `worker_processes = 1`, HAProxy `nbthread 1`), loopback, closed loop, 1 KiB responses and 32 connections unless
  stated. nginx 1.30 and HAProxy 3.4 with minimal proxy configurations. Load generator and origin: `r2tool`
  ([benchmarks/r2-data-path/tool](../../r2-data-path/tool)).
- Runs: 4 s after a 2 s warm-up, at most 2 attempts per row. An attempt is discarded when the scheduler records more
  than 1.5 CPUs of foreign work on the pinned CPUs or the process identity changed. `clean` is runs kept of 2
  attempts; a row with fewer than 2 clean runs is **indicative** and its range is one sample.
- PSI is `/proc/pressure/cpu` `some` avg10 in percent, recorded before and after each row; it is diagnostic only and
  was never used to wait, retry or discard.
- `epoll` is the default data plane, `tokio` the same binary with `http_data_plane = "tokio"`, `io_uring` the same
  binary with `http_data_plane = "io_uring"`. `instr/req` is user-space instructions per request on the proxy process
  (`perf stat`, `instructions:u`); kernel work is not counted. Latencies are closed loop and carry coordinated
  omission.
- Harness: `benchmarks/scripts/vs_nginx_haproxy.py`, a local script that is not part of this checkout. The
  instruction-count lanes of [benchmarks/r2-data-path](../../r2-data-path/README.md) are checked in; the nginx and
  HAProxy columns cannot be reproduced from this repository alone.

### HTTP/1.1 keep-alive, 1 KiB response

| Connections | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | PSI before / after |
| ---: | --- | ---: | ---: | ---: | ---: | ---: |
| 32 | OxiRoute epoll (default) | 169,706 (167,747-171,664) | 302 | 14,712 | 2/2 | 0.0 / 0.0 |
| 32 | OxiRoute tokio plane | 128,043 (127,413-128,673) | 303 | 23,862 | 2/2 | 0.0 / 0.0 |
| 32 | OxiRoute io_uring | 170,511 (169,812-171,210) | 260 | 18,771 | 2/2 | 0.0 / 0.0 |
| 32 | nginx | 134,205 (single run) | 403 | 19,877 | 1/2, indicative | 0.0 / 0.0 |
| 32 | HAProxy | 121,704 (121,154-122,253) | 368 | 35,245 | 2/2 | 0.0 / 0.0 |
| 256 | OxiRoute epoll (default) | 169,498 (168,632-170,364) | 2,278 | 14,714 | 2/2 | 0.0 / 0.0 |
| 256 | OxiRoute tokio plane | 124,830 (124,611-125,048) | 2,808 | 23,801 | 2/2 | 0.0 / 0.1 |
| 256 | OxiRoute io_uring | 165,702 (163,815-167,590) | 2,001 | 19,150 | 2/2 | 0.1 / 0.0 |
| 256 | nginx | 135,754 (134,371-137,138) | 2,311 | 20,286 | 2/2 | 0.0 / 0.0 |
| 256 | HAProxy | 112,552 (112,123-112,982) | 2,904 | 40,009 | 2/2 | 0.0 / 0.0 |
| 1,024 | OxiRoute epoll (default) | 170,641 (169,272-172,010) | 8,318 | 14,710 | 2/2 | 0.0 / 0.0 |
| 1,024 | OxiRoute tokio plane | 116,555 (115,937-117,173) | 165,548 | 23,822 | 2/2 | 0.0 / 0.0 |
| 1,024 | OxiRoute io_uring | 150,450 (148,213-152,687) | 12,423 | 19,270 | 2/2 | 0.0 / 0.1 |
| 1,024 | nginx | 99,152 (96,039-102,266) | 16,843 | 20,455 | 2/2 | 0.0 / 0.0 |
| 1,024 | HAProxy | 80,984 (80,503-81,464) | 22,112 | 39,641 | 2/2 | 0.0 / 0.0 |

### HTTP/1.1 keep-alive, 64 KiB response, and new connection per request

32 connections. The epoll plane splices large response bodies from the origin socket to the client socket. The new
connection lane sends `Connection: close` on the proxied exchange.

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 64 KiB | OxiRoute epoll (default) | 87,002 (85,383-88,620) | 740 | 15,242 | 2/2 | 0.1 / 0.3 |
| 64 KiB | OxiRoute tokio plane | 48,338 (46,387-50,290) | 1,227 | 26,520 | 2/2 | 0.1 / 0.1 |
| 64 KiB | OxiRoute io_uring | 58,404 (57,522-59,286) | 986 | 21,107 | 2/2 | 0.1 / 0.0 |
| 64 KiB | nginx | 35,542 (35,530-35,553) | 1,410 | 37,730 | 2/2 | 0.0 / 0.0 |
| 64 KiB | HAProxy | 41,062 (40,309-41,815) | 1,112 | 48,970 | 2/2 | 0.0 / 0.0 |
| New connection | OxiRoute epoll (default) | 88,643 (88,118-89,168) | 639 | 16,256 | 2/2 | 0.1 / 0.3 |
| New connection | OxiRoute tokio plane | 71,366 (68,959-73,772) | 720 | 28,666 | 2/2 | 0.2 / 0.4 |
| New connection | OxiRoute io_uring | 87,554 (85,071-90,037) | 664 | 21,442 | 2/2 | 0.2 / 0.4 |
| New connection | nginx | 74,708 (74,429-74,987) | 702 | 21,460 | 2/2 | 0.2 / 0.4 |
| New connection | HAProxy | 67,016 (66,047-67,986) | 683 | 38,370 | 2/2 | 0.4 / 0.2 |

### Cache hits

1 KiB and 64 KiB bodies served from the in-memory cache, 32 connections. The cache hit runs on the fast lane, so the
tokio and `io_uring` columns differ from epoll only by noise.

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 1 KiB | OxiRoute epoll (default) | 231,274 (227,362-235,185) | 200 | 22,157 | 2/2 | 0.1 / 0.0 |
| 1 KiB | OxiRoute tokio plane | 234,817 (single run) | 161 | 22,160 | 1/2, indicative | 0.5 / 0.2 |
| 1 KiB | OxiRoute io_uring | 228,206 (226,578-229,835) | 228 | 22,166 | 2/2 | 0.1 / 0.1 |
| 1 KiB | nginx `proxy_cache` | 128,691 (single run) | 301 | 19,792 | 1/2, indicative | 0.0 / 0.3 |
| 64 KiB | OxiRoute epoll (default) | 97,425 (95,020-99,830) | 754 | 21,991 | 2/2 | 0.8 / 0.9 |
| 64 KiB | OxiRoute tokio plane | 98,171 (97,810-98,532) | 798 | 22,036 | 2/2 | 1.3 / 2.5 |
| 64 KiB | OxiRoute io_uring | 106,003 (102,221-109,785) | 668 | 22,097 | 2/2 | 2.4 / 2.5 |
| 64 KiB | nginx `proxy_cache` | 99,966 (99,311-100,620) | 660 | 18,228 | 2/2 | 2.4 / 1.8 |

At 64 KiB the cache hit is level with nginx (epoll 2.5% below, `io_uring` 6% above, inside the run-to-run range).

### TLS 1.3

32 connections, one worker, ECDSA P-256 certificate, rustls (aws-lc provider) default backend.

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Full handshake per request | OxiRoute rustls | 16,108 (16,035-16,180) | 2,778 | 665,649 | 2/2 | 0.6 / 0.2 |
| Full handshake per request | nginx | 5,397 (5,336-5,458) | 6,613 | 1,559,140 | 2/2 | 2.2 / 1.1 |
| Full handshake per request | HAProxy | 4,751 (4,599-4,903) | 8,332 | 1,608,282 | 2/2 | 1.1 / 3.3 |
| Keep-alive, 1 KiB | OxiRoute rustls | 108,694 (single run) | 492 | 31,965 | 1/2, indicative | 0.1 / 0.2 |
| Keep-alive, 1 KiB | nginx | 101,283 (100,827-101,739) | 504 | 36,629 | 2/2 | 1.6 / 1.1 |

The keep-alive OxiRoute row has one clean run: the other attempt was discarded for a process identity change. The
HAProxy keep-alive attempts and the explicit `http_data_plane = "epoll"` TLS attempts (one at 132,312 requests per
second with 22,495 instructions per request, foreign CPU 2.1) were both discarded, so neither is reported.

### HTTP/3

8 QUIC connections of 4 streams, 1 KiB, one worker, loopback. OxiRoute uses more than one core for QUIC.

| Proxy | req/s median (min-max) | p99 µs | instr/req | clean | PSI before / after |
| --- | ---: | ---: | ---: | ---: | ---: |
| OxiRoute, `bbr` (default) | 71,034 (single run) | 822 | 64,119 | 1/2, indicative | 2.4 / 2.0 |
| nginx | 67,133 (single run) | 566 | 55,921 | 1/2, indicative | 0.3 / 0.2 |

The `cubic` comparison produced no accepted run in two attempts and is not re-measured here. Both rows are one sample
on a busier moment of the host (PSI up to 2.4%), so the ratio (1.06) is indicative only.

### Not re-measured in this pass

Memory (PSS idle and with 1,024 connections held), the idle TLS connection footprint, the scaling table before start
pacing, the TLS 1.2, resumption and OpenSSL-backend lanes, the TLS 1 MiB static file lane, the compressed cache lanes,
the BBR loss-path and fairness matrix, and 2,048 connections. Their earlier figures follow.

## Earlier measurements (not re-measured on the final tree)

Recorded 2026-11-16 and 2026-11-17 with the integration build of the release: `c204c924` plus the integration
changes (dirty working tree, version string `6.5.2`, binary SHA-256 prefix `5204831b`), release profile, fat LTO, no
profile guidance, same host class and pinning, up to 3 runs of 4 s per cell. CPU pressure was mostly below 3%, with
spikes to 17% in some cells of the scaling and cache A/B harnesses.

### High-concurrency scaling before and after start pacing

Before the fix every ready connection started its request as soon as its event was dispatched, so the requests in
flight grew with the connection count and their working set outgrew the CPU cache above about 512 connections. A pass
over the ready events now starts at most 8 new requests and the rest wait in arrival order. One worker, medians of up
to 3 attempts (clean attempts in parentheses); `before` is the epoll plane without the fix.

| Connections | before, req/s | after, req/s | nginx, req/s | after p99 µs | nginx p99 µs |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 32 | 157,764 (2/3) | 171,163 (3/3) | 136,455 (2/3) | 225 | 288 |
| 256 | 157,893 (2/3) | 175,167 (2/3) | 125,616 (2/3) | 1,666 | 3,454 |
| 1,024 | 109,769 (2/3) | 164,201 (2/3) | 85,379 (3/3) | 7,676 | 18,932 |
| 2,048 | 55,289 (3/3) | 131,046 (3/3) | 59,882 (3/3) | 22,860 | 60,775 |

Source: `epoll-scale/table-final`. 6.5.2 measured 25.7k instructions per request on the 32-connection lane
(`timers/bench-h1_1k`, 3 paired runs, 125k requests per second).

### Cache A/B and compressed variants

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 KiB, A/B | `main` before the cache work (`c204c924`) | 113,185 | n/a | 44,351 | 3 runs |
| 1 KiB, A/B | 7.0.0 line | 223,853 | n/a | 22,159 | 3 runs |
| 64 KiB, gzip level 1 body | OxiRoute epoll (default) | 132,718 (131,240-136,780) | 449 | 30,860 | 3/3 |
| 64 KiB, gzip level 1 body | nginx, compresses per request | 46,994 (46,834-47,994) | 744 | 145,771 | 3/3 |
| 64 KiB, zstd level 1 body | OxiRoute epoll (default) | 204,244 (202,995-205,208) | 182 | 29,416 | 3/3 |

The first two rows are one A/B pair from an earlier harness (`cachehit/results`), the rest are from the integration
harness (`round2/bench/cache_gzip_hit_64k`, `cache_zstd_hit_64k`, under `integration2/`). The compressed lanes serve a
body stored once per coding (277 bytes of the 65,536 for gzip, 71 for zstd; nginx sent 836). Each compressed lane is
one invocation.

### TLS lanes other than the re-measured ones

32 connections, one worker, TLS 1.3 with an ECDSA P-256 certificate unless stated. `openssl` is `backend = "openssl"`
in the same binary. One invocation (`tls/bench`) unless stated.

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean |
| --- | --- | ---: | ---: | ---: | ---: |
| Full handshake per request | OxiRoute openssl | 5,821 (5,688-5,954) | 13,938 | 1,650,828 | 2/3 |
| TLS 1.2 ECDHE handshake | OxiRoute rustls | 16,750 (16,578-17,291) | 3,989 | 609,309 | 3/3 |
| TLS 1.2 ECDHE handshake | OxiRoute openssl | 6,812 (6,756-6,825) | 12,365 | 1,472,554 | 3/3 |
| Resumed handshake (ticket) | OxiRoute rustls | 17,309 (17,095-17,378) | 2,610 | 554,700 | 3/3 |
| Resumed handshake (ticket) | OxiRoute openssl | 6,283 (6,217-6,440) | 6,189 | 1,543,421 | 3/3 |
| Keep-alive, 1 KiB | OxiRoute openssl | 91,429 (91,235-91,623) | 467 | 39,288 | 2/3 |
| HTTP/2 over TLS, 1 KiB | OxiRoute rustls | 107,835 (106,079-109,591) | 512 | 38,645 | 2/3 |
| HTTP/2 over TLS, 1 KiB | OxiRoute openssl | 92,863 (89,623-96,103) | 607 | 42,062 | 2/3 |
| HTTP/2 over TLS, 1 KiB | nginx | 94,012 (single run) | 555 | 33,412 | 1/3, indicative |

Idle TLS connections held 15.0 kB (rustls), 32.4 kB (openssl) and 34.8 kB (nginx) of resident memory each (medians of
3, `tls/bench-idle-run.log`). On the epoll plane with a rustls profile (`http_data_plane = "epoll"`) the 1 KiB
keep-alive lane gave 135,217 requests per second with 23,380 instructions per request against 103,608 and 32,525 on
the tokio plane and 97,418 and 36,640 for nginx (`tlsepoll/summary`, one pass on a noisier host: **indicative**).

#### Static file over TLS

1 MiB file, TLS 1.3 AES-128-GCM keep-alive, 32 connections, one worker.

| Proxy | req/s median (min-max) | instr/req (user) | clean |
| --- | ---: | ---: | ---: |
| OxiRoute rustls, kernel TLS and `sendfile` (`ktls = "auto"`) | 4,169 (4,132-4,189) | 10,267 | 3/3 |
| nginx, user-space TLS | 2,195 (2,138-2,205) | 2,964,564 | 3/3 |

Source: `integration2/round2/bench/tls_static_1m`. A paired standalone run (3 runs of 10 s, 3 s warm-up) gave 3,621,
3,647 and 3,814 requests per second with kernel TLS, 2,454, 2,633 and 2,531 with `ktls = "off"` (user space) and 3,517
and 3,506 for nginx with kernel TLS (`ktls/profile-summary`). Instruction counts exclude kernel work, which is why the
kernel TLS row looks cheap: compare the request rates.

### HTTP/3 congestion control

Earlier HTTP/3 pass (`round4/h3_1k`, 3/3 clean each): OxiRoute `bbr` 78,255 requests per second (75,665-78,826),
`cubic` 79,174 (75,989-79,244), nginx 68,726 (66,044-68,893), 63.9k, 63.6k and 55.7k instructions per request. The
loss-path, fairness and CPU measurements of the BBR default are checked in:
[summary](../../r2-data-path/fairness/results/summary.md), [records](../../r2-data-path/fairness/results/) and the
"BBR measurements" section of [benchmarks/r2-data-path/README.md](../../r2-data-path/README.md). The tracked summary
records a 10 MiB download in 0.37 s with `bbr` against 15.0 s with `cubic` at a 40 ms round trip and 0.5% loss,
`bbr` taking 99.3% to 99.7% of a shared bottleneck (Jain's index 0.50), and a loss-free LAN 48 MiB download at
226 MiB/s with `bbr` against 1,100 MiB/s with `cubic`.

### Memory

Proportional set size (PSS) of the proxy process, one worker, idle keep-alive connections held open. `before` is the
`c204c924` build, `after` the 7.0.0 line. Binary on disk; with the binary on tmpfs the file-backed pages add 10 to
12 MB to both columns.

| State | before, MB | after, MB |
| --- | ---: | ---: |
| Idle | 63.8 | 13.0 |
| 32 connections held | 69.2 | 14.8 |
| 1,024 connections held | 82.1 | 24.2 |
| 1,024 connections drained | 75.3 | 22.0 |

The idle reduction is mostly `thp:never` in the embedded jemalloc configuration (transparent huge pages made 51 MB of
the idle footprint); the per-connection part is buffers that start small and grow with the request. Source:
`memory/measure/final-results`, one run per state. In the integration harness the epoll plane holds 34.0 MB at 1,024
connections against 35.0 MB for the tokio plane, with the binary on tmpfs (`integration2/round2/pss`).

### Idle wake-ups

Per-thread wake-ups per second of an idle direct-runtime process: 99 to 102 before, 0.1 after. The supervised
master and workers went from 1.05 to 0.10 per second after a traffic burst. The proxied request spends 1.66% of its
instructions in time subtraction after the change, 2.12% before (callgrind). Source: `timers/idle-*`,
`timers/callgrind-*`.

## What is not covered

- No run of the profile-guided release binary; no run of an arm64 binary.
- One worker per proxy. Two-worker results exist only in the io_uring
  [short report](../io-uring-http1/README.md) and are not part of the claims.
- nginx and HAProxy run minimal proxy configurations on the same loopback; their tuning was not explored.
- Two attempts per row in the final-tree pass: several rows have one clean run and are marked indicative.
- The io_uring plane matches epoll at 1 KiB keep-alive with 32 connections, is 2% below at 256 and 12% below at
  1,024 connections, costs 4.1k to 5.9k more user instructions per request and is slower at 64 KiB (58.4k against
  87.0k); it stays opt-in.
- Compressed-variant cache hit rows come from one invocation each.
- Closed-loop load, loopback, shared host: results show relative behavior of builds on that host, not capacity.
