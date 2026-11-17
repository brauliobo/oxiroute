# OxiRoute 7.0.0 performance report

Summary of the measurements behind the 7.0.0 claims. Each table is copied from the harness output of one
invocation; nothing is pooled across invocations unless a table says so. The raw run records, traces and logs are
local development material and are not published; this page keeps the medians, ranges and run counts. The `Source`
labels name those local output sets; they are not paths in this repository.

## Conditions common to every table

- Machine class: 16-core (32 logical CPU) desktop CPU, Linux 7.3, shared with unrelated jobs. CPU pressure
  (`/proc/pressure/cpu`, `some` avg10) was recorded before and after each cell and was mostly below 3%, with spikes
  to 17% in some cells of the scaling and cache A/B harnesses; it was never used to wait or retry.
- Pinning: proxy on 2 CPUs (8-9), origin on 2 (10-11), load generator on 4 (12-15). One proxy worker
  (`listener_workers = 1`, nginx `worker_processes 1`), loopback, closed loop, 32 connections unless stated.
- Load generator and origin: `r2tool`, [benchmarks/r2-data-path/tool](../../r2-data-path/tool). nginx 1.30 and
  HAProxy 3.4 with minimal proxy configurations.
- Harness: `benchmarks/scripts/vs_nginx_haproxy.py`, a local script that is not part of this checkout. The
  instruction-count lanes of [benchmarks/r2-data-path](../../r2-data-path/README.md) are checked in and reproduce
  the instructions per request of the OxiRoute columns; the nginx and HAProxy columns cannot be reproduced from this
  repository alone.
- Runs: up to 3 runs of 4 s after a 2 s warm-up per cell. A run is discarded when the scheduler records more than
  1.5 CPUs of foreign work on the pinned CPUs or the process identity changed. The `clean` column is runs kept of 3
  attempts; a cell with fewer than 2 clean runs is **indicative**, its range is a single sample.
- Build: release profile, fat LTO, no profile-guided optimization, built from `c204c924` plus the integration changes
  of the release (the working tree was dirty; version string `6.5.2`, release binary SHA-256 prefix `5204831b`).
  The 7.0.0 release binaries are profile-guided and were not measured; the numbers below are for the plain build.
  `epoll` is the default data plane, `tokio` is the same binary with `http_data_plane = "tokio"` (the 6.5 path),
  `io_uring` the opt-in plane.
- `instr/req` is user-space instructions per request attached to the proxy process (`perf stat`, `instructions:u`).
  Kernel work is not counted. Latencies are closed-loop and carry coordinated omission.
- Recorded 2026-11-16 and 2026-11-17.

## HTTP/1.1 keep-alive, 1 KiB response

32 connections. Two invocations of the same binaries; the second ran on a busier host (load average 4 to 12,
pressure up to 1.7%).

| Invocation | Proxy | req/s median (min-max) | p99 µs | instr/req | clean |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 | OxiRoute epoll (default) | 173,925 (169,253-174,339) | 216 | 14,651 | 3/3 |
| 1 | OxiRoute tokio plane | 127,338 (126,202-128,474) | 294 | 23,864 | 2/3 |
| 1 | OxiRoute io_uring | 173,785 (172,922-177,744) | 228 | 17,375 | 3/3 |
| 1 | nginx | 138,608 (single run) | 410 | 19,918 | 1/3, indicative |
| 2 | OxiRoute epoll (default) | 163,952 (160,211-167,694) | 241 | 14,712 | 2/3 |
| 2 | OxiRoute tokio plane | 116,855 (115,346-118,755) | 437 | 23,868 | 3/3 |
| 2 | OxiRoute io_uring | 164,168 (single run) | 252 | 18,770 | 1/3, indicative |
| 2 | nginx | 132,472 (single run) | 300 | 19,832 | 1/3, indicative |

6.5.2 measured 25.7k instructions per request on this lane (`timers/bench-h1_1k`, 3 paired runs, 125k requests per
second). Source: invocation 1 `integration2/round3/final/h1_1k`, invocation 2 `integration2/round4/h1_1k`.

## Concurrency: 256 and 1,024 keep-alive connections, 1 KiB

Single worker, same conditions. 256 connections are from the same two invocations; 1,024 from invocation 1.

