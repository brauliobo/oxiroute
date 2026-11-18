# OxiRoute 7.0.0 performance report

Summary of the measurements behind the 7.0.0 claims. Each row is copied from the harness output of one
invocation; a table can combine rows from different invocations, and nothing is pooled within a row. The raw run records, traces and logs are
local development material and are not published; this page keeps the medians, ranges and run counts. The `Source`
labels name those local output sets; they are not paths in this repository.

The shipped release binary was measured on 2026-11-17 across the lanes the harness supported then, and again on
2026-11-18 in a completion pass that re-measured every cell without two clean runs and added the lanes that had not
been run. Every cell of the first section below has at least two clean runs. The headline lanes had been measured
earlier on 2026-11-17 on a build of the final tree without profile guidance (the section after it); lanes that were
measured in neither pass keep their earlier figures under "Earlier measurements", labelled with their date and build.

## Shipped binary, all lanes (2026-11-17 and 2026-11-18)

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
- Runs: 4 s after a 2 s warm-up; every cell has at least 2 clean runs and was repeated (up to 16 rounds) until it did.
  `clean` is runs kept of attempts. An attempt is discarded when the scheduler trace is invalid, the CPU of the
  proxy's threads cannot be attributed, more than 1.5 CPUs of foreign work ran on the pinned CPUs, more than 8 CPUs ran
  on their sibling hardware threads, or any request failed (the HTTP/3 new-connection lanes tolerate 0.03% of
  connections ending with `H3_NO_ERROR`, which both servers produce).
- PSI is `/proc/pressure/cpu` `some` avg10 in percent, the median over the clean runs of the value before and after
  the run; diagnostic only, never used to wait, retry or discard. It was 0.0 to 7.9 across rows.
- `epoll` is the default data plane. `tokio` and `io_uring` are the same binary with `http_data_plane` set in the
  configuration. `instr/req` is user-space instructions per request on the proxy process (`perf stat`,
  `instructions:u`); kernel work is not counted. Latencies are closed loop and carry coordinated omission.
