# OxiRoute 7.0.0 performance report

Summary of the measurements behind the 7.0.0 claims. Each table is copied from the harness output of one
invocation; nothing is pooled across invocations unless a table says so. The raw run records, traces and logs are
local development material and are not published; this page keeps the medians, ranges and run counts. The `Source`
labels name those local output sets; they are not paths in this repository.

The shipped release binary was measured on 2026-11-17 across every lane the harness supports (the next section). The
headline lanes had been measured earlier the same day on a build of the final tree without profile guidance (the
section after it); lanes that were measured in neither pass keep their earlier figures under "Earlier
measurements", labelled with their date and build.

## Shipped binary, all lanes (2026-11-17)

Conditions:

- Binary: the published `oxiroute-7.0.0-x86_64-unknown-linux-gnu` release tarball, SHA-256
  `5081e6aadad4cee8b29017e5cd14a5c3c8f9790eae9c6c409e78801d351a506c` (extracted `bin/oxiroute`
  `de71b89131b5ff746612d1b6003b6ce64934ffddf4830459a03296ae4c99f9da`). Its build record: tree `c01445fe`, rustc 1.97.1,
  `pgo: true`, `bolt: false`, `panic: unwind`, C flags `-O3`, baseline x86-64 ISA, glibc 2.34 floor. Nothing in the
  proxy was rebuilt; the load generator and origin `r2tool` were built from the source tree at `9a14030e`
  (SHA-256 `282c4e84...`).
- Machine class: 16-core (32 logical CPU) desktop CPU, Linux 7.3, shared with unrelated jobs. Proxy on 2 CPUs (8-9),
  origin on 2 (10-11), load generator on 4 (12-15), one proxy worker unless a row says otherwise, loopback, closed loop,
  1 KiB responses and 32 connections unless stated. nginx 1.30.6 and HAProxy 3.4.7 with minimal proxy configurations,
  linked with OpenSSL 4.1.0.
- Runs: 4 s after a 2 s warm-up, 2 attempts per row (3 or 4 for rows marked in the table where the first pass left
  fewer than 2 clean runs). `clean` is runs kept of attempts. An attempt is discarded when the scheduler trace is
  inconsistent, the owned threads changed, or more than 1.5 CPUs of foreign work ran on the pinned CPUs; discards of
  the scheduler-trace kind were frequent in this pass (for every proxy), so many rows have one clean run. A row with fewer than 2 clean runs is **indicative** and its range is one sample.
- PSI is `/proc/pressure/cpu` `some` avg10 in percent, the median over the clean runs of the value before and after
  the run; diagnostic only, never used to wait, retry or discard. It was 0.2 to 7.9 across rows.
- `epoll` is the default data plane. `tokio` and `io_uring` are the same binary with `http_data_plane` set in the
  configuration. `instr/req` is user-space instructions per request on the proxy process (`perf stat`,
  `instructions:u`); kernel work is not counted. Latencies are closed loop and carry coordinated omission.