| Connections | Proxy | req/s median (min-max) | p99 µs | instr/req | clean |
| --- | --- | ---: | ---: | ---: | ---: |
| 256 | OxiRoute epoll (default) | 171,902 (171,743-173,296) | 1,565 | 14,650 | 3/3 |
| 256 | OxiRoute tokio plane | 125,220 (125,005-125,435) | 2,206 | 23,788 | 2/3 |
| 256 | OxiRoute io_uring | 170,170 (166,166-170,365) | 1,600 | 17,531 | 3/3 |
| 256 | nginx | 137,712 (136,539-137,740) | 1,987 | 20,297 | 3/3 |
| 1,024 | OxiRoute epoll (default) | 169,040 (168,964-169,116) | 6,403 | 14,652 | 2/3 |
| 1,024 | OxiRoute tokio plane | 118,891 (118,222-119,061) | 168,924 | 23,809 | 3/3 |
| 1,024 | OxiRoute io_uring | 168,730 (167,362-169,160) | 6,455 | 17,621 | 3/3 |
| 1,024 | nginx | 108,036 (106,930-109,306) | 10,339 | 20,474 | 3/3 |

Second invocation at 256 connections: epoll 170,146, tokio plane 122,322, io_uring 162,903, nginx 136,530
(2/3 to 3/3 clean). Source: `round3/final/h1_1k_c256`, `h1_1k_c1024`, `round4/h1_1k_c256`.

### High-concurrency scaling before and after start pacing

Before the fix every ready connection started its request as soon as its event was dispatched, so the requests in
flight grew with the connection count and their working set outgrew the CPU cache above about 512 connections. A pass
over the ready events now starts at most 8 new requests and the rest wait in arrival order. Same host and pinning, one
worker, medians of up to 3 attempts (clean attempts in parentheses); `before` is the epoll plane without the fix.

| Connections | before, req/s | after, req/s | nginx, req/s | after p99 µs | nginx p99 µs |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 32 | 157,764 (2/3) | 171,163 (3/3) | 136,455 (2/3) | 225 | 288 |
| 256 | 157,893 (2/3) | 175,167 (2/3) | 125,616 (2/3) | 1,666 | 3,454 |
| 1,024 | 109,769 (2/3) | 164,201 (2/3) | 85,379 (3/3) | 7,676 | 18,932 |
| 2,048 | 55,289 (3/3) | 131,046 (3/3) | 59,882 (3/3) | 22,860 | 60,775 |

Source: `epoll-scale/table-final` (the `io_uring` column of that table predates the io_uring fixes and is superseded by
the 1,024-connection rows above).

## HTTP/1.1 keep-alive, 64 KiB response

32 connections. The epoll plane splices large response bodies from the origin socket to the client socket.

| Proxy | req/s median (min-max) | p99 µs | instr/req | clean |
| --- | ---: | ---: | ---: | ---: |
| OxiRoute epoll (default) | 88,879 (85,291-89,280) | 696 | 15,228 | 3/3 |
| OxiRoute tokio plane | 49,791 (49,703-49,835) | 913 | 26,513 | 3/3 |
| OxiRoute io_uring | 60,815 (60,784-60,827) | 682 | 21,106 | 3/3 |
| nginx | 36,195 (34,581-36,966) | 1,005 | 37,740 | 3/3 |

Source: `round4/h1_64k`.

## HTTP/1.1, new connection per request, 1 KiB

32 connections, `Connection: close` on the proxied exchange.

| Proxy | req/s median (min-max) | p99 µs | instr/req | clean |
| --- | ---: | ---: | ---: | ---: |
| OxiRoute epoll (default), invocation A | 89,608 (86,778-91,032) | 657 | 16,513 | 3/3 |
| OxiRoute epoll (default), invocation B | 92,163 (89,781-94,342) | 576 | 16,309 | 3/3 |
| OxiRoute tokio plane, A | 71,537 (65,835-72,445) | 695 | 28,498 | 3/3 |
| OxiRoute tokio plane, B | 68,088 (65,403-70,774) | 819 | 28,670 | 2/3 |
| nginx, A | 72,726 (72,319-73,133) | 717 | 21,464 | 2/3 |
| nginx, B | 72,792 (71,916-73,667) | 680 | 21,484 | 2/3 |
| HAProxy, A | 65,738 (65,723-66,512) | 684 | 38,388 | 3/3 |