- Harness: `benchmarks/scripts/vs_nginx_haproxy.py`, a local script that is not in this checkout, used with two local
  adaptations for the 7.0 configuration schema (selecting `http_data_plane` per column, and the cache `compression`
  policy of the compressed-hit lane, which the script's service-level gzip setting cannot express on 7.0). The
  `benchmarks/r2-data-path` lanes below are checked in and reproduce from the repository.
- Harness corrections of the completion pass: identical duplicate `perf` records are dropped before the scheduler
  timeline is checked (they caused `running PID inconsistent` discards for nginx and for OxiRoute HTTP/3); CPU time of
  short-lived threads (blocking pool, QUIC runtime) is attributed by owned process instead of by thread identity
  (they caused `owned thread identities changed` discards in the static, cache and HTTP/3 lanes); the load generator
  verifies brotli and zstd bodies, not only gzip; the HAProxy cache stores 64 KiB bodies through a `Cache-Control`
  response rule; the HAProxy 1,000-route configuration uses one file-backed `path` ACL (a first configuration with a
  linear ACL per route measured 18,000 requests per second and was an artifact of that configuration); nginx
  HTTP/2 upstream (`proxy_http_version 2`), the nginx stream module, the nginx HTTP/3 listener and HAProxy `alpn h2`
  upstream are configured per lane; four-worker cells keep a housekeeping CPU for the scheduler recorder outside the
  watched CPUs.

### Where OxiRoute is not ahead

- Default epoll scheduling with several workers: at two workers and 64 or 256 connections the default OxiRoute row
  (177,640 and 183,176 requests per second) trails nginx (249,555 and 248,264), HAProxy (218,280, 206,878), the tokio
  plane (249,088, 267,236), `io_uring` (303,192, 291,636) and `aged_per_worker` (326,586, 283,586). At four workers
  and 1,024 connections or fewer the default (262,670 to 272,122) and the aged scheduling (295,522 to 344,030) are
  below nginx (300,136 to 352,074) and HAProxy (299,608 to 357,568) except at 2,048 connections.
- HTTP/2 upstream: with an HTTP/2 origin OxiRoute serves 40,125 requests per second (98,230 user instructions per
  request) against 87,964 for nginx (39,694) and 87,490 for HAProxy (48,622), on every data plane.
- WebSocket echo: nginx 225,079 against OxiRoute 174,002 (HAProxy 170,088); TCP relay echo of 1 KiB: nginx 208,781
  against 202,172. Compression without a cache at 64 KiB: nginx 35,131 against 33,202 (HAProxy 24,783). The static
  1 MiB file is level with nginx (1,699 against 1,668).
- The tokio plane at 1,024 and 2,048 connections has p99 latencies of 159 ms and 349 ms (epoll: 9.7 ms and 14.6 ms);
  at two workers it fails about one request in a million with `UnexpectedEof`, so its cells needed up to six attempts.
- `aged_per_worker` is lower than the default for TLS and HTTP/2 over TLS at 1,024 connections or more (TLS 1.3
  keep-alive 82,268 against 109,691 at 1,024 connections) with a lower p99 (37.7 ms against 167 ms).
- Memory after 50 configuration reloads with 1,001 routes: OxiRoute grew from 19.8 to 41.6 MB, nginx from 17.7 to 26.2
  MB and HAProxy stayed between 34.8 and 36.9 MB. The growth flattens after about ten reloads; it was not attributed.
- Basic authentication with bcrypt is not like for like: nginx and HAProxy verify the hash on every request (26
  requests per second, 231 million instructions per request); OxiRoute does not run bcrypt per request for the same
  credentials (46,806 instructions per request) and serves 90,646.

### HTTP/1.1 keep-alive, 1 KiB response, by connection count

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 32 | OxiRoute epoll (default) | 178,396 (177,389-179,402) | 254 | 11,668 | 2/2 | 0.3 / 0.2 |
| 32 | OxiRoute tokio plane | 145,374 (144,755-145,994) | 260 | 18,574 | 2/4 | 0.2 / 0.3 |
| 32 | OxiRoute io_uring | 168,510 (165,716-171,303) | 238 | 15,732 | 2/2 | 0.3 / 0.4 |
| 32 | nginx | 134,061 (131,708-136,414) | 350 | 19,909 | 2/2 | 0.3 / 0.5 |
| 32 | HAProxy | 114,860 (114,644-115,076) | 390 | 35,242 | 2/2 | 0.4 / 0.5 |
| 256 | OxiRoute epoll (default) | 180,487 (177,246-183,728) | 1,771 | 11,670 | 2/2 | 0.8 / 1.0 |
| 256 | OxiRoute tokio plane | 143,662 (143,113-144,210) | 1,989 | 18,516 | 2/2 | 5.6 / 3.6 |
| 256 | OxiRoute io_uring | 165,160 (164,645-165,676) | 2,432 | 16,133 | 2/2 | 1.0 / 1.1 |
| 256 | nginx | 122,657 (120,698-124,616) | 2,460 | 20,290 | 2/2 | 1.0 / 0.9 |
| 256 | HAProxy | 106,794 (105,405-108,184) | 3,654 | 40,028 | 2/2 | 5.2 / 5.3 |
| 1,024 | OxiRoute epoll (default) | 166,806 (156,085-177,528) | 9,730 | 11,678 | 2/3 | 0.7 / 1.0 |
| 1,024 | OxiRoute tokio plane | 130,310 (129,898-130,722) | 159,402 | 18,544 | 2/2 | 0.7 / 0.8 |
| 1,024 | OxiRoute io_uring | 147,642 (143,544-151,741) | 10,170 | 16,373 | 2/3 | 1.1 / 1.1 |
| 1,024 | nginx | 92,548 (90,013-95,082) | 18,658 | 20,404 | 2/2 | 2.8 / 3.7 |
| 1,024 | HAProxy | 79,344 (78,672-80,015) | 15,580 | 39,570 | 2/2 | 4.3 / 5.4 |
| 2,048 | OxiRoute epoll (default) | 159,112 (157,412-160,811) | 14,576 | 11,666 | 2/2 | 0.0 / 0.2 |
| 2,048 | OxiRoute tokio plane | 126,575 (123,777-129,373) | 349,352 | 18,564 | 2/2 | 0.2 / 0.2 |
| 2,048 | OxiRoute io_uring | 133,476 (133,417-133,536) | 42,583 | 16,414 | 2/2 | 0.2 / 0.3 |
| 2,048 | nginx | 71,364 (67,419-75,309) | 48,150 | 20,464 | 2/2 | 0.2 / 0.1 |
| 2,048 | HAProxy | 61,604 (59,250-63,959) | 50,182 | 39,923 | 2/2 | 0.2 / 0.2 |

The 32-connection rows are the second invocation of that lane (4 attempts), after a first with one clean run for epoll
and nginx. CPU time per request (scheduler-accounted, 32 connections): epoll 5.5 µs, tokio 6.8, io_uring 5.9, nginx
7.4, HAProxy 8.7. The tokio and HAProxy 256-connection rows and the nginx and HAProxy 1,024-connection rows were
measured at PSI 2.8% to 5.6%, the other rows at 0.0% to 1.1%.

### Bodies, new connection per request, and POST

32 connections. The epoll plane splices large response bodies from the origin socket to the client socket. The new
connection lane sends `Connection: close` on the proxied exchange. POST sends the body to an origin sink.

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 64 KiB | OxiRoute epoll (default) | 91,602 (89,400-93,804) | 620 | 12,122 | 2/2 | 3.5 / 3.3 |
| 64 KiB | OxiRoute tokio plane | 51,338 (51,103-51,572) | 1,022 | 20,453 | 2/2 | 0.7 / 0.7 |
| 64 KiB | OxiRoute io_uring | 59,642 (59,075-60,210) | 746 | 17,995 | 2/2 | 4.7 / 3.3 |
| 64 KiB | nginx | 34,961 (34,680-35,242) | 1,292 | 37,833 | 2/2 | 4.0 / 4.3 |
| 64 KiB | HAProxy | 38,988 (38,722-39,255) | 1,006 | 48,956 | 2/2 | 4.0 / 3.6 |
| 1 MiB | OxiRoute epoll (default) | 5,300 (5,276-5,325) | 12,220 | 26,672 | 2/2 | 6.6 / 3.0 |
| 1 MiB | OxiRoute tokio plane | 4,201 (4,183-4,219) | 19,482 | 29,116 | 2/2 | 2.9 / 5.6 |
| 1 MiB | OxiRoute io_uring | 3,230 (3,213-3,247) | 13,668 | 51,367 | 2/2 | 5.2 / 2.8 |
| 1 MiB | nginx | 2,056 (2,029-2,082) | 22,570 | 262,647 | 2/2 | 4.6 / 3.3 |
| 1 MiB | HAProxy | 2,692 (2,635-2,750) | 17,556 | 166,086 | 2/2 | 0.6 / 0.8 |
| New connection | OxiRoute epoll (default) | 91,432 (88,800-94,065) | 752 | 12,972 | 2/2 | 7.9 / 5.1 |
| New connection | OxiRoute tokio plane | 80,070 (79,336-80,805) | 586 | 22,999 | 2/2 | 2.9 / 2.8 |
| New connection | OxiRoute io_uring | 92,178 (91,374-92,981) | 516 | 18,070 | 2/2 | 2.3 / 2.8 |
| New connection | nginx | 69,558 (68,322-70,793) | 694 | 21,454 | 2/2 | 0.7 / 1.1 |
| New connection | HAProxy | 60,109 (58,243-61,975) | 748 | 38,382 | 2/2 | 1.3 / 0.7 |
| POST 1 KiB | OxiRoute epoll (default) | 171,858 (169,863-173,853) | 228 | 12,102 | 2/2 | 1.2 / 1.2 |
| POST 1 KiB | OxiRoute tokio plane | 133,159 (128,107-138,211) | 293 | 19,756 | 2/3 | 0.7 / 0.7 |
| POST 1 KiB | OxiRoute io_uring | 164,172 (161,194-167,151) | 262 | 16,394 | 2/3 | 1.1 / 0.3 |
| POST 1 KiB | nginx | 112,460 (111,835-113,084) | 370 | 23,102 | 2/3 | 0.7 / 1.4 |
| POST 1 KiB | HAProxy | 114,382 (113,089-115,676) | 398 | 37,158 | 2/2 | 2.5 / 3.8 |
| POST 64 KiB | OxiRoute epoll (default) | 47,063 (45,658-48,468) | 787 | 13,193 | 2/2 | 2.1 / 2.3 |
| POST 64 KiB | OxiRoute tokio plane | 37,802 (37,078-38,526) | 1,131 | 34,546 | 2/3 | 6.5 / 2.3 |
| POST 64 KiB | OxiRoute io_uring | 38,800 (37,993-39,606) | 1,278 | 37,486 | 2/3 | 1.2 / 6.9 |
| POST 64 KiB | nginx | 26,976 (26,459-27,494) | 2,124 | 30,642 | 2/3 | 7.2 / 6.4 |
| POST 64 KiB | HAProxy | 30,322 (29,147-31,496) | 1,287 | 52,022 | 2/2 | 4.0 / 4.2 |

In the 1 MiB epoll rows the proxy process used 0.64 of one CPU, so the limit of that row was not the proxy's user and
system time; the limiting resource was not isolated.

### Cache hits and compression

1 KiB and 64 KiB bodies served from the in-memory cache, 32 connections; the compressed rows serve a stored 64 KiB text
body as gzip, zstd or brotli level 1 (the cache `compression` policy), or compress it on every request with no cache. The
cache hit runs on the fast lane, so the three planes differ only by noise. nginx and HAProxy have no stored-coding
cache and no brotli or zstd encoder in the measured builds, so those lanes are OxiRoute only.

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 1 KiB hit | OxiRoute epoll (default) | 270,224 (267,606-272,842) | 146 | 18,466 | 2/2 | 0.7 / 2.1 |
| 1 KiB hit | OxiRoute tokio plane | 267,722 (266,912-268,532) | 145 | 18,473 | 2/2 | 2.7 / 1.2 |
| 1 KiB hit | OxiRoute io_uring | 267,454 (267,098-267,810) | 142 | 18,471 | 2/2 | 1.1 / 0.9 |
| 1 KiB hit | nginx `proxy_cache` | 131,942 (131,803-132,082) | 265 | 19,803 | 2/2 | 0.2 / 0.2 |
| 1 KiB hit | HAProxy cache | 186,358 (181,131-191,585) | 248 | 30,786 | 2/2 | 2.4 / 3.5 |
| 64 KiB hit | OxiRoute epoll (default) | 106,662 (106,441-106,883) | 624 | 18,342 | 2/2 | 2.6 / 5.5 |
| 64 KiB hit | OxiRoute tokio plane | 113,590 (113,564-113,616) | 499 | 18,333 | 2/2 | 5.9 / 6.4 |
| 64 KiB hit | OxiRoute io_uring | 110,002 (109,187-110,817) | 630 | 18,492 | 2/3 | 1.3 / 3.7 |
| 64 KiB hit | nginx `proxy_cache` | 98,718 (97,029-100,406) | 701 | 18,267 | 2/2 | 2.1 / 3.4 |
| 64 KiB hit | HAProxy cache | 57,800 (57,344-58,255) | 716 | 63,712 | 2/2 | 3.0 / 1.2 |
| 64 KiB gzip level 1, stored coding, hit | OxiRoute epoll (default) | 129,376 (127,660-131,093) | 494 | 26,899 | 2/2 | 4.1 / 3.7 |
| 64 KiB gzip level 1, stored coding, hit | OxiRoute tokio plane | 135,674 (135,236-136,111) | 406 | 26,920 | 2/4 | 0.2 / 0.2 |
| 64 KiB gzip level 1, stored coding, hit | OxiRoute io_uring | 137,866 (137,153-138,580) | 404 | 26,858 | 2/2 | 0.2 / 0.1 |
| 64 KiB gzip level 1, stored coding, hit | nginx, compresses per request | 44,746 (44,425-45,066) | 910 | 145,827 | 2/2 | 5.9 / 3.0 |
| 64 KiB gzip level 1, stored coding, hit | HAProxy, compresses per request | 27,816 (27,350-28,282) | 1,316 | 256,430 | 2/2 | 0.1 / 0.0 |
| 64 KiB zstd level 1, stored coding, hit | OxiRoute epoll (default) | 172,212 (166,116-178,307) | 442 | 26,669 | 2/4 | 0.1 / 0.2 |
| 64 KiB zstd level 1, stored coding, hit | OxiRoute tokio plane | 146,226 (90,486-201,966) | 501 | 26,652 | 2/2 | 0.2 / 0.1 |
| 64 KiB zstd level 1, stored coding, hit | OxiRoute io_uring | 195,873 (177,260-214,486) | 316 | 26,513 | 2/2 | 0.2 / 0.1 |
| 64 KiB brotli level 1, stored coding, hit | OxiRoute epoll (default) | 111,320 (99,791-122,850) | 578 | 27,158 | 2/3 | 0.2 / 0.2 |
| 64 KiB brotli level 1, stored coding, hit | OxiRoute tokio plane | 124,931 (122,779-127,083) | 491 | 27,130 | 2/2 | 0.3 / 0.4 |
| 64 KiB brotli level 1, stored coding, hit | OxiRoute io_uring | 128,348 (125,179-131,516) | 484 | 27,112 | 2/2 | 0.4 / 0.2 |
| 64 KiB gzip level 1, no cache | OxiRoute epoll (default) | 33,202 (32,973-33,430) | 1,510 | 164,178 | 2/2 | 3.6 / 4.4 |
| 64 KiB gzip level 1, no cache | OxiRoute tokio plane | 32,861 (32,855-32,867) | 1,691 | 164,190 | 2/2 | 1.6 / 1.0 |
| 64 KiB gzip level 1, no cache | OxiRoute io_uring | 32,459 (31,998-32,920) | 1,274 | 164,190 | 2/2 | 1.1 / 0.5 |
| 64 KiB gzip level 1, no cache | nginx | 35,131 (34,559-35,703) | 1,088 | 166,760 | 2/2 | 2.8 / 4.0 |
| 64 KiB gzip level 1, no cache | HAProxy | 24,783 (24,310-25,256) | 1,894 | 212,622 | 2/2 | 3.7 / 3.2 |

The 64 KiB hit is 8% above nginx (98,718): treat it as level. HAProxy's cache serves the 64 KiB body at 57,800 requests
per second with 63,712 instructions per request. The zstd tokio row has a wide range (90,486-201,966) on two clean runs.

### TLS and HTTP/2

32 connections, one worker, ECDSA P-256 certificate unless a lane says RSA-2048, rustls (aws-lc provider) default
backend. HTTP/2 over TLS: 8 connections of 4 streams. `io_uring` does not serve TLS: with `http_data_plane =
"io_uring"` the TLS and HTTP/2 listeners run the tokio path, so there is no separate `io_uring` column for them.

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| TLS 1.3 keep-alive, 1 KiB | OxiRoute default (tokio path) | 128,113 (126,656-129,570) | 416 | 25,574 | 2/3 | 0.4 / 0.5 |
| TLS 1.3 keep-alive, 1 KiB | OxiRoute, `http_data_plane = "tokio"` | 132,840 (131,232-134,448) | 384 | 25,574 | 2/2 | 0.4 / 0.3 |
| TLS 1.3 keep-alive, 1 KiB | OxiRoute, `http_data_plane = "epoll"` | 154,726 (154,130-155,322) | 246 | 19,206 | 2/2 | 1.3 / 0.6 |
| TLS 1.3 keep-alive, 1 KiB | nginx | 96,906 (95,519-98,293) | 442 | 36,605 | 2/2 | 0.4 / 1.7 |
| TLS 1.3 keep-alive, 1 KiB | HAProxy | 84,794 (81,487-88,101) | 636 | 50,045 | 2/2 | 1.0 / 0.5 |
| TLS 1.2 keep-alive, 1 KiB | OxiRoute default (tokio path) | 120,756 (118,543-122,970) | 338 | 25,773 | 2/2 | 0.4 / 0.5 |
| TLS 1.2 keep-alive, 1 KiB | OxiRoute, `http_data_plane = "tokio"` | 118,990 (117,935-120,045) | 388 | 25,774 | 2/2 | 0.4 / 0.5 |
| TLS 1.2 keep-alive, 1 KiB | nginx | 93,872 (91,179-96,564) | 487 | 36,298 | 2/2 | 0.4 / 0.4 |
| TLS 1.2 keep-alive, 1 KiB | HAProxy | 85,790 (84,829-86,751) | 462 | 49,612 | 2/2 | 0.3 / 0.4 |
| TLS 1.3 full handshake | OxiRoute | 16,092 (16,043-16,141) | 2,248 | 648,136 | 2/2 | 4.0 / 1.7 |
| TLS 1.3 full handshake | OxiRoute, `http_data_plane = "tokio"` | 15,596 (15,458-15,735) | 2,407 | 648,072 | 2/2 | 0.5 / 0.6 |
| TLS 1.3 full handshake | nginx | 5,131 (5,102-5,160) | 6,797 | 1,559,352 | 2/2 | 2.6 / 2.1 |
| TLS 1.3 full handshake | HAProxy | 4,674 (4,666-4,681) | 9,654 | 1,610,014 | 2/2 | 2.0 / 4.1 |
| TLS 1.2 ECDHE full handshake | OxiRoute | 16,770 (16,628-16,911) | 4,487 | 594,384 | 2/2 | 1.3 / 3.2 |
| TLS 1.2 ECDHE full handshake | OxiRoute, `http_data_plane = "tokio"` | 17,063 (16,591-17,535) | 2,413 | 594,372 | 2/2 | 0.5 / 0.8 |
| TLS 1.2 ECDHE full handshake | nginx | 5,404 (single value, both runs) | 7,258 | 1,390,006 | 2/2 | 2.3 / 4.9 |
| TLS 1.2 ECDHE full handshake | HAProxy | 4,864 (4,842-4,886) | 7,394 | 1,446,594 | 2/2 | 4.9 / 1.8 |
| TLS 1.3 resumed (ticket) | OxiRoute epoll (default) | 17,012 (16,972-17,053) | 2,448 | 536,088 | 2/2 | 0.9 / 1.1 |
| TLS 1.3 resumed (ticket) | OxiRoute, `http_data_plane = "tokio"` | 17,125 (17,077-17,173) | 2,442 | 536,066 | 2/2 | 1.0 / 1.1 |
| TLS 1.3 resumed (ticket) | nginx | 5,226 (5,215-5,236) | 11,323 | 1,480,935 | 2/2 | 1.6 / 5.3 |
| TLS 1.3 resumed (ticket) | HAProxy | 4,592 (4,549-4,636) | 8,198 | 1,521,310 | 2/2 | 4.8 / 1.9 |
| TLS 1.2 resumed (ticket) | OxiRoute | 57,527 (57,260-57,794) | 1,022 | 109,704 | 2/2 | 2.3 / 5.4 |
| TLS 1.2 resumed (ticket) | OxiRoute, `http_data_plane = "tokio"` | 53,758 (52,123-55,393) | 1,129 | 109,792 | 2/2 | 1.6 / 2.0 |
| TLS 1.2 resumed (ticket) | nginx | 11,902 (11,695-12,109) | 3,646 | 271,880 | 2/2 | 2.3 / 5.2 |
| TLS 1.2 resumed (ticket) | HAProxy | 8,871 (8,821-8,921) | 4,089 | 303,218 | 2/2 | 4.9 / 2.0 |
| TLS 1.2 resumed (session id) | OxiRoute default (tokio path) | 57,089 (56,867-57,311) | 1,050 | 93,534 | 2/2 | 0.2 / 1.2 |
| TLS 1.2 resumed (session id) | OxiRoute, `http_data_plane = "tokio"` | 56,312 (55,837-56,786) | 1,096 | 93,544 | 2/2 | 0.6 / 1.2 |
| TLS 1.2 resumed (session id) | nginx | 11,937 (11,657-12,217) | 3,271 | 261,420 | 2/2 | 0.9 / 0.5 |
| TLS 1.2 resumed (session id) | HAProxy | 9,076 (8,939-9,214) | 4,030 | 291,534 | 2/2 | 0.6 / 0.2 |
| TLS 1.3 full handshake, RSA-2048 certificate | OxiRoute default (tokio path) | 1,871 (1,865-1,877) | 28,930 | 6,785,664 | 2/2 | 0.3 / 0.3 |
| TLS 1.3 full handshake, RSA-2048 certificate | OxiRoute, `http_data_plane = "tokio"` | 1,938 (1,934-1,943) | 17,798 | 6,785,638 | 2/2 | 0.2 / 0.1 |
| TLS 1.3 full handshake, RSA-2048 certificate | nginx | 1,536 (1,532-1,540) | 22,445 | 7,691,866 | 2/2 | 0.2 / 0.1 |
| TLS 1.3 full handshake, RSA-2048 certificate | HAProxy | 1,512 (1,505-1,519) | 22,476 | 7,738,128 | 2/2 | 0.3 / 0.2 |
| TLS 1.3 full handshake with a client certificate | OxiRoute default (tokio path) | 5,273 (5,229-5,317) | 8,090 | 2,092,113 | 2/2 | 0.3 / 0.3 |
| TLS 1.3 full handshake with a client certificate | OxiRoute, `http_data_plane = "tokio"` | 5,288 (5,256-5,320) | 6,561 | 2,091,943 | 2/2 | 0.4 / 0.3 |
| TLS 1.3 full handshake with a client certificate | nginx | 2,746 (2,722-2,771) | 12,628 | 3,230,904 | 2/2 | 0.4 / 0.2 |
| TLS 1.3 full handshake with a client certificate | HAProxy | 2,613 (2,591-2,635) | 13,224 | 3,293,604 | 2/2 | 0.2 / 0.1 |

The default HTTPS listener runs on the tokio path (25.6k instructions per request); with `http_data_plane = "epoll"`
and a rustls profile the same lane is 21% faster and 25% cheaper in user instructions. In the handshake rows OxiRoute
used 1.4 to 1.5 CPUs of the 2 (handshakes run on helper threads, `tls_handshake_threads`); nginx and HAProxy used one.

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| HTTP/2 over TLS, 1 KiB | OxiRoute | 132,781 (131,965-133,597) | 374 | 31,617 | 2/2 | 0.8 / 0.8 |
| HTTP/2 over TLS, 1 KiB | OxiRoute, `http_data_plane = "tokio"` | 128,492 (128,321-128,662) | 388 | 31,552 | 2/2 | 1.0 / 0.9 |
| HTTP/2 over TLS, 1 KiB | nginx | 94,742 (94,438-95,046) | 406 | 33,388 | 2/2 | 1.2 / 0.9 |
| HTTP/2 over TLS, 1 KiB | HAProxy | 85,054 (85,030-85,079) | 546 | 45,777 | 2/3 | 0.3 / 0.8 |
| HTTP/2 over TLS, 64 KiB | OxiRoute | 27,914 (27,842-27,985) | 2,103 | 206,136 | 2/2 | 3.8 / 5.7 |
| HTTP/2 over TLS, 64 KiB | OxiRoute, `http_data_plane = "tokio"` | 28,144 (27,793-28,496) | 1,820 | 206,137 | 2/2 | 0.4 / 0.2 |
| HTTP/2 over TLS, 64 KiB | nginx | 18,159 (18,106-18,212) | 2,192 | 254,268 | 2/2 | 0.1 / 0.3 |
| HTTP/2 over TLS, 64 KiB | HAProxy | 20,706 (20,612-20,800) | 1,878 | 234,662 | 2/2 | 7.7 / 5.0 |
| HTTP/2 over TLS, POST 1 KiB | OxiRoute | 113,696 (112,550-114,842) | 424 | 37,444 | 2/2 | 0.4 / 0.7 |
| HTTP/2 over TLS, POST 1 KiB | OxiRoute, `http_data_plane = "tokio"` | 115,244 (113,400-117,089) | 420 | 37,452 | 2/2 | 0.7 / 0.9 |
| HTTP/2 over TLS, POST 1 KiB | nginx | 72,002 (71,052-72,953) | 546 | 40,856 | 2/4 | 0.5 / 0.7 |
| HTTP/2 over TLS, POST 1 KiB | HAProxy | 71,801 (71,644-71,958) | 625 | 55,414 | 2/2 | 0.6 / 0.6 |
| Upstream HTTP/2, 1 KiB (HTTP/1.1 front end) | OxiRoute epoll (default) | 40,125 (39,352-40,898) | 1,226 | 98,230 | 2/2 | 0.4 / 0.4 |
| Upstream HTTP/2, 1 KiB (HTTP/1.1 front end) | OxiRoute tokio plane | 39,620 (38,903-40,337) | 1,228 | 98,130 | 2/2 | 0.6 / 0.4 |
| Upstream HTTP/2, 1 KiB (HTTP/1.1 front end) | OxiRoute io_uring | 39,476 (39,077-39,874) | 1,322 | 98,176 | 2/2 | 0.4 / 0.6 |
| Upstream HTTP/2, 1 KiB (HTTP/1.1 front end) | nginx | 87,964 (87,716-88,211) | 404 | 39,694 | 2/2 | 0.5 / 0.5 |
| Upstream HTTP/2, 1 KiB (HTTP/1.1 front end) | HAProxy | 87,490 (86,706-88,275) | 582 | 48,622 | 2/2 | 0.5 / 0.8 |
| Upstream HTTP/2, POST 1 KiB (HTTP/1.1 front end) | OxiRoute epoll (default) | 32,580 (32,463-32,696) | 1,314 | 124,256 | 2/2 | 0.4 / 0.5 |
| Upstream HTTP/2, POST 1 KiB (HTTP/1.1 front end) | OxiRoute tokio plane | 31,590 (31,378-31,801) | 1,562 | 124,106 | 2/2 | 0.3 / 0.4 |
| Upstream HTTP/2, POST 1 KiB (HTTP/1.1 front end) | OxiRoute io_uring | 32,220 (32,216-32,225) | 1,332 | 124,248 | 2/2 | 0.4 / 0.3 |
| Upstream HTTP/2, POST 1 KiB (HTTP/1.1 front end) | nginx | 78,782 (77,010-80,553) | 582 | 44,698 | 2/2 | 0.5 / 0.5 |
| Upstream HTTP/2, POST 1 KiB (HTTP/1.1 front end) | HAProxy | 89,446 (88,761-90,131) | 588 | 48,604 | 2/2 | 0.4 / 0.4 |

### HTTPS and HTTP/2 over TLS with many connections

One worker, `aged_per_worker` is the opt-in `connection_scheduling` mode; HTTP/2 rows use 4 streams per connection.

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | proxy CPUs used | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| TLS 1.3 keep-alive, 512 connections | OxiRoute epoll (default) | 115,171 (113,066-117,276) | 68,332 | 25,542 | 2/2 | 0.99 | 0.2 / 0.4 |
| TLS 1.3 keep-alive, 512 connections | OxiRoute epoll, aged | 109,966 (104,632-115,300) | 14,490 | 25,911 | 2/2 | 0.99 | 0.2 / 0.3 |
| TLS 1.3 keep-alive, 512 connections | nginx | 86,158 (85,401-86,916) | 13,220 | 37,060 | 2/2 | 0.99 | 0.3 / 0.3 |
| TLS 1.3 keep-alive, 512 connections | HAProxy | 77,212 (76,318-78,107) | 7,618 | 53,845 | 2/2 | 0.99 | 0.2 / 0.1 |
| TLS 1.3 keep-alive, 1,024 connections | OxiRoute epoll (default) | 109,691 (108,721-110,661) | 167,276 | 25,539 | 2/2 | 0.99 | 0.1 / 0.2 |
| TLS 1.3 keep-alive, 1,024 connections | OxiRoute epoll, aged | 82,268 (80,567-83,970) | 37,704 | 25,956 | 2/2 | 0.99 | 0.1 / 0.3 |
| TLS 1.3 keep-alive, 1,024 connections | nginx | 62,962 (62,684-63,239) | 18,842 | 37,220 | 2/2 | 0.99 | 0.1 / 0.1 |
| TLS 1.3 keep-alive, 1,024 connections | HAProxy | 55,584 (54,443-56,724) | 22,982 | 54,101 | 2/2 | 0.99 | 0.1 / 0.1 |
| TLS 1.3 keep-alive, 2,048 connections | OxiRoute epoll (default) | 111,988 (111,267-112,710) | 358,928 | 25,554 | 2/2 | 0.99 | 0.0 / 0.4 |
| TLS 1.3 keep-alive, 2,048 connections | OxiRoute epoll, aged | 81,552 (80,402-82,702) | 76,550 | 26,008 | 2/2 | 0.99 | 0.1 / 0.1 |
| TLS 1.3 keep-alive, 2,048 connections | nginx | 43,720 (43,025-44,415) | 82,095 | 37,290 | 2/2 | 0.99 | 0.1 / 0.2 |
| TLS 1.3 keep-alive, 2,048 connections | HAProxy | 43,892 (43,431-44,352) | 50,394 | 54,272 | 2/2 | 0.99 | 0.1 / 0.1 |
| HTTP/2 over TLS, 128 connections of 4 streams | OxiRoute epoll (default) | 120,869 (120,815-120,923) | 38,278 | 32,192 | 2/2 | 0.99 | 0.0 / 0.0 |
| HTTP/2 over TLS, 128 connections of 4 streams | OxiRoute epoll, aged | 120,216 (118,470-121,962) | 10,116 | 33,134 | 2/2 | 0.99 | 0.0 / 0.0 |
| HTTP/2 over TLS, 128 connections of 4 streams | nginx | 92,706 (90,934-94,479) | 7,032 | 33,818 | 2/2 | 0.99 | 0.0 / 0.0 |
| HTTP/2 over TLS, 128 connections of 4 streams | HAProxy | 80,662 (78,268-83,056) | 7,165 | 49,909 | 2/2 | 0.99 | 0.0 / 0.0 |
| HTTP/2 over TLS, 256 connections of 4 streams | OxiRoute epoll (default) | 117,671 (117,338-118,004) | 97,820 | 32,310 | 2/2 | 0.99 | 0.0 / 0.1 |
| HTTP/2 over TLS, 256 connections of 4 streams | OxiRoute epoll, aged | 99,607 (98,355-100,859) | 23,918 | 32,890 | 2/2 | 0.99 | 0.0 / 0.1 |
| HTTP/2 over TLS, 256 connections of 4 streams | nginx | 73,317 (72,244-74,390) | 21,813 | 33,944 | 2/2 | 0.99 | 0.0 / 0.0 |
| HTTP/2 over TLS, 256 connections of 4 streams | HAProxy | 65,752 (65,604-65,900) | 17,010 | 50,232 | 2/2 | 0.99 | 0.0 / 0.0 |
| HTTP/2 over TLS, 512 connections of 4 streams | OxiRoute epoll (default) | 115,432 (114,615-116,249) | 213,066 | 32,332 | 2/2 | 0.99 | 0.0 / 0.0 |
| HTTP/2 over TLS, 512 connections of 4 streams | OxiRoute epoll, aged | 79,068 (79,055-79,080) | 58,680 | 32,680 | 2/2 | 0.99 | 0.0 / 0.0 |
| HTTP/2 over TLS, 512 connections of 4 streams | nginx | 54,493 (53,283-55,703) | 54,776 | 34,050 | 2/2 | 0.99 | 0.0 / 0.1 |
| HTTP/2 over TLS, 512 connections of 4 streams | HAProxy | 56,720 (56,262-57,177) | 39,148 | 50,773 | 2/2 | 0.99 | 0.0 / 0.0 |

#### Static files

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 1 MiB | OxiRoute epoll (default) | 1,699 (1,660-5,407) | 21,805 | 8,906 | 3/3 | 0.3 / 0.1 |
| 1 MiB | nginx | 1,668 (1,665-1,697) | 20,546 | 13,272 | 3/3 | 0.8 / 0.3 |
| 1 MiB over TLS 1.3, kernel TLS and `sendfile` | OxiRoute epoll (default) | 1,394 (1,392-3,414) | 25,537 | 9,633 | 3/3 | 0.1 / 0.2 |
| 1 MiB over TLS 1.3, user-space TLS | nginx | 694 (694-695) | 48,673 | 2,964,173 | 3/3 | 0.3 / 0.3 |
| 1 KiB | OxiRoute epoll (default) | 343,474 (342,925-344,022) | 189 | 6,485 | 2/2 | 0.1 / 0.7 |
| 1 KiB | nginx | 78,452 (77,462-79,441) | 449 | 13,212 | 2/2 | 0.2 / 0.2 |

The first of the three OxiRoute runs of each 1 MiB lane is faster than the later two (plain: 5,407, 1,699, 1,660; TLS:
3,414, 1,394, 1,392) with equal instruction counts; the earlier pass showed the same pattern (5,190 and 1,714). The
median is the later, steady rate and is level with nginx (1,668; nginx over TLS 694). The cause of the faster first run
was not isolated. The 1 KiB lane is 4.4 times nginx (343,474 against 78,452).

### Scaling over two and four workers and aged scheduling

`listener_workers = N`, nginx `worker_processes = N`, HAProxy `nbthread N`. One and two workers: proxy CPUs 8-9, origin
10-11, load generator 12-15; four workers: proxy CPUs 8-11, origin 12-13, load generator 14, 15 and 28-31. The `aged` column is the `connection_scheduling = "aged_per_worker"` opt-in. Two workers with every data
plane, 1 KiB:

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | proxy CPUs used | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 KiB, 64 connections | OxiRoute epoll, default scheduling | 177,640 (175,792-179,489) | 528 | 11,663 | 2/3 | 0.99 | 0.5 / 0.4 |
| 1 KiB, 64 connections | OxiRoute tokio plane | 249,088 (248,380-249,795) | 426 | 18,683 | 2/6 | 1.94 | 1.3 / 1.9 |
| 1 KiB, 64 connections | OxiRoute io_uring | 303,192 (290,995-315,390) | 389 | 15,852 | 2/2 | 1.93 | 6.3 / 6.0 |
| 1 KiB, 64 connections | OxiRoute epoll, aged | 326,586 (321,623-331,549) | 302 | 12,350 | 2/3 | 1.91 | 1.9 / 1.1 |
| 1 KiB, 64 connections | nginx | 249,555 (233,027-266,083) | 364 | 19,906 | 2/2 | 1.92 | 4.9 / 5.2 |
| 1 KiB, 64 connections | HAProxy | 218,280 (212,874-223,686) | 398 | 35,279 | 2/3 | 1.96 | 0.5 / 0.4 |
| 1 KiB, 256 connections | OxiRoute epoll, default scheduling | 183,176 (181,244-185,107) | 1,590 | 11,664 | 2/3 | 0.99 | 0.5 / 0.6 |
| 1 KiB, 256 connections | OxiRoute tokio plane | 267,236 (261,479-272,994) | 1,610 | 18,596 | 2/3 | 1.96 | 0.6 / 0.8 |
| 1 KiB, 256 connections | OxiRoute io_uring | 291,636 (283,932-299,341) | 2,644 | 15,880 | 2/2 | 1.92 | 5.1 / 3.8 |
| 1 KiB, 256 connections | OxiRoute epoll, aged | 283,586 (279,645-287,527) | 2,878 | 12,530 | 2/2 | 1.88 | 3.3 / 6.8 |
| 1 KiB, 256 connections | nginx | 248,264 (236,454-260,073) | 1,390 | 19,926 | 2/2 | 1.96 | 5.5 / 5.1 |
| 1 KiB, 256 connections | HAProxy | 206,878 (203,766-209,989) | 1,932 | 37,232 | 2/2 | 1.97 | 0.4 / 0.6 |

With two workers and the default `work_stealing` scheduling the OxiRoute process used 0.99 CPUs in these two lanes and
its rate was the one-worker rate, while the aged scheduling used both CPUs (1.91 to 1.97). Whether the first load run
after process start behaves differently was checked with 6 consecutive runs against one process (no tracing): at 64
connections the first run reached 273,191 requests per second with both worker threads busy (4.37 and 3.22
CPU-seconds) and the next five 181,045 to 182,513 with one thread at 4.4 CPU-seconds; at 256 connections 354,233
(4.36 and 4.34) and then 184,500 to 184,976. The tokio plane (272,495) and `io_uring` (335,427) used both threads in a
one run each. In the larger matrix the default epoll range between its two clean runs is wide at four workers (for
example 184,268 to 341,072 at 64 connections) while the aged scheduling stays within 3%. The shipped default is the
lower row; the cause in the connection distribution was not investigated here.

One, two and four workers at 512 to 2,048 connections, and the 512-connection lane in which 1 origin response in 500
is delayed 200 ms:

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | proxy CPUs used | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| One worker, 512 connections | OxiRoute epoll (default) | 183,648 (179,930-187,367) | 4,328 | 11,669 | 2/2 | 0.99 | 0.3 / 0.5 |
| One worker, 512 connections | OxiRoute epoll, aged | 181,716 (179,048-184,383) | 3,480 | 12,324 | 2/2 | 0.99 | 0.3 / 0.5 |
| One worker, 512 connections | nginx | 125,426 (123,539-127,314) | 6,054 | 20,362 | 2/2 | 0.99 | 0.2 / 0.4 |
| One worker, 512 connections | HAProxy | 103,606 (100,374-106,838) | 7,041 | 39,448 | 2/2 | 0.99 | 0.2 / 0.3 |
| One worker, 2,048 connections | OxiRoute epoll (default) | 152,380 (149,619-155,142) | 22,921 | 11,668 | 2/2 | 0.99 | 0.2 / 0.4 |
| One worker, 2,048 connections | OxiRoute epoll, aged | 152,884 (150,981-154,786) | 24,514 | 12,323 | 2/2 | 0.99 | 0.1 / 0.3 |
| One worker, 2,048 connections | nginx | 72,082 (71,609-72,554) | 31,860 | 20,446 | 2/2 | 0.99 | 0.1 / 0.2 |
| One worker, 2,048 connections | HAProxy | 64,124 (63,218-65,029) | 35,394 | 39,878 | 2/2 | 0.99 | 0.1 / 0.1 |
| One worker, 512, 1 in 500 origin responses delayed 200 ms connections | OxiRoute epoll (default) | 174,850 (173,651-176,049) | 2,751 | 11,748 | 2/2 | 0.94 | 0.5 / 1.7 |
| One worker, 512, 1 in 500 origin responses delayed 200 ms connections | OxiRoute epoll, aged | 173,242 (172,686-173,798) | 2,824 | 12,404 | 2/2 | 0.94 | 1.7 / 1.7 |
| One worker, 512, 1 in 500 origin responses delayed 200 ms connections | nginx | 117,462 (117,282-117,643) | 9,102 | 20,356 | 2/2 | 0.94 | 3.9 / 1.9 |
| One worker, 512, 1 in 500 origin responses delayed 200 ms connections | HAProxy | 102,450 (100,322-104,577) | 5,514 | 39,124 | 2/2 | 0.95 | 0.0 / 0.0 |
| Two workers, 512 connections | OxiRoute epoll (default) | 260,457 (178,450-342,464) | 3,352 | 11,674 | 2/2 | 1.47 | 0.0 / 0.4 |
| Two workers, 512 connections | OxiRoute epoll, aged | 336,584 (333,378-339,789) | 2,250 | 12,358 | 2/2 | 1.97 | 0.1 / 0.6 |
| Two workers, 512 connections | nginx | 252,868 (249,412-256,323) | 2,954 | 20,134 | 2/2 | 1.98 | 0.1 / 0.6 |
| Two workers, 512 connections | HAProxy | 206,559 (199,866-213,252) | 3,158 | 37,479 | 2/2 | 1.98 | 0.1 / 0.2 |
| Two workers, 1,024 connections | OxiRoute epoll (default) | 285,210 (246,018-324,403) | 7,664 | 11,786 | 2/2 | 1.80 | 0.2 / 0.8 |
| Two workers, 1,024 connections | OxiRoute epoll, aged | 310,097 (294,975-325,219) | 9,716 | 12,412 | 2/2 | 1.95 | 0.2 / 0.9 |
| Two workers, 1,024 connections | nginx | 206,178 (205,126-207,231) | 5,618 | 20,174 | 2/2 | 1.97 | 0.2 / 0.6 |
| Two workers, 1,024 connections | HAProxy | 160,014 (157,706-162,322) | 7,530 | 38,574 | 2/2 | 1.98 | 0.5 / 0.2 |
| Two workers, 2,048 connections | OxiRoute epoll (default) | 308,658 (298,889-318,428) | 20,046 | 11,642 | 2/2 | 1.95 | 1.7 / 3.5 |
| Two workers, 2,048 connections | OxiRoute epoll, aged | 270,051 (260,129-279,973) | 12,636 | 12,376 | 2/2 | 1.97 | 1.4 / 2.6 |
| Two workers, 2,048 connections | nginx | 137,250 (135,988-138,512) | 23,322 | 20,263 | 2/2 | 1.98 | 1.2 / 3.8 |
| Two workers, 2,048 connections | HAProxy | 118,959 (117,440-120,478) | 32,938 | 38,514 | 2/2 | 1.98 | 1.2 / 1.7 |
| Two workers, 512, 1 in 500 origin responses delayed 200 ms connections | OxiRoute epoll (default) | 212,216 (181,049-243,383) | 3,624 | 11,864 | 2/2 | 1.28 | 2.3 / 1.8 |
| Two workers, 512, 1 in 500 origin responses delayed 200 ms connections | OxiRoute epoll, aged | 314,895 (308,640-321,150) | 2,468 | 12,428 | 2/2 | 1.86 | 1.5 / 3.2 |
| Two workers, 512, 1 in 500 origin responses delayed 200 ms connections | nginx | 232,924 (227,391-238,457) | 4,132 | 20,211 | 2/2 | 1.87 | 2.5 / 2.0 |
| Two workers, 512, 1 in 500 origin responses delayed 200 ms connections | HAProxy | 200,084 (199,750-200,419) | 2,541 | 36,703 | 2/2 | 1.89 | 0.0 / 0.0 |
| Four workers, 64 connections | OxiRoute epoll (default) | 262,670 (184,268-341,072) | 398 | 11,841 | 2/2 | 1.75 | 0.2 / 0.4 |
| Four workers, 64 connections | OxiRoute epoll, aged | 331,513 (330,234-332,792) | 375 | 13,214 | 2/2 | 2.69 | 0.2 / 0.7 |
| Four workers, 64 connections | nginx | 348,248 (342,566-353,929) | 326 | 20,226 | 2/2 | 3.27 | 0.0 / 0.5 |
| Four workers, 64 connections | HAProxy | 352,972 (351,302-354,642) | 324 | 35,034 | 2/2 | 3.72 | 0.1 / 0.9 |
| Four workers, 256 connections | OxiRoute epoll (default) | 266,735 (184,589-348,881) | 1,598 | 11,841 | 2/2 | 1.79 | 0.2 / 0.6 |
| Four workers, 256 connections | OxiRoute epoll, aged | 344,030 (342,926-345,133) | 1,218 | 13,219 | 2/2 | 2.77 | 0.1 / 0.5 |
| Four workers, 256 connections | nginx | 352,074 (346,764-357,384) | 1,583 | 20,151 | 2/2 | 3.25 | 0.0 / 1.0 |
| Four workers, 256 connections | HAProxy | 357,568 (354,431-360,706) | 2,146 | 35,264 | 2/2 | 3.75 | 0.1 / 0.9 |
| Four workers, 512 connections | OxiRoute epoll (default) | 272,122 (182,245-362,000) | 3,050 | 11,841 | 2/2 | 1.83 | 0.4 / 0.5 |
| Four workers, 512 connections | OxiRoute epoll, aged | 332,712 (326,444-338,981) | 6,988 | 13,164 | 2/2 | 2.71 | 0.1 / 0.9 |
| Four workers, 512 connections | nginx | 345,814 (344,343-347,285) | 6,548 | 20,167 | 2/2 | 3.23 | 0.1 / 1.1 |
| Four workers, 512 connections | HAProxy | 338,920 (328,706-349,134) | 6,560 | 35,806 | 2/2 | 3.61 | 0.1 / 1.2 |
| Four workers, 1,024 connections | OxiRoute epoll (default) | 264,854 (176,806-352,902) | 10,338 | 11,851 | 2/2 | 1.83 | 0.3 / 0.6 |
| Four workers, 1,024 connections | OxiRoute epoll, aged | 295,522 (293,528-297,516) | 20,348 | 13,227 | 2/2 | 2.71 | 0.2 / 0.9 |
| Four workers, 1,024 connections | nginx | 300,136 (294,121-306,151) | 18,709 | 20,216 | 2/2 | 3.10 | 0.1 / 1.8 |
| Four workers, 1,024 connections | HAProxy | 299,608 (297,133-302,082) | 18,706 | 35,825 | 2/2 | 3.58 | 0.4 / 1.5 |
| Four workers, 2,048 connections | OxiRoute epoll (default) | 271,604 (196,367-346,842) | 26,052 | 11,973 | 2/2 | 2.21 | 0.2 / 0.8 |
| Four workers, 2,048 connections | OxiRoute epoll, aged | 282,846 (279,537-286,154) | 45,376 | 13,032 | 2/2 | 2.70 | 0.2 / 0.8 |
| Four workers, 2,048 connections | nginx | 266,907 (266,466-267,348) | 45,483 | 20,208 | 2/2 | 3.13 | 0.1 / 1.3 |
| Four workers, 2,048 connections | HAProxy | 229,849 (220,657-239,041) | 39,006 | 36,967 | 2/2 | 3.73 | 0.2 / 0.9 |
| Four workers, 512, 1 in 500 origin responses delayed 200 ms connections | OxiRoute epoll (default) | 239,054 (170,223-307,886) | 3,776 | 11,920 | 2/2 | 1.63 | 0.1 / 0.7 |
| Four workers, 512, 1 in 500 origin responses delayed 200 ms connections | OxiRoute epoll, aged | 318,366 (312,385-324,347) | 4,632 | 13,213 | 2/2 | 2.62 | 0.1 / 0.6 |
| Four workers, 512, 1 in 500 origin responses delayed 200 ms connections | nginx | 324,852 (317,843-331,862) | 4,290 | 20,210 | 2/2 | 3.11 | 0.0 / 1.0 |
| Four workers, 512, 1 in 500 origin responses delayed 200 ms connections | HAProxy | 303,745 (299,821-307,669) | 4,696 | 35,646 | 2/2 | 3.33 | 0.2 / 1.0 |

### Memory

Proportional set size (PSS) of the proxy process with one worker, idle then with N idle keep-alive connections held
open, then after they are closed (the process is the same across the states, 32 first, then 1,024; three processes,
medians shown, range at most 0.12 MB for every state). Binary on disk, instruction-count harness.

| State | PSS, MB (3 processes) |
| --- | ---: |
| Idle | 13.2 (13.2-13.2) |
| 32 connections held | 13.8 (13.8-13.8) |
| 32 connections drained | 13.9 (13.9-13.9) |
| 1,024 connections held | 15.6 (15.6-15.7) |
| 1,024 connections drained | 15.7 (15.7-15.8) |

Of the idle 13.2 MB, 8.4 MB is file-backed (the binary), 4.8 MB anonymous. The 1,024-held figure is lower than the
24.2 MB of the earlier memory pass (below); the method here is one process driven by the same load tool with a
single request per connection, so a harness difference cannot be excluded.

The comparison harness measured the same three states for all three proxies (3 processes each, median with min-max).
Its OxiRoute figures are lower than the instruction-count harness figures above; the difference was not isolated:

| State | OxiRoute | nginx | HAProxy |
| --- | ---: | ---: | ---: |
| Idle | 9.9 (9.8-9.9) | 10.6 (10.5-10.9) | 30.9 (30.4-32.3) |
| 32 connections held | 10.5 (10.5-10.5) | 11.0 (10.9-11.2) | 31.4 (30.9-32.7) |
| 32 connections drained | 10.5 (10.5-10.5) | 11.0 (10.9-11.2) | 31.4 (30.9-32.7) |
| 1,024 connections held | 12.2 (12.2-12.2) | 19.9 (19.8-20.2) | 40.1 (39.7-41.5) |
| 1,024 connections drained | 12.2 (12.2-12.2) | 19.9 (19.8-20.3) | 40.1 (39.7-41.5) |

Memory after configuration reloads, one worker, a configuration with 1,000 exact routes plus a catch-all (3 processes,
medians with min-max; PSS in MB; OxiRoute `generation reload` through the management API with the previous generation
retired before measuring, nginx `SIGHUP`, HAProxy master-worker `SIGUSR2`, each after settling for one second):

| State | OxiRoute | nginx | HAProxy |
| --- | ---: | ---: | ---: |
| Idle | 19.8 (19.8-19.9) | 17.7 (17.7-17.7) | 36.6 (35.0-36.8) |
| After 1 reload | 30.8 (30.6-36.9) | 25.3 (25.3-25.3) | 34.9 (34.8-36.6) |
| After 10 reloads | 39.3 (39.3-40.0) | 25.5 (25.5-25.6) | 34.9 (34.9-36.9) |
| After 50 reloads | 41.6 (41.3-41.6) | 26.2 (26.2-26.3) | 36.3 (34.8-36.5) |

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

Comparison harness, one worker, loopback, shipped binary, 8 QUIC connections of 4 streams unless stated. The `cubic`
rows are the same OxiRoute listener with `http3_congestion_control = "cubic"`. nginx HTTP/2 over TCP is the same
nginx build on the same lanes (the new-connection row opens a new TLS connection per request).

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Proxy, 8 connections of 4 streams, 1 KiB | OxiRoute HTTP/3, `bbr` (default) | 70,169 (69,421-70,917) | 796 | 68,892 | 2/2 | 0.3 / 0.6 |
| Proxy, 8 connections of 4 streams, 1 KiB | OxiRoute HTTP/3, `cubic` | 70,101 (69,899-70,303) | 743 | 68,570 | 2/2 | 0.3 / 0.7 |
| Proxy, 8 connections of 4 streams, 1 KiB | nginx HTTP/3 | 66,773 (66,457-67,089) | 686 | 55,662 | 2/2 | 0.6 / 0.8 |
| Proxy, 8 connections of 4 streams, 1 KiB | nginx HTTP/2 over TCP | 94,742 (94,438-95,046) | 406 | 33,388 | 2/2 | 1.2 / 0.9 |
| Direct 1 KiB response | OxiRoute HTTP/3, `bbr` (default) | 189,926 (189,004-190,849) | 314 | 34,653 | 2/2 | 0.2 / 0.7 |
| Direct 1 KiB response | OxiRoute HTTP/3, `cubic` | 193,052 (191,943-194,161) | 300 | 34,346 | 2/3 | 0.6 / 1.0 |
| Direct 1 KiB response | nginx HTTP/3 | 125,342 (124,895-125,790) | 440 | 41,724 | 2/2 | 0.3 / 0.6 |
| Direct 1 KiB response | nginx HTTP/2 over TCP | 176,600 (174,853-178,347) | 267 | 22,458 | 2/2 | 0.4 / 0.8 |
| Proxy POST 1 KiB | OxiRoute HTTP/3, `bbr` (default) | 64,250 (62,638-65,861) | 774 | 67,342 | 2/2 | 0.4 / 0.4 |
| Proxy POST 1 KiB | OxiRoute HTTP/3, `cubic` | 64,849 (64,748-64,950) | 687 | 67,220 | 2/3 | 0.3 / 0.7 |
| Proxy POST 1 KiB | nginx HTTP/3 | 60,725 (60,585-60,865) | 896 | 59,489 | 2/2 | 0.5 / 0.9 |
| Proxy POST 1 KiB | nginx HTTP/2 over TCP | 72,002 (71,052-72,953) | 546 | 40,856 | 2/4 | 0.5 / 0.7 |
| New QUIC (HTTP/2: new TLS) connection per request, direct | OxiRoute HTTP/3, `bbr` (default) | 5,250 (5,133-5,279) | 2,501 | 1,881,115 | 3/3 | 0.3 / 0.6 |
| New QUIC (HTTP/2: new TLS) connection per request, direct | OxiRoute HTTP/3, `cubic` | 5,140 (5,117-5,149) | 3,388 | 1,876,930 | 3/3 | 0.5 / 0.6 |
| New QUIC (HTTP/2: new TLS) connection per request, direct | nginx HTTP/3 | 3,376 (3,274-3,421) | 2,729 | 2,549,549 | 3/3 | 0.4 / 0.3 |
| New QUIC (HTTP/2: new TLS) connection per request, direct | nginx HTTP/2 over TCP | 5,057 (4,971-5,143) | 1,886 | 1,573,780 | 2/2 | 0.1 / 0.2 |

OxiRoute HTTP/3 is level with nginx HTTP/3 through a proxy (70,169 against 66,773 requests per second), 1.5 times it for
a direct response (189,926 against 125,342) and 1.6 times it for new connections (5,250 against 3,376, with 1.9 against
2.5 million instructions per request). Through a proxy, nginx HTTP/2 over TCP serves 94,742 requests per second: more
than either HTTP/3 stack. `bbr` and `cubic` are level on a loss-free loopback.

Instruction-count harness (`benchmarks/r2-data-path`, 3 repetitions):

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
its unfairness against other flows are in the tracked summary, not re-measured.

### WebSocket, TCP relay, pools, features and other lanes (comparison harness)

32 connections unless stated; HAProxy and nginx run the same functional configuration (the nginx stream module for the
TCP relay).

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| WebSocket echo, 1 KiB message, 16 connections | OxiRoute epoll (default) | 174,002 (170,920-177,083) | 138 | 9,918 | 2/2 | 0.6 / 0.8 |
| WebSocket echo, 1 KiB message, 16 connections | nginx | 225,079 (219,520-230,638) | 98 | 1,528 | 2/2 | 0.4 / 0.7 |
| WebSocket echo, 1 KiB message, 16 connections | HAProxy | 170,088 (164,846-175,330) | 139 | 8,279 | 2/2 | 0.5 / 0.9 |
| TCP relay echo, 1 KiB, 16 connections | OxiRoute epoll (default) | 202,172 (200,374-203,970) | 144 | 5,046 | 2/2 | 0.7 / 1.0 |
| TCP relay echo, 1 KiB, 16 connections | nginx | 208,781 (199,431-218,131) | 96 | 2,026 | 2/2 | 1.2 / 1.5 |
| TCP relay echo, 1 KiB, 16 connections | HAProxy | 183,838 (183,330-184,345) | 130 | 7,310 | 2/2 | 1.2 / 1.3 |
| TCP relay echo, 64 KiB, 16 connections | OxiRoute epoll (default) | 71,078 (69,495-72,662) | 430 | 6,744 | 2/2 | 0.6 / 1.6 |
| TCP relay echo, 64 KiB, 16 connections | nginx | 32,340 (32,207-32,474) | 717 | 5,810 | 2/2 | 1.0 / 0.7 |
| TCP relay echo, 64 KiB, 16 connections | HAProxy | 28,798 (28,339-29,256) | 903 | 18,145 | 2/2 | 0.6 / 0.4 |
| Pool `least_connections`, 8 servers | OxiRoute epoll (default) | 177,662 (174,780-180,543) | 264 | 12,268 | 2/2 | 0.7 / 0.8 |
| Pool `least_connections`, 8 servers | nginx | 130,749 (128,640-132,858) | 357 | 20,492 | 2/2 | 0.5 / 0.4 |
| Pool `least_connections`, 8 servers | HAProxy | 113,962 (113,570-114,355) | 478 | 35,805 | 2/2 | 0.4 / 0.5 |
| Pool `weighted_round_robin`, 8 servers | OxiRoute epoll (default) | 183,626 (183,132-184,120) | 208 | 12,630 | 2/2 | 0.3 / 0.4 |
| Pool `weighted_round_robin`, 8 servers | nginx | 131,884 (129,739-134,030) | 350 | 20,276 | 2/2 | 0.3 / 0.3 |
| Pool `weighted_round_robin`, 8 servers | HAProxy | 113,846 (111,485-116,206) | 442 | 35,334 | 2/2 | 0.3 / 0.4 |
| HTTP/1.1 keep-alive, 1 connection | OxiRoute epoll (default) | 39,430 (38,965-39,896) | 36 | 12,419 | 2/2 | 1.1 / 0.9 |
| HTTP/1.1 keep-alive, 1 connection | nginx | 35,394 (34,620-36,168) | 42 | 20,845 | 2/2 | 0.5 / 0.7 |
| HTTP/1.1 keep-alive, 1 connection | HAProxy | 35,632 (34,622-36,641) | 42 | 37,100 | 2/2 | 0.4 / 0.7 |
| HTTP/1.1, 2 KiB request headers | OxiRoute epoll (default) | 141,700 (140,275-143,124) | 334 | 45,967 | 2/2 | 0.3 / 0.5 |
| HTTP/1.1, 2 KiB request headers | nginx | 92,754 (92,506-93,002) | 438 | 48,493 | 2/2 | 0.4 / 0.6 |
| HTTP/1.1, 2 KiB request headers | HAProxy | 98,984 (97,560-100,409) | 473 | 54,752 | 2/2 | 0.4 / 0.4 |
| Access log on | OxiRoute epoll (default) | 151,706 (150,716-152,697) | 302 | 13,584 | 2/3 | 7.3 / 6.2 |
| Access log on | nginx | 81,698 (80,810-82,587) | 474 | 22,322 | 2/2 | 6.6 / 6.0 |
| Access log on | HAProxy | 69,423 (68,327-70,519) | 569 | 41,102 | 2/2 | 7.2 / 5.4 |
| Request id on | OxiRoute epoll (default) | 167,191 (162,492-171,890) | 282 | 11,663 | 2/2 | 5.3 / 4.0 |
| Request id on | nginx | 111,407 (109,116-113,698) | 338 | 24,286 | 2/2 | 4.7 / 4.2 |
| Request id on | HAProxy | 94,566 (91,730-97,402) | 495 | 44,320 | 2/2 | 4.8 / 4.3 |
| Request and response header rules | OxiRoute epoll (default) | 168,756 (158,701-178,812) | 324 | 13,210 | 2/2 | 2.3 / 2.2 |
| Request and response header rules | nginx | 128,982 (126,167-131,796) | 436 | 20,590 | 2/2 | 2.9 / 2.8 |
| Request and response header rules | HAProxy | 111,549 (109,278-113,820) | 441 | 39,667 | 2/2 | 3.1 / 1.4 |
| 4 origin servers, round robin | OxiRoute epoll (default) | 184,829 (182,062-187,596) | 238 | 11,690 | 2/2 | 0.4 / 0.6 |
| 4 origin servers, round robin | nginx | 131,421 (130,735-132,107) | 430 | 20,169 | 2/2 | 0.4 / 0.5 |
| 4 origin servers, round robin | HAProxy | 114,340 (114,275-114,405) | 467 | 35,296 | 2/2 | 0.3 / 0.5 |
| Literal 301 redirect, no upstream | OxiRoute epoll (default) | 383,152 (381,188-385,115) | 165 | 5,383 | 2/2 | 0.4 / 0.8 |
| Literal 301 redirect, no upstream | nginx | 301,240 (300,638-301,842) | 124 | 9,956 | 2/2 | 0.3 / 0.7 |
| Literal 301 redirect, no upstream | HAProxy | 259,642 (257,069-262,216) | 237 | 16,318 | 2/2 | 0.5 / 0.7 |
| Basic authentication, bcrypt cost 10, same credentials every request | OxiRoute epoll (default) | 90,646 (90,293-90,999) | 398 | 46,806 | 2/2 | 0.1 / 0.4 |
| Basic authentication, bcrypt cost 10, same credentials every request | nginx | 26 (26-26) | 1,223,695 | 231,737,262 | 2/2 | 0.2 / 0.1 |
| Basic authentication, bcrypt cost 10, same credentials every request | HAProxy | 26 (26-26) | 2,217,885 | 231,751,974 | 2/2 | 0.1 / 0.0 |
| 1,000 exact routes plus a catch-all | OxiRoute epoll (default) | 181,623 (180,077-183,169) | 298 | 11,666 | 2/2 | 0.4 / 0.7 |
| 1,000 exact routes plus a catch-all | nginx | 133,808 (131,894-135,722) | 338 | 20,402 | 2/2 | 0.5 / 0.7 |
| 1,000 exact routes plus a catch-all | HAProxy | 116,588 (115,785-117,392) | 386 | 35,884 | 2/2 | 0.0 / 0.0 |

Basic authentication: nginx and HAProxy verify the bcrypt hash on every request, which dominates both (231 million
instructions per request); OxiRoute does not run bcrypt per request for the same credentials, so the row is not a
comparison of equal work.

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
| Reverse proxy with 7 production features on (forwarded, access log, timeouts, passive health, breaker, outlier, capacity) | 159,847 | 134 | 18,940 | 0.1% |
| Upstream HTTP/2, 1 KiB | 41,725 | 508 | 99,418 | 0.1% |
| Upstream HTTP/2, POST 1 KiB | 31,488 | 791 | 124,572 | 0.1% |
| Forward proxy, HTTP/1.1 absolute-URI request, 1 KiB | 23,459 | 14,940 | 56,931 | 0.02% (req/s spread 65.9%) |
| Forward proxy, `CONNECT` tunnel echo, 1 KiB | 175,043 | 111 | 14,569 | 0.2% |
| Forward proxy, new `CONNECT` tunnel per request | 24,879 | 853 | 66,045 | 0.2% |

The `least_connections` pool is level with `round_robin` and `weighted_round_robin` at 8 servers. The forward-proxy
HTTP lane has stable instruction counts (spread 0.02%) and an unstable rate (spread 65.9%, p99 14.9 ms); the cause
was not isolated. nginx answers `CONNECT` with 405 and no comparator forward-proxy configuration was written for
nginx absolute-URI requests or HAProxy.

### Lanes not measured

RTMP (the harness has no RTMP load client and the nginx build has no RTMP module); an arm64 binary (no arm64 host); the
forward proxy against nginx and HAProxy (see above); the BBR fairness matrix (the tracked results are in the
repository); the `io_uring` column of the TLS and HTTP/2 lanes (the plane does not serve TLS).


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
  attempts; rows that kept fewer than 2 clean runs are not listed, and the first section measures every lane with at
  least 2 clean runs.
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
| 1 KiB | OxiRoute io_uring | 228,206 (226,578-229,835) | 228 | 22,166 | 2/2 | 0.1 / 0.1 |
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
| Keep-alive, 1 KiB | nginx | 101,283 (100,827-101,739) | 504 | 36,629 | 2/2 | 1.6 / 1.1 |

Rows that kept fewer than two clean runs in this pass (OxiRoute keep-alive, HAProxy keep-alive, the explicit
`http_data_plane = "epoll"` lane) are not listed; the first section measures them with at least two clean runs.

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

Idle TLS connections held 15.0 kB (rustls), 32.4 kB (openssl) and 34.8 kB (nginx) of resident memory each (medians of
3, `tls/bench-idle-run.log`). On the epoll plane with a rustls profile (`http_data_plane = "epoll"`) the 1 KiB
keep-alive lane gave 135,217 requests per second with 23,380 instructions per request against 103,608 and 32,525 on
the tokio plane and 97,418 and 36,640 for nginx (`tlsepoll/summary`, one pass on a noisier host).

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

- No run of an arm64 binary, of RTMP, or of forward proxying against nginx and HAProxy. The profile-guided x86_64 binary was measured for the lanes of the first section.
- One worker per proxy except in the scaling tables of the first section (two and four workers); the io_uring
  [short report](../io-uring-http1/README.md) has more two-worker results. None is part of the claims.
- nginx and HAProxy run minimal proxy configurations on the same loopback; their tuning was not explored.
- Two attempts per row in the non-PGO final-tree pass; rows with fewer than two clean runs were dropped. The shipped-binary section has at least two clean runs in every row.
- The io_uring plane matches epoll at 1 KiB keep-alive with 32 connections, is 2% below at 256 and 12% below at
  1,024 connections, costs 4.1k to 5.9k more user instructions per request and is slower at 64 KiB (58.4k against
  87.0k); it stays opt-in. With the shipped binary it is 6% below epoll at 32 connections, 8% at 256, 11% at
  1,024, 16% at 2,048, 35% below at 64 KiB (59.6k against 91.6k) and 39% below at 1 MiB, with 4.1k to 24.7k more user
  instructions per request.
- Compressed-variant cache hit rows come from one invocation each.
- Closed-loop load, loopback, shared host: results show relative behavior of builds on that host, not capacity.