- Harness: `benchmarks/scripts/vs_nginx_haproxy.py`, a local script that is not in this checkout, used with two local
  adaptations for the 7.0 configuration schema (selecting `http_data_plane` per column, and the cache `compression`
  policy of the compressed-hit lane, which the script's service-level gzip setting cannot express on 7.0). The
  `benchmarks/r2-data-path` lanes below are checked in and reproduce from the repository.

### HTTP/1.1 keep-alive, 1 KiB response, by connection count

| Connections | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | PSI before / after |
| ---: | --- | ---: | ---: | ---: | ---: | ---: |
| 32 | OxiRoute epoll (default) | 178,396 (177,389-179,402) | 254 | 11,668 | 2/2 | 0.3 / 0.2 |
| 32 | OxiRoute tokio plane | 145,374 (144,755-145,994) | 260 | 18,574 | 2/4 | 0.2 / 0.3 |
| 32 | OxiRoute io_uring | 168,510 (165,716-171,303) | 238 | 15,732 | 2/2 | 0.3 / 0.4 |
| 32 | nginx | 134,061 (131,708-136,414) | 350 | 19,909 | 2/2 | 0.3 / 0.5 |
| 32 | HAProxy | 114,860 (114,644-115,076) | 390 | 35,242 | 2/2 | 0.4 / 0.5 |
| 256 | OxiRoute epoll (default) | 171,930 (single run) | 4,415 | 11,681 | 1/2, indicative | 5.3 / 3.9 |
| 256 | OxiRoute tokio plane | 143,662 (143,113-144,210) | 1,989 | 18,516 | 2/2 | 5.6 / 3.6 |
| 256 | OxiRoute io_uring | 162,403 (single run) | 2,218 | 16,157 | 1/2, indicative | 2.7 / 4.0 |
| 256 | nginx | no clean run in 2 attempts | - | - | 0/2 | - |
| 256 | HAProxy | 106,794 (105,405-108,184) | 3,654 | 40,028 | 2/2 | 5.2 / 5.3 |
| 1,024 | OxiRoute epoll (default) | 179,035 (single run) | 6,228 | 11,662 | 1/2, indicative | 4.8 / 4.5 |
| 1,024 | OxiRoute tokio plane | 135,597 (single run) | 158,184 | 18,529 | 1/2, indicative | 5.3 / 7.2 |
| 1,024 | OxiRoute io_uring | 149,092 (single run) | 19,291 | 16,343 | 1/2, indicative | 4.4 / 4.5 |
| 1,024 | nginx | 92,548 (90,013-95,082) | 18,658 | 20,404 | 2/2 | 2.8 / 3.7 |
| 1,024 | HAProxy | 79,344 (78,672-80,015) | 15,580 | 39,570 | 2/2 | 4.3 / 5.4 |

The 32-connection rows are the second invocation of that lane (4 attempts), after a first with one clean run for epoll
and nginx. CPU time per request (scheduler-accounted, 32 connections): epoll 5.5 µs, tokio 6.8, io_uring 5.9, nginx
7.4, HAProxy 8.7. The 256-connection and 1,024-connection rows were measured while PSI was 3% to 7%.

### Bodies, new connection per request, and POST

32 connections. The epoll plane splices large response bodies from the origin socket to the client socket. The new
connection lane sends `Connection: close` on the proxied exchange. POST sends the body to an origin sink.

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 64 KiB | OxiRoute epoll (default) | 91,602 (89,400-93,804) | 620 | 12,122 | 2/2 | 3.5 / 3.3 |
| 64 KiB | OxiRoute tokio plane | 53,337 (single run) | 884 | 20,433 | 1/2, indicative | 3.2 / 3.6 |
| 64 KiB | OxiRoute io_uring | 59,642 (59,075-60,210) | 746 | 17,995 | 2/2 | 4.7 / 3.3 |
| 64 KiB | nginx | 34,961 (34,680-35,242) | 1,292 | 37,833 | 2/2 | 4.0 / 4.3 |
| 64 KiB | HAProxy | 38,988 (38,722-39,255) | 1,006 | 48,956 | 2/2 | 4.0 / 3.6 |
| 1 MiB | OxiRoute epoll (default) | 5,300 (5,276-5,325) | 12,220 | 26,672 | 2/2 | 6.6 / 3.0 |
| 1 MiB | OxiRoute tokio plane | 4,201 (4,183-4,219) | 19,482 | 29,116 | 2/2 | 2.9 / 5.6 |
| 1 MiB | OxiRoute io_uring | 3,230 (3,213-3,247) | 13,668 | 51,367 | 2/2 | 5.2 / 2.8 |
| 1 MiB | nginx | 2,056 (2,029-2,082) | 22,570 | 262,647 | 2/2 | 4.6 / 3.3 |
| 1 MiB | HAProxy | 2,983 (single run) | 15,039 | 165,771 | 1/2, indicative | 4.4 / 3.5 |
| New connection | OxiRoute epoll (default) | 91,432 (88,800-94,065) | 752 | 12,972 | 2/2 | 7.9 / 5.1 |
| New connection | OxiRoute tokio plane | 80,070 (79,336-80,805) | 586 | 22,999 | 2/2 | 2.9 / 2.8 |
| New connection | OxiRoute io_uring | 92,178 (91,374-92,981) | 516 | 18,070 | 2/2 | 2.3 / 2.8 |
| New connection | nginx | no clean run in 2 attempts | - | - | 0/2 | - |
| New connection | HAProxy | 62,840 (single run) | 741 | 38,378 | 1/2, indicative | 2.4 / 3.4 |
| POST 1 KiB | OxiRoute epoll (default) | 180,102 (single run) | 314 | 12,101 | 1/2, indicative | 7.4 / 5.0 |
| POST 1 KiB | OxiRoute tokio plane | no clean run in 2 attempts | - | - | 0/2 | - |
| POST 1 KiB | OxiRoute io_uring | 168,522 (single run) | 245 | 16,393 | 1/2, indicative | 5.5 / 1.7 |
| POST 1 KiB | nginx | no clean run in 2 attempts | - | - | 0/2 | - |
| POST 1 KiB | HAProxy | 114,382 (113,089-115,676) | 398 | 37,158 | 2/2 | 2.5 / 3.8 |
| POST 64 KiB | OxiRoute epoll (default) | 47,063 (45,658-48,468) | 787 | 13,193 | 2/2 | 2.1 / 2.3 |
| POST 64 KiB | OxiRoute tokio plane | 40,960 (single run) | 1,435 | 34,545 | 1/2, indicative | 0.4 / 0.2 |
| POST 64 KiB | OxiRoute io_uring | 39,531 (single run) | 1,790 | 37,132 | 1/2, indicative | 2.5 / 1.3 |
| POST 64 KiB | nginx | no clean run in 2 attempts | - | - | 0/2 | - |
| POST 64 KiB | HAProxy | 32,711 (single run) | 1,178 | 52,008 | 1/2, indicative | 0.2 / 0.4 |

The nginx new-connection attempts were discarded for a scheduler-trace inconsistency and for 14 CPUs of foreign work
during one attempt, so the nginx figures of the earlier non-PGO pass (74,708 requests per second) are the only
comparison. In the 1 MiB epoll rows the proxy process used 0.64 of one CPU, so the limit of that row was not the
proxy's user and system time; the limiting resource was not isolated.

### Cache hits and compression

1 KiB and 64 KiB bodies served from the in-memory cache, 32 connections; the compressed rows serve a stored 64 KiB text
body as gzip level 1 (the cache `compression` policy), or compress it on every request with no cache. The cache hit
runs on the fast lane, so the three planes differ only by noise.

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 1 KiB hit | OxiRoute epoll (default) | 270,224 (267,606-272,842) | 146 | 18,466 | 2/2 | 0.7 / 2.1 |
| 1 KiB hit | OxiRoute tokio plane | 267,722 (266,912-268,532) | 145 | 18,473 | 2/2 | 2.7 / 1.2 |
| 1 KiB hit | OxiRoute io_uring | 267,454 (267,098-267,810) | 142 | 18,471 | 2/2 | 1.1 / 0.9 |
| 1 KiB hit | nginx `proxy_cache` | 129,656 (single run) | 274 | 19,638 | 1/2, indicative | 0.5 / 0.3 |
| 1 KiB hit | HAProxy cache | 186,358 (181,131-191,585) | 248 | 30,786 | 2/2 | 2.4 / 3.5 |
| 64 KiB hit | OxiRoute epoll (default) | 106,662 (106,441-106,883) | 624 | 18,342 | 2/2 | 2.6 / 5.5 |
| 64 KiB hit | OxiRoute tokio plane | 113,590 (113,564-113,616) | 499 | 18,333 | 2/2 | 5.9 / 6.4 |
| 64 KiB hit | OxiRoute io_uring | 108,739 (single run) | 565 | 18,317 | 1/2, indicative | 5.8 / 3.7 |
| 64 KiB hit | nginx `proxy_cache` | 98,567 (single run) | 684 | 18,206 | 1/2, indicative | 6.3 / 4.2 |
| 64 KiB gzip level 1, stored coding, hit | OxiRoute epoll (default) | 129,376 (127,660-131,093) | 494 | 26,899 | 2/2 | 4.1 / 3.7 |
| 64 KiB gzip level 1, stored coding, hit | nginx, compresses per request | 44,746 (44,425-45,066) | 910 | 145,827 | 2/2 | 5.9 / 3.0 |
| 64 KiB gzip level 1, no cache | OxiRoute epoll (default) | 33,202 (32,973-33,430) | 1,510 | 164,178 | 2/2 | 3.6 / 4.4 |
| 64 KiB gzip level 1, no cache | nginx | 35,131 (34,559-35,703) | 1,088 | 166,760 | 2/2 | 2.8 / 4.0 |
| 64 KiB gzip level 1, no cache | HAProxy | 24,783 (24,310-25,256) | 1,894 | 212,622 | 2/2 | 3.7 / 3.2 |

The 64 KiB hit is 8% above nginx on one clean nginx run: treat it as level. Brotli and zstd stored-coding lanes were
not run: the load generator can verify only gzip bodies. The nginx 1 KiB hit and 64 KiB hit rows have one clean run
(the other attempt of the 1 KiB lane failed the request checks).

### TLS and HTTP/2

32 connections, one worker, ECDSA P-256 certificate, rustls (aws-lc provider) default backend. HTTP/2 over TLS: 8
connections of 4 streams.

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| TLS 1.3 keep-alive, 1 KiB | OxiRoute default (tokio path) | 128,113 (126,656-129,570) | 416 | 25,574 | 2/3 | 0.4 / 0.5 |
| TLS 1.3 keep-alive, 1 KiB | OxiRoute, `http_data_plane = "tokio"` | 132,840 (131,232-134,448) | 384 | 25,574 | 2/2 | 0.4 / 0.3 |
| TLS 1.3 keep-alive, 1 KiB | OxiRoute, `http_data_plane = "epoll"` | 154,726 (154,130-155,322) | 246 | 19,206 | 2/2 | 1.3 / 0.6 |
| TLS 1.3 keep-alive, 1 KiB | nginx | 96,906 (95,519-98,293) | 442 | 36,605 | 2/2 | 0.4 / 1.7 |
| TLS 1.3 keep-alive, 1 KiB | HAProxy | 84,794 (81,487-88,101) | 636 | 50,045 | 2/2 | 1.0 / 0.5 |
| TLS 1.3 full handshake | OxiRoute | 16,092 (16,043-16,141) | 2,248 | 648,136 | 2/2 | 4.0 / 1.7 |
| TLS 1.3 full handshake | nginx | 5,131 (5,102-5,160) | 6,797 | 1,559,352 | 2/2 | 2.6 / 2.1 |
| TLS 1.3 full handshake | HAProxy | 4,674 (4,666-4,681) | 9,654 | 1,610,014 | 2/2 | 2.0 / 4.1 |
| TLS 1.2 ECDHE full handshake | OxiRoute | 16,770 (16,628-16,911) | 4,487 | 594,384 | 2/2 | 1.3 / 3.2 |
| TLS 1.2 ECDHE full handshake | nginx | 5,404 (single value, both runs) | 7,258 | 1,390,006 | 2/2 | 2.3 / 4.9 |
| TLS 1.2 ECDHE full handshake | HAProxy | 4,864 (4,842-4,886) | 7,394 | 1,446,594 | 2/2 | 4.9 / 1.8 |
| TLS 1.3 resumed (ticket) | OxiRoute | 17,386 (single run) | 3,095 | 536,202 | 1/2, indicative | 2.0 / 3.3 |
| TLS 1.3 resumed (ticket) | nginx | 5,226 (5,215-5,236) | 11,323 | 1,480,935 | 2/2 | 1.6 / 5.3 |
| TLS 1.3 resumed (ticket) | HAProxy | 4,592 (4,549-4,636) | 8,198 | 1,521,310 | 2/2 | 4.8 / 1.9 |
| TLS 1.2 resumed (ticket) | OxiRoute | 57,527 (57,260-57,794) | 1,022 | 109,704 | 2/2 | 2.3 / 5.4 |
| TLS 1.2 resumed (ticket) | nginx | 11,902 (11,695-12,109) | 3,646 | 271,880 | 2/2 | 2.3 / 5.2 |
| TLS 1.2 resumed (ticket) | HAProxy | 8,871 (8,821-8,921) | 4,089 | 303,218 | 2/2 | 4.9 / 2.0 |
| HTTP/2 over TLS, 1 KiB | OxiRoute | 132,781 (131,965-133,597) | 374 | 31,617 | 2/2 | 0.8 / 0.8 |
| HTTP/2 over TLS, 1 KiB | nginx | 105,442 (single run) | 350 | 33,387 | 1/4, indicative | 0.8 / 0.8 |
| HTTP/2 over TLS, 1 KiB | HAProxy | 85,054 (85,030-85,079) | 546 | 45,777 | 2/3 | 0.3 / 0.8 |
| HTTP/2 over TLS, 64 KiB | OxiRoute | 27,914 (27,842-27,985) | 2,103 | 206,136 | 2/2 | 3.8 / 5.7 |
| HTTP/2 over TLS, 64 KiB | nginx | 17,426 (single run) | 4,983 | 254,374 | 1/2, indicative | 4.8 / 5.2 |
| HTTP/2 over TLS, 64 KiB | HAProxy | 20,706 (20,612-20,800) | 1,878 | 234,662 | 2/2 | 7.7 / 5.0 |
| HTTP/2 over TLS, POST 1 KiB | OxiRoute | 113,696 (112,550-114,842) | 424 | 37,444 | 2/2 | 0.4 / 0.7 |
| HTTP/2 over TLS, POST 1 KiB | nginx | 72,002 (71,052-72,953) | 546 | 40,856 | 2/4 | 0.5 / 0.7 |
| HTTP/2 over TLS, POST 1 KiB | HAProxy | 71,801 (71,644-71,958) | 625 | 55,414 | 2/2 | 0.6 / 0.6 |

The default HTTPS listener runs on the tokio path (25.6k instructions per request); with `http_data_plane = "epoll"`
and a rustls profile the same lane is 21% faster and 25% cheaper in user instructions. The TLS 1.2 nginx handshake
rows are two equal values, not a range. In the handshake rows OxiRoute used 1.4 to 1.5 CPUs of the 2 (handshakes run
on helper threads, `tls_handshake_threads`); nginx and HAProxy used one.

#### Static files

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 1 MiB | OxiRoute epoll (default) | 3,452 (1,714-5,190) | 18,790 | 8,412 | 2/2 | 2.6 / 5.3 |
| 1 MiB | nginx | 1,661 (1,651-1,671) | 27,098 | 13,272 | 2/2 | 5.5 / 4.4 |
| 1 MiB over TLS 1.3, kernel TLS and `sendfile` | OxiRoute | 3,157 (single run) | 11,895 | 8,940 | 1/2, indicative | 4.7 / 8.2 |
| 1 MiB over TLS 1.3, user-space TLS | nginx | 681 (674-688) | 52,358 | 2,964,560 | 2/2 | 7.1 / 3.3 |
| 1 KiB (plain) | OxiRoute, nginx, nginx with `open_file_cache` | no clean run | - | - | 0/2 each | - |

The two OxiRoute 1 MiB plain runs differ by a factor of three (5,190 and 1,714) while the instruction counts agree: the
spread was not explained. The nginx TLS static rate is a third of the earlier
pass (2,195) with an identical instruction count; the cause was not isolated. The plain 1 KiB static lane produced no
clean run for any proxy (the OxiRoute attempts were discarded because the process's thread set changed during the
window, the nginx attempts for scheduler-trace inconsistency); `benchmarks/r2-data-path` measured it instead (below).

### Scaling over two workers and aged scheduling

`listener_workers = 2`, nginx `worker_processes = 2`, HAProxy `nbthread 2`, proxy CPUs 8-9. The `aged` column is the
`connection_scheduling = "aged_per_worker"` opt-in.

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | proxy CPUs used | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 KiB, 64 connections | OxiRoute epoll, default scheduling | 177,640 (175,792-179,489) | 528 | 11,663 | 2/3 | 0.99 | 0.5 / 0.4 |
| 1 KiB, 64 connections | nginx | no clean run in 3 attempts | - | - | 0/3 | - | - |
| 1 KiB, 64 connections | HAProxy | 218,280 (212,874-223,686) | 398 | 35,279 | 2/3 | 1.96 | 0.5 / 0.4 |
| 1 KiB, 256 connections | OxiRoute epoll, default scheduling | 183,176 (181,244-185,107) | 1,590 | 11,664 | 2/3 | 0.99 | 0.5 / 0.6 |
| 1 KiB, 256 connections | OxiRoute epoll, aged | 340,345 (single run) | 998 | 12,343 | 1/3, indicative | 1.97 | 0.5 / 0.7 |
| 1 KiB, 256 connections | nginx | no clean run in 3 attempts | - | - | 0/3 | - | - |
| 1 KiB, 256 connections | HAProxy | 206,878 (203,766-209,989) | 1,932 | 37,232 | 2/2 | 1.97 | 0.4 / 0.6 |

With two workers and the default `work_stealing` scheduling the OxiRoute process used 0.99 CPUs and its rate was the
one-worker rate (nginx's two-worker figure in the first, overwritten invocation of the 64-connection lane was 259,861;
that single run is not in the table). The aged scheduling used both CPUs (1.97) and reached 340,345 requests per
second, 1.9 times the default, in a single clean run. Whether the default scheduling should leave a worker idle in
this lane was not investigated here; the shipped default is the lower row. The 4-worker, 512 to 2,048 connection and
dynamic-scaling matrices were not run in this pass.

### Memory

Proportional set size (PSS) of the proxy process with one worker, idle then with N idle keep-alive connections held
open, then after they are closed (the process is the same across the states, 32 first, then 1,024; three processes,
medians shown, range at most 0.12 MB for every state). Binary on disk.

| State | PSS, MB (3 processes) |
| --- | ---: |
| Idle | 13.2 (13.2-13.2) |
| 32 connections held | 13.8 (13.8-13.8) |
| 32 connections drained | 13.9 (13.9-13.9) |
| 1,024 connections held | 15.6 (15.6-15.7) |
| 1,024 connections drained | 15.7 (15.7-15.8) |

Of the idle 13.2 MB, 8.4 MB is file-backed (the binary), 4.8 MB anonymous. The 1,024-held figure is lower than the
24.2 MB of the earlier memory pass (below); the method here is one process driven by the same load tool with a
single request per connection, so a harness difference cannot be excluded. Reload memory was not measured (no
checked-in lane).

Incremental resident memory per idle connection (resident memory with connections held minus empty process, divided by
the connection count) from the harness idle lanes: 5,000 connections for the first four rows, 1,000 for HTTP/2 after
eight closed streams each; 2 runs each, OxiRoute default (rustls) TLS backend.

| Idle workload | OxiRoute, bytes | nginx, bytes | HAProxy, bytes |
| --- | ---: | ---: | ---: |
| H1 keep-alive | 1,749 (1,747-1,751) | 9,989 (9,980-9,998) | 11,728 (11,669-11,787) |
| H1 keep-alive over TLS | 16,749 (16,472-17,026) | 34,784 (34,759-34,809) | 35,762 (35,662-35,863) |
| WebSocket | 12,526 (12,526-12,526) | 18,364 (18,358-18,371) | 5,693 (5,106-6,280) |
| WebSocket over TLS | 21,634 (21,633-21,635) | 59,832 (59,829-59,835) | 31,188 (30,071-32,304) |
| H2 over TLS | 29,864 (29,426-30,302) | 48,792 (48,472-49,111) | 47,074 (46,928-47,219) |

HAProxy is lower for idle plain WebSocket connections; OxiRoute is lower elsewhere in this table.

### HTTP/3

`benchmarks/r2-data-path` and the lossy-path script, one worker, loopback, shipped binary. The harness comparison lane
(8 QUIC connections of 4 streams, 1 KiB) produced no clean OxiRoute run in 3 attempts: the scheduler-trace validation
fails because the QUIC runtime uses more threads than the proxied lanes, so no ratio with nginx is stated here from
that harness (nginx: 66,358 requests per second, 56,301 instructions per request, one clean run; direct 1 KiB response:
126,442, 41,642, 2 clean).

| Lane (instruction-count harness, 3 repetitions) | req/s median | p50 µs | p99 µs | instr/req | spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| H3 proxy, 8 connections of 4 streams, 1 KiB, `bbr` default | 72,178 | 484 | 632 | 70,011 | 1.2% |
| H3 direct 1 KiB response | 198,369 | 160 | 229 | 35,537 | 6.2% |
| H3 proxy POST 1 KiB | 62,792 | 560 | 729 | 67,615 | 0.4% |
| H3, new QUIC connection per request, direct response | 5,256 | 1,525 | 2,273 | 1,870,346 | 21.4% |

Download of a 10 MiB file at 40 ms round trip and 0.5% packet loss, user and network namespace with `netem`, 3 runs
each (time per download):

| Server | median s | min s | p99 s | goodput MiB/s |
| --- | ---: | ---: | ---: | ---: |
| OxiRoute HTTP/3, `bbr` (default) | 0.629 | 0.621 | 0.668 | 15.9 |
| OxiRoute HTTP/3, `cubic` | 7.512 | 1.093 | 7.909 | 1.3 |
| nginx HTTP/3 | 27.337 | 25.354 | 29.692 | 0.4 |
| nginx HTTP/2 over TCP | 0.683 | 0.563 | 8.412 | 14.6 |

The `bbr` default is 12 times faster than `cubic` on this path and level with TCP's median; the tracked fairness
summary (below) records 0.37 s and 15.0 s on an earlier run of the same scenario. BBR's costs on a loss-free LAN and
its unfairness against other flows are in the tracked summary, not re-measured. The `cubic` and `bbr` HTTP/3 request
rate lane (1 KiB, harness) was not re-measured for the reason above.

### WebSocket, TCP relay, pools and other lanes (instruction-count harness)

`benchmarks/r2-data-path/run.sh`, OxiRoute only (the harness has no nginx or HAProxy configuration for these lanes),
16 connections unless stated, 3 repetitions, the median is shown; `spread` is `(max-min)/median` of the instruction
count.

| Lane | req/s | p99 µs | instr/req | spread |
| --- | ---: | ---: | ---: | ---: |
| WebSocket echo, 1 KiB message | 174,142 | 114 | 9,511 | 1.9% |
| TCP relay echo, 1 KiB | 203,193 | 99 | 5,043 | 0.1% |
| TCP relay echo, 64 KiB | 77,778 | 366 | 5,705 | 25.9% |
| Pool `least_connections`, 8 servers | 175,280 | 125 | 12,152 | 0.4% |
| Pool `weighted_round_robin`, 8 servers | 177,533 | 123 | 12,515 | 0.5% |
| HTTP/1.1 keep-alive, 1 connection | 45,689 | 31 | 12,702 | 0.8% |
| HTTP/1.1, 2 KiB request headers | 142,306 | 157 | 45,918 | 0.2% |
| Static file, 1 KiB | 357,327 | 61 | 6,503 | 1.3% |
| Static file, 1 MiB | 6,157 | 3,625 | 8,166 | 3.5% |
| Static file over TLS 1.3, 1 MiB, user-space TLS | 2,720 | 9,107 | 2,661,718 | 0.1% |
| Static file over TLS 1.3, 1 MiB, kernel TLS | 3,140 | 5,915 | 8,914 | 6.4% |
| Reverse proxy with 7 production features on (forwarded, access log, timeouts, passive health, breaker, outlier, capacity) | 155,098 | n/a | 18,929 | 0.1% |
| Upstream HTTP/2, 1 KiB | 41,725 | 508 | 99,418 | 0.1% |
| Upstream HTTP/2, POST 1 KiB | 31,488 | 791 | 124,572 | 0.1% |

Forward proxy and RTMP have no performance lane in the harness and were not measured. The `least_connections` pool is
level with `round_robin` and `weighted_round_robin` at 8 servers.

### Lanes not run in this pass

HTTP/1.1 at 2,048 connections; the 1 connection latency lane against nginx and HAProxy; access log, request id, header
rewrite, 1,000-route, 4-origin and redirect lanes; RSA-2048 and client certificate handshakes; TLS 1.2 keep-alive;
resumed handshakes by session id; the static 1 KiB lane against nginx (no clean run); basic authentication; brotli and
zstd cache hits; the four-worker and dynamic-scaling matrices; reload memory; forward proxy and RTMP (no lane); the
tokio and `io_uring` columns of the TLS, HTTP/2, compression and handshake lanes (`io_uring` does not serve TLS); the
BBR fairness matrix (tracked results are in the repository). Time-box: 120 minutes in total.

## Final-tree re-measurement without profile guidance (2026-11-17)

Conditions:

- Tree: `1d39e69d` (clean, version string `7.0.0`), release profile, fat LTO, no profile-guided optimization, binary
  SHA-256 prefix `f5974fd3`. The shipped, profile-guided binary was measured in the section above: it is faster than
  this build on the plain HTTP/1.1 lanes (178,396 against 169,706 requests per second at 32 connections) and costs
  fewer user instructions per request (11.7k against 14.7k).
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

- No run of an arm64 binary. The profile-guided x86_64 binary was measured only for the lanes of the first section.
- One worker per proxy, except the two-worker rows of the first section (a single clean aged-scheduling run among
  them); the io_uring [short report](../io-uring-http1/README.md) has more two-worker results. None is part of the
  claims.
- nginx and HAProxy run minimal proxy configurations on the same loopback; their tuning was not explored.
- Two attempts per row in the final-tree pass: several rows have one clean run and are marked indicative.
- The io_uring plane matches epoll at 1 KiB keep-alive with 32 connections, is 2% below at 256 and 12% below at
  1,024 connections, costs 4.1k to 5.9k more user instructions per request and is slower at 64 KiB (58.4k against
  87.0k); it stays opt-in. With the shipped binary it is 6% below epoll at 32 connections, 6% at 256 (one run), 17% at
  1,024 (one run), 35% below at 64 KiB (59.6k against 91.6k) and 39% below at 1 MiB, with 4.1k to 24.7k more user
  instructions per request.
- Compressed-variant cache hit rows come from one invocation each.
- Closed-loop load, loopback, shared host: results show relative behavior of builds on that host, not capacity.