Source: `integration2/bench/h1_1k_newconn` (A) and `integration2/round2/bench/h1_1k_newconn` (B). The io_uring plane's own
[short report](../io-uring-http1/README.md) records 86.0k requests per second on this lane.

## Cache hits

1 KiB and 64 KiB bodies served from the in-memory cache, 32 connections, one worker.

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 KiB, A/B | `main` before the cache work (`c204c924`) | 113,185 | n/a | 44,351 | 3 runs |
| 1 KiB, A/B | 7.0.0 line | 223,853 | n/a | 22,159 | 3 runs |
| 1 KiB | OxiRoute epoll (default) | 238,757 (236,001-239,612) | 171 | 22,160 | 3/3 |
| 1 KiB | nginx `proxy_cache` | 127,842 (126,656-129,028) | 303 | 19,720 | 2/3 |
| 64 KiB | OxiRoute epoll (default) | 101,260 (98,084-101,664) | 836 | 22,099 | 3/3 |
| 64 KiB | nginx `proxy_cache` | 100,708 (99,995-101,420) | 574 | 18,228 | 2/3 |
| 64 KiB, gzip level 1 body | OxiRoute epoll (default) | 132,718 (131,240-136,780) | 449 | 30,860 | 3/3 |
| 64 KiB, gzip level 1 body | nginx, compresses per request | 46,994 (46,834-47,994) | 744 | 145,771 | 3/3 |
| 64 KiB, zstd level 1 body | OxiRoute epoll (default) | 204,244 (202,995-205,208) | 182 | 29,416 | 3/3 |

The first two rows are one A/B pair from an earlier harness (`cachehit/results`), the rest are from the integration
harness (`round2/bench/cache_hit_1k`, `round3/final/cache_hit_64k`, `round2/bench/cache_gzip_hit_64k`,
`round2/bench/cache_zstd_hit_64k`, all under `integration2/`). At 64 KiB the cache hit is level with nginx; the
first measurement of that lane (`round2`) had nginx ahead (100,190 against 82,872) and the A/B of `round3/ab` shows
no change from the cache work at that size. The compressed lanes serve a body stored once per coding (277 bytes of the
65,536 for gzip, 71 for zstd; nginx sent 836). Each compressed lane is one invocation.

## TLS

32 connections, one worker, TLS 1.3 with an ECDSA P-256 certificate unless stated. `rustls` is the default backend
(aws-lc provider); `openssl` is `backend = "openssl"` in the same binary. One invocation (`tls/bench`) unless stated.

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean |
| --- | --- | ---: | ---: | ---: | ---: |
| Full handshake per request | OxiRoute rustls | 14,865 (14,614-15,734) | 2,631 | 664,806 | 3/3 |
| Full handshake per request | OxiRoute openssl | 5,821 (5,688-5,954) | 13,938 | 1,650,828 | 2/3 |
| Full handshake per request | nginx | 5,098 (4,732-5,147) | 7,274 | 1,559,000 | 3/3 |
| TLS 1.2 ECDHE handshake | OxiRoute rustls | 16,750 (16,578-17,291) | 3,989 | 609,309 | 3/3 |
| TLS 1.2 ECDHE handshake | OxiRoute openssl | 6,812 (6,756-6,825) | 12,365 | 1,472,554 | 3/3 |
| Resumed handshake (ticket) | OxiRoute rustls | 17,309 (17,095-17,378) | 2,610 | 554,700 | 3/3 |
| Resumed handshake (ticket) | OxiRoute openssl | 6,283 (6,217-6,440) | 6,189 | 1,543,421 | 3/3 |
| Keep-alive, 1 KiB | OxiRoute rustls | 105,040 (103,991-106,089) | 384 | 33,420 | 2/3 |
| Keep-alive, 1 KiB | OxiRoute openssl | 91,429 (91,235-91,623) | 467 | 39,288 | 2/3 |
| Keep-alive, 1 KiB | nginx | 96,362 (93,396-97,209) | 496 | 36,708 | 3/3 |
| HTTP/2 over TLS, 1 KiB | OxiRoute rustls | 107,835 (106,079-109,591) | 512 | 38,645 | 2/3 |
| HTTP/2 over TLS, 1 KiB | OxiRoute openssl | 92,863 (89,623-96,103) | 607 | 42,062 | 2/3 |
| HTTP/2 over TLS, 1 KiB | nginx | 94,012 (single run) | 555 | 33,412 | 1/3, indicative |

Repeat of the full handshake lane with the final integration binary (`integration2/bench/tls_hs_1k`, 3 attempts):
OxiRoute rustls 15,938 (15,860-16,017) requests per second with 663,042 instructions (2/3 clean), nginx 5,143 with
1,559,404 (3/3), HAProxy 4,672 with 1,608,992 (2/3). Idle TLS connections held 15.0 kB (rustls), 32.4 kB (openssl) and
34.8 kB (nginx) of resident memory each (medians of 3, `tls/bench-idle-run.log`).

On the epoll plane with a rustls profile (`http_data_plane = "epoll"`) the 1 KiB keep-alive lane gave 135,217 requests
per second with 23,380 instructions per request against 103,608 and 32,525 on the tokio plane and 97,418 and 36,640
for nginx (`tlsepoll/summary`, one separate pass on a noisier host: **indicative**).

### Static file over TLS

1 MiB file, TLS 1.3 AES-128-GCM keep-alive, 32 connections, one worker.

| Proxy | req/s median (min-max) | instr/req (user) | clean |
| --- | ---: | ---: | ---: |
| OxiRoute rustls, kernel TLS and `sendfile` (`ktls = "auto"`) | 4,169 (4,132-4,189) | 10,267 | 3/3 |
| nginx, user-space TLS | 2,195 (2,138-2,205) | 2,964,564 | 3/3 |

Source: `integration2/round2/bench/tls_static_1m`. A paired standalone run (3 runs of 10 s, 3 s warm-up) gave 3,621,
3,647 and 3,814 requests per second with kernel TLS, 2,454, 2,633 and 2,531 with `ktls = "off"` (user space) and 3,517
and 3,506 for nginx with kernel TLS (`ktls/profile-summary`). Instruction counts exclude kernel work, which is why the
kernel TLS row looks cheap: compare the request rates.

## HTTP/3

8 QUIC connections of 4 streams, 1 KiB, one worker, loopback. OxiRoute uses more than one core for QUIC
(1.26 CPUs on the pinned pair); nginx is limited to the one worker.

| Proxy | req/s median (min-max) | p99 µs | instr/req | clean |
| --- | ---: | ---: | ---: | ---: |
| OxiRoute, `bbr` (default) | 78,255 (75,665-78,826) | 565 | 63,910 | 3/3 |
| OxiRoute, `cubic` | 79,174 (75,989-79,244) | 554 | 63,609 | 3/3 |
| nginx | 68,726 (66,044-68,893) | 535 | 55,683 | 3/3 |

Source: `round4/h3_1k`. The loss-path, fairness and CPU measurements of the BBR default are checked in:
[summary](../../r2-data-path/fairness/results/summary.md), [records](../../r2-data-path/fairness/results/) and the
"BBR measurements" section of [benchmarks/r2-data-path/README.md](../../r2-data-path/README.md). The tracked summary
records a 10 MiB download in 0.37 s with `bbr` against 15.0 s with `cubic` at a 40 ms round trip and 0.5% loss,
`bbr` taking 99.3% to 99.7% of a shared bottleneck (Jain's index 0.50), and a loss-free LAN 48 MiB download at
226 MiB/s with `bbr` against 1,100 MiB/s with `cubic`.

## Memory

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

## Idle wake-ups

Per-thread wake-ups per second of an idle direct-runtime process: 99 to 102 before, 0.1 after. The supervised
master and workers went from 1.05 to 0.10 per second after a traffic burst. The proxied request spends 1.66% of its
instructions in time subtraction after the change, 2.12% before (callgrind). Source: `timers/idle-*`,
`timers/callgrind-*`.

## What is not covered

- No run of the profile-guided release binary; no run of an arm64 binary.
- One worker per proxy. Two-worker results exist only in the io_uring
  [short report](../io-uring-http1/README.md) and are not part of the claims.
- nginx and HAProxy run minimal proxy configurations on the same loopback; their tuning was not explored.
- The io_uring plane matches epoll at 1 KiB keep-alive with 32 connections and is within 4% at 256, but costs 2.7k
  to 4.4k more user instructions per request and is slower at 64 KiB (60.8k against 88.9k); it stays opt-in.
- Compressed-variant cache hit rows come from one invocation each.
- Closed-loop load, loopback, shared host: results show relative behavior of builds on that host, not capacity.
