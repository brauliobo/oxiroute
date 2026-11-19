# OxiRoute 7.1.0 performance report

Summary of the measurements behind the 7.1.0 figures. Each row is copied from the harness output of one invocation; a
table can combine rows from different invocations, and nothing is pooled within a row. The raw run records, traces and
logs are local development material and are not published; this page keeps the medians, ranges and run counts.

The published 7.1.0 release binary was measured on 2026-11-19 across the comparison-harness lanes of the
[7.0 report](../7.0/README.md). A second, shorter pass on the same host and binary added single-run rows for eight
feature lanes, the memory after configuration reloads, the lossy-path HTTP/3 download and four instruction-count harness
lanes; the other instruction-count harness lanes are listed under "Lanes not measured".

## Conditions

- Binary: the published `oxiroute-7.1.0-x86_64-unknown-linux-gnu` release tarball, SHA-256
  `b002f507aba6b4f839c74bb45b317548d4aa3b6f354c2046c642b1e6fae1d651` (extracted `bin/oxiroute`
  `1bb49032beb77fdb43ddae08ae95b4b4a671265b0c1b38c0f5336e813b7bb2b7`). Its build record: commit `f4d62d78`, rustc 1.97.1,
  `pgo: true`, `bolt: false`, `panic: unwind`, C flags `-O3`. Nothing in the proxy was rebuilt. The load generator and
  origin were the `r2tool` of this repository's `benchmarks/r2-data-path/tool`.
- Machine class: 16-core (32 logical CPU) desktop CPU, Linux 7.3.4, shared with unrelated jobs. Proxy on 2 CPUs (8-9),
  origin on 2 (10-11), load generator on 4 (12-15), one proxy worker unless a row says otherwise, loopback, closed loop,
  1 KiB responses and 32 connections unless stated. Four-worker cells use proxy CPUs 8-11, origin 12-13 and load
  generator 14, 15 and 28-31. nginx 1.30.7 and HAProxy 3.4.8 with minimal proxy configurations, linked with OpenSSL 4.1.0
  (the nginx stream module for the TCP relay lanes).
- Runs: 4 s after a 2 s warm-up, repeated (up to 16 rounds) until a cell had two clean runs; memory cells use three
  processes. `clean` is runs kept of attempts. **Clean here means PSI `some` avg10 below 10% before and after the run,
  no request errors and a valid scheduler trace.** Foreign work on the pinned CPUs and on their sibling hardware threads
  was recorded but not used to discard a run (the 7.0 pass discarded runs with more than 1.5 and 8 CPUs of it): the clean
  runs of this pass recorded up to 3.8 CPUs of foreign work and up to 0.7 CPUs on sibling threads. The rows are
  therefore not strictly comparable with the 7.0 rows. The HTTP/3 new-connection lanes tolerate up to 0.1% of
  connections ending in error (largest observed 0.034%, in OxiRoute, its `cubic` listener and nginx alike).
- PSI is `/proc/pressure/cpu` `some` avg10 in percent, the median over the clean runs of the value before and after the
  run. It was 0.0 to 3.2 in single clean runs; the table columns show the medians.
- **Single-run cells.** Four cells of the main pass have exactly one clean run: HTTP/2 over TLS POST 1 KiB (one worker; also the nginx
  HTTP/2 over TCP row of the HTTP/3 POST lane), TCP relay echo 1 KiB and 64 KiB (16 connections), and the two-worker
  256-connection lane (all six columns). Their `clean` column reads `1/1`. Every other cell has at least two clean
  runs, and memory cells three. For the two-worker 256-connection lane the tokio plane had produced request errors in
  all 16 attempts of the first invocation, so the cell was measured once more with one run per column.
- **Quick pass.** The eight feature lanes, the four instruction-count harness lanes, the reload memory and the lossy-path
  download were measured afterwards on the same host, the same binary and the same pinned CPUs (proxy 8-9, origin 10-11,
  load generator 12-15, harness on CPUs 0-7), one run per cell (`--runs 1 --max-rounds 4`, 4 s after a 2 s warm-up),
  so they are single-run cells and not strictly comparable with the 7.0 rows. PSI `some` avg10 before and after every
  run was at most 0.4%, no run had request errors, and the comparison-harness rows have a valid scheduler trace; foreign
  work on the pinned CPUs was recorded for the comparison-harness rows (up to 0.9 CPUs, up to 0.1 CPUs on sibling threads)
  and not used to discard a run. The instruction-count harness rows (`benchmarks/r2-data-path/run.py`, one repetition, 16 connections) are clean
  by PSI and by zero request errors only; they have no scheduler trace.
- `epoll` is the default data plane. `tokio` and `io_uring` are the same binary with `http_data_plane` set in the
  configuration. `instr/req` is user-space instructions per request on the proxy process (`perf stat`,
  `instructions:u`); kernel work is not counted. Latencies are closed loop and carry coordinated omission.
- Harness: `benchmarks/scripts/vs_nginx_haproxy.py`, a local script that is not part of this checkout, run from the tree
  at commit `22122328`. The `benchmarks/r2-data-path` lanes and `reload_memory.py` are checked in and reproduce from the
  repository; only the lanes named in the quick pass above were run.

## 7.1 against 7.0

OxiRoute epoll default, requests per second. The 7.0 column is the median of the shipped 7.0.0 binary from the
[7.0 report](../7.0/README.md); nginx and HAProxy are the 7.1 pass. Different days, different run-acceptance rules: nginx
and HAProxy also moved between the passes (for example nginx -17% and HAProxy -12% at 1,024 connections, nginx -15% and
HAProxy -9% at 2,048), so a difference of a few percent in either direction is not a change of the proxy.

| Lane | OxiRoute 7.1 | OxiRoute 7.0 | 7.1 against 7.0 | nginx 1.30.7 | HAProxy 3.4.8 | OxiRoute 7.1 against nginx |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Keep-alive, 32 connections | 182,364 | 178,396 | +2.2% | 134,242 | 118,436 | 1.36x |
| Keep-alive, 1,024 connections | 170,322 | 166,806 | +2.1% | 76,857 | 69,628 | 2.22x |
| Keep-alive, 2,048 connections | 135,808 | 159,112 | -14.6% | 60,529 | 56,160 | 2.24x |
| 64 KiB response | 92,895 | 91,602 | +1.4% | 35,234 | 37,533 | 2.64x |
| 1 MiB response | 4,666 | 5,300 | -12.0% | 1,876 | 2,875 | 2.49x |
| New connection per request | 91,826 | 91,432 | +0.4% | 65,484 | 61,530 | 1.40x |
| POST 1 KiB | 172,832 | 171,858 | +0.6% | 121,477 | 115,117 | 1.42x |
| Two workers, 64 connections | 324,356 | 177,640 | +82.6% | 262,650 | 212,406 | 1.23x |
| Two workers, 256 connections (one run) | 350,108 | 183,176 | +91.1% | 265,540 | 221,293 | 1.32x |
| Four workers, 256 connections | 338,358 | 266,735 | +26.9% | 364,879 | 370,925 | 0.93x |
| TLS 1.3 keep-alive, 1 KiB | 127,919 | 128,113 | -0.2% | 94,787 | 84,884 | 1.35x |
| TLS 1.3 full handshake | 14,960 | 16,092 | -7.0% | 4,939 | 4,394 | 3.03x |
| TLS 1.2 resumed (ticket) | 53,804 | 57,527 | -6.5% | 11,736 | 8,856 | 4.58x |
| HTTP/2 over TLS, 1 KiB | 130,661 | 132,781 | -1.6% | 98,260 | 80,351 | 1.33x |
| Upstream HTTP/2, 1 KiB | 38,719 | 40,125 | -3.5% | 81,740 | 81,526 | 0.47x |
| 1 KiB cache hit | 270,691 | 270,224 | +0.2% | 126,682 | 183,402 | 2.14x |
| 64 KiB gzip, stored coding, hit | 132,603 | 129,376 | +2.5% | 43,540 | 25,449 | 3.05x |
| HTTP/3 proxy, 1 KiB (nginx: HTTP/3) | 65,808 | 70,169 | -6.2% | 62,538 | n/a | 1.05x |
| HTTP/3 direct 1 KiB (nginx: HTTP/3) | 174,649 | 189,926 | -8.0% | 117,212 | n/a | 1.49x |
| Static file 1 KiB | 318,252 | 343,474 | -7.3% | 75,598 | n/a | 4.21x |
| WebSocket echo, 1 KiB, 16 connections | 174,132 | 174,002 | +0.1% | 220,462 | 179,158 | 0.79x |
| Pool `weighted_round_robin`, 8 servers | 179,268 | 183,626 | -2.4% | 126,428 | 107,705 | 1.42x |

User-space instructions per request are within 5% of the 7.0 figures on every lane that both passes measured, except
TLS 1.2 resumed (ticket), 7% higher (109,704 to 117,657), and the single-run TCP relay echo of 64 KiB, 11% lower
(6,744 to 5,978).

### Where OxiRoute is ahead of its 7.0 figures

The single-run feature lanes with one worker (access log, request id, header rules, 1,000 routes, 4 origin servers,
literal redirect, 2 KiB request headers) put OxiRoute 1.3 to 1.9 times above nginx and 1.3 to 2.2 times above HAProxy
in requests per second. Basic authentication with bcrypt is 88,168 against 26 for both comparators, which verify the hash on every request.

- Two and four workers with the default scheduling. In 7.0 the default epoll scheduling used 0.99 to 1.95 CPUs of two
  at two workers and its rate was close to the one-worker rate; in this pass it used 1.84 to 1.97 CPUs and reached
  316,118 to 350,108 requests per second at 64 to 1,024 connections (7.0: 177,640 to 285,210). At four workers it
  reached 271,710 to 338,358 (7.0: 239,054 to 272,122) and the run-to-run range narrowed from as much as 1.9x to
  about 10%. The bimodal behaviour recorded for 7.0 (a fresh process spread connections over both workers, later runs from the
  same process kept them on one) did not appear in these runs. Whether code, run order or the host explains it was not
  isolated. The default is now level with or ahead of `aged_per_worker` at two workers (for example 324,356 against
  303,178 at 64 connections) and equal to it at four.
- Plain HTTP/1.1 at 32 and 1,024 connections, POST 1 KiB, 64 KiB responses and 1 KiB cache hits are within 3% of 7.0
  and remain ahead of nginx and HAProxy.

### Where OxiRoute is not ahead

- Four workers: the default is 0.3% to 7% below nginx in all six four-worker cells (336,284 against 343,714 at 64
  connections, 338,358 against 364,879 at 256, 292,146 against 305,694 at 1,024) and 0.2% to 9% below HAProxy in five
  of them (338,358 against 370,925 at 256 connections); at 2,048 connections it is 13% above HAProxy (271,710 against
  240,320).
- HTTP/2 upstream: with an HTTP/2 origin OxiRoute serves 38,719 requests per second (98,188 user instructions per
  request) against 81,740 for nginx (40,050) and 81,526 for HAProxy (48,654), on every data plane; POST 1 KiB 29,766
  against 72,552 and 81,580.
- WebSocket echo: nginx 220,462 against OxiRoute 174,132 and HAProxy 179,158. TCP relay echo of 1 KiB (one run): nginx
  219,786 against 203,425 (HAProxy 184,368). Compression without a cache at 64 KiB: nginx 34,090 against 30,464 (the
  `io_uring` plane reached 32,676; HAProxy 21,773). The static 1 MiB file is level with nginx (1,516 against 1,546).
- HTTP/3: through a proxy nginx HTTP/2 over TCP serves 98,260 requests per second, more than either HTTP/3 stack
  (OxiRoute 65,808, nginx HTTP/3 62,538); for new connections nginx HTTP/2 serves 5,192 against OxiRoute `bbr` 4,908.
  `cubic` is 11% below `bbr` on POST (55,711 against 62,298) with a higher p99 (1,790 against 831 µs); the two were level
  in 7.0.
- The tokio plane at 1,024 and 2,048 connections has p99 latencies of 154,772 µs and 339,596 µs (epoll: 16,724 and
  29,842). At two workers it produces request errors: 10 of 12 attempts at 64 connections were discarded, and all 16
  attempts of the first 256-connection invocation.
- `aged_per_worker` is lower than the default in every TLS 1.3 keep-alive and HTTP/2 over TLS many-connection lane (TLS
  1.3 keep-alive 77,329 against 107,192 at 1,024 connections), with a lower p99 in all of them (44.8 ms against 160 ms
  there), and in the one-worker lane with 1 in 500 origin responses delayed it is lower in rate and higher in p99 (146,542 and 12.4 ms against
  167,645 and 4.7 ms).
- Memory: idle PSS is 11.5 MB against nginx 11.0 MB; in the 7.0 pass it was 9.9 against 10.6. With 1,024 connections
  held OxiRoute is 13.8 MB against nginx 20.4 MB and HAProxy 41.8 MB.
- Memory after reloads (one run): with 1,001 routes OxiRoute holds 33.8 MB after 50 reloads against nginx 21.3 MB and
  HAProxy 25.0 MB; its idle figure for that configuration is 33.1 MB against 12.8 and 27.0 MB (the memory table above
  comes from another harness, so the idle figures differ).
- Lossy path (one run): nginx HTTP/2 over TCP downloads 10 MiB in 2.664 s against 0.357 s for OxiRoute `bbr`; OxiRoute
  `cubic` took 13.112 s and nginx HTTP/3 30.967 s.
- The `io_uring` plane is below epoll at 32 connections (-9%), 1,024 (-20%), 2,048 (-9%), 64 KiB (-41%) and 1 MiB
  (-35%), and stays opt-in.

### Regressions against 7.0

Lanes where OxiRoute fell by more than the comparators did in the same lane. None was investigated for a cause.

- Two workers, 2,048 connections: 253,178 against 308,658 (-18%); nginx -3%, HAProxy -9%.
- POST 64 KiB: 43,645 against 47,063 (-7%) while nginx rose 12% (30,250).
- Compression without a cache at 64 KiB: 30,464 against 33,202 (-8%); nginx -3%.
- TLS 1.3 full handshake: 14,960 against 16,092 (-7%); nginx -4%, HAProxy -6%. TLS 1.2 resumed (ticket): 53,804 against
  57,527 (-6.5%) with 7% more user instructions per request; nginx and HAProxy unchanged.
- HTTP/3 `cubic` POST: 55,711 against 64,849 (-14%); `bbr` -3%.
- Idle PSS 11.5 MB against 9.9 MB in the 7.0 pass of the same harness (+16%), 13.8 against 12.2 with 1,024 connections
  held; idle H1 connections over TLS use 18,326 bytes each against 16,749.

Other lanes that fell by 6% to 20% (keep-alive at 2,048 connections, 1 MiB, TLS keep-alive and HTTP/2 over TLS at many
connections, static 1 KiB, HTTP/3 proxy, direct and new connections) fell by a comparable amount for nginx in the same
lane (within 8 points) and are not counted as regressions of the proxy.

## Tables

### HTTP/1.1 keep-alive, 1 KiB response, by connection count

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 32 | OxiRoute epoll (default) | 182,364 (177,516-187,212) | 265 | 11,737 | 2/2 | 0.4 / 1.1 |
| 32 | OxiRoute tokio plane | 141,597 (136,278-146,916) | 358 | 18,720 | 2/2 | 0.4 / 1.0 |
| 32 | OxiRoute io_uring | 165,783 (165,369-166,197) | 352 | 15,802 | 2/2 | 0.4 / 1.0 |
| 32 | nginx | 134,242 (132,958-135,527) | 402 | 19,932 | 2/2 | 0.6 / 1.1 |
| 32 | HAProxy | 118,436 (117,985-118,888) | 388 | 35,446 | 2/2 | 0.9 / 1.1 |
| 256 | OxiRoute epoll (default) | 175,485 (174,392-176,578) | 2,855 | 11,752 | 2/2 | 1.0 / 1.0 |
| 256 | OxiRoute tokio plane | 135,568 (130,335-140,801) | 3,640 | 18,704 | 2/2 | 0.7 / 1.1 |
| 256 | OxiRoute io_uring | 172,429 (171,779-173,079) | 1,946 | 16,212 | 2/2 | 0.5 / 0.9 |
| 256 | nginx | 129,207 (125,729-132,685) | 3,657 | 20,228 | 2/2 | 0.3 / 1.2 |
| 256 | HAProxy | 103,392 (101,686-105,098) | 6,345 | 40,198 | 2/2 | 0.8 / 1.1 |
| 1,024 | OxiRoute epoll (default) | 170,322 (169,615-171,029) | 16,724 | 11,746 | 2/2 | 0.8 / 1.0 |
| 1,024 | OxiRoute tokio plane | 124,590 (124,480-124,699) | 154,772 | 18,722 | 2/2 | 0.8 / 1.3 |
| 1,024 | OxiRoute io_uring | 136,781 (136,743-136,819) | 24,369 | 16,464 | 2/2 | 1.2 / 1.5 |
| 1,024 | nginx | 76,857 (73,751-79,963) | 38,066 | 20,446 | 2/2 | 0.8 / 0.9 |
| 1,024 | HAProxy | 69,628 (66,327-72,928) | 33,422 | 39,774 | 2/2 | 0.7 / 0.8 |
| 2,048 | OxiRoute epoll (default) | 135,808 (135,473-136,144) | 29,842 | 11,742 | 2/2 | 0.6 / 0.9 |
| 2,048 | OxiRoute tokio plane | 126,411 (124,029-128,793) | 339,596 | 18,716 | 2/2 | 1.1 / 0.9 |
| 2,048 | OxiRoute io_uring | 123,889 (123,351-124,427) | 31,108 | 16,524 | 2/2 | 0.5 / 0.9 |
| 2,048 | nginx | 60,529 (59,250-61,808) | 66,409 | 20,431 | 2/2 | 0.6 / 0.9 |
| 2,048 | HAProxy | 56,160 (53,985-58,335) | 87,526 | 40,064 | 2/2 | 0.8 / 0.9 |

CPU time per request (scheduler-accounted, 32 connections): epoll 5.4 µs, tokio 6.9, io_uring 5.8, nginx 7.3, HAProxy
8.4. At 1,024 connections: epoll 5.8, tokio 8.0, io_uring 7.3, nginx 13.0, HAProxy 14.3.

### Bodies, new connection per request, and POST

32 connections. The epoll plane splices large response bodies from the origin socket to the client socket. The new
connection lane sends `Connection: close` on the proxied exchange. POST sends the body to an origin sink.

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 64 KiB | OxiRoute epoll (default) | 92,895 (92,150-93,640) | 664 | 12,199 | 2/2 | 1.0 / 1.7 |
| 64 KiB | OxiRoute tokio plane | 52,540 (51,032-54,049) | 1,265 | 20,594 | 2/2 | 0.8 / 1.1 |
| 64 KiB | OxiRoute io_uring | 54,644 (53,191-56,097) | 1,572 | 18,104 | 2/2 | 1.2 / 1.5 |
| 64 KiB | nginx | 35,234 (34,477-35,990) | 1,636 | 37,890 | 2/2 | 0.9 / 1.0 |
| 64 KiB | HAProxy | 37,533 (36,206-38,860) | 1,618 | 49,148 | 2/2 | 0.9 / 1.4 |
| 1 MiB | OxiRoute epoll (default) | 4,666 (4,637-4,696) | 18,222 | 27,671 | 2/2 | 1.1 / 1.5 |
| 1 MiB | OxiRoute tokio plane | 3,683 (3,661-3,705) | 23,692 | 29,292 | 2/2 | 1.2 / 1.9 |
| 1 MiB | OxiRoute io_uring | 3,034 (3,031-3,038) | 15,051 | 51,908 | 2/2 | 1.4 / 1.9 |
| 1 MiB | nginx | 1,876 (1,736-2,015) | 31,987 | 262,485 | 2/2 | 1.3 / 1.7 |
| 1 MiB | HAProxy | 2,875 (2,819-2,931) | 22,445 | 164,314 | 2/2 | 1.2 / 1.6 |
| New connection | OxiRoute epoll (default) | 91,826 (89,183-94,470) | 676 | 13,133 | 2/2 | 0.6 / 1.1 |
| New connection | OxiRoute tokio plane | 76,158 (74,744-77,573) | 868 | 23,174 | 2/2 | 0.8 / 1.3 |
| New connection | OxiRoute io_uring | 86,100 (83,523-88,678) | 734 | 18,275 | 2/2 | 1.0 / 1.5 |
| New connection | nginx | 65,484 (65,138-65,830) | 868 | 21,482 | 2/2 | 0.8 / 1.1 |
| New connection | HAProxy | 61,530 (59,151-63,910) | 875 | 38,530 | 2/2 | 0.8 / 1.0 |
| POST 1 KiB | OxiRoute epoll (default) | 172,832 (171,449-174,215) | 336 | 12,177 | 2/2 | 0.7 / 1.0 |
| POST 1 KiB | OxiRoute tokio plane | 137,322 (135,464-139,179) | 448 | 19,891 | 2/2 | 0.6 / 1.3 |
| POST 1 KiB | OxiRoute io_uring | 154,714 (150,221-159,206) | 373 | 16,483 | 2/2 | 0.7 / 1.2 |
| POST 1 KiB | nginx | 121,477 (121,273-121,681) | 506 | 23,124 | 2/2 | 0.8 / 1.5 |
| POST 1 KiB | HAProxy | 115,117 (115,065-115,169) | 468 | 37,370 | 2/2 | 0.7 / 0.8 |
| POST 64 KiB | OxiRoute epoll (default) | 43,645 (43,025-44,265) | 1,457 | 13,267 | 2/2 | 0.9 / 1.2 |
| POST 64 KiB | OxiRoute tokio plane | 36,902 (36,056-37,748) | 1,815 | 34,690 | 2/2 | 1.2 / 1.4 |
| POST 64 KiB | OxiRoute io_uring | 36,699 (34,769-38,629) | 2,114 | 37,356 | 2/2 | 1.3 / 1.6 |
| POST 64 KiB | nginx | 30,250 (29,200-31,301) | 1,886 | 30,688 | 2/2 | 1.1 / 1.2 |
| POST 64 KiB | HAProxy | 30,554 (29,963-31,145) | 2,067 | 52,236 | 2/2 | 0.7 / 1.1 |

### Cache hits and compression

1 KiB and 64 KiB bodies served from the in-memory cache, 32 connections; the compressed rows serve a stored 64 KiB text
body as gzip, zstd or brotli level 1 (the cache `compression` policy), or compress it on every request with no cache.
The cache hit runs on the fast lane, so the three planes differ only by noise. nginx and HAProxy have no stored-coding
cache and no brotli or zstd encoder in the measured builds, so those lanes are OxiRoute only.

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 1 KiB hit | OxiRoute epoll (default) | 270,691 (265,776-275,606) | 218 | 18,498 | 2/2 | 0.3 / 1.0 |
| 1 KiB hit | OxiRoute tokio plane | 270,568 (264,672-276,465) | 212 | 18,500 | 2/2 | 1.0 / 1.3 |
| 1 KiB hit | OxiRoute io_uring | 271,036 (270,007-272,065) | 196 | 18,493 | 2/2 | 0.7 / 1.3 |
| 1 KiB hit | nginx `proxy_cache` | 126,682 (123,967-129,397) | 439 | 19,736 | 2/2 | 0.8 / 0.7 |
| 1 KiB hit | HAProxy cache | 183,402 (177,015-189,790) | 315 | 31,526 | 2/2 | 0.6 / 0.6 |
| 64 KiB hit | OxiRoute epoll (default) | 101,339 (98,644-104,034) | 824 | 18,324 | 2/2 | 0.9 / 2.1 |
| 64 KiB hit | OxiRoute tokio plane | 99,514 (98,713-100,315) | 877 | 18,378 | 2/2 | 0.5 / 2.5 |
| 64 KiB hit | OxiRoute io_uring | 99,528 (96,430-102,626) | 849 | 18,329 | 2/2 | 1.2 / 2.6 |
| 64 KiB hit | nginx `proxy_cache` | 94,573 (92,177-96,969) | 831 | 18,210 | 2/2 | 1.7 / 2.5 |
| 64 KiB hit | HAProxy cache | 55,102 (54,151-56,052) | 1,294 | 63,902 | 2/2 | 1.3 / 1.2 |
| 64 KiB gzip level 1, stored coding, hit | OxiRoute epoll (default) | 132,603 (129,642-135,564) | 458 | 26,905 | 2/2 | 1.3 / 1.1 |
| 64 KiB gzip level 1, stored coding, hit | OxiRoute tokio plane | 135,128 (134,143-136,113) | 425 | 26,919 | 2/2 | 0.8 / 0.8 |
| 64 KiB gzip level 1, stored coding, hit | OxiRoute io_uring | 130,332 (129,022-131,643) | 434 | 26,887 | 2/2 | 0.9 / 1.0 |
| 64 KiB gzip level 1, stored coding, hit | nginx, compresses per request | 43,540 (41,244-45,836) | 1,122 | 145,776 | 2/2 | 0.8 / 0.7 |
| 64 KiB gzip level 1, stored coding, hit | HAProxy, compresses per request | 25,449 (24,913-25,985) | 3,246 | 255,524 | 2/2 | 0.9 / 1.2 |
| 64 KiB zstd level 1, stored coding, hit | OxiRoute epoll (default) | 162,700 (159,865-165,534) | 574 | 26,620 | 2/2 | 0.8 / 1.6 |
| 64 KiB zstd level 1, stored coding, hit | OxiRoute tokio plane | 199,708 (199,495-199,922) | 384 | 26,212 | 2/2 | 1.1 / 0.8 |
| 64 KiB zstd level 1, stored coding, hit | OxiRoute io_uring | 184,269 (177,392-191,146) | 466 | 26,413 | 2/3 | 1.1 / 1.1 |
| 64 KiB brotli level 1, stored coding, hit | OxiRoute epoll (default) | 127,424 (125,659-129,189) | 494 | 26,972 | 2/2 | 1.3 / 1.5 |
| 64 KiB brotli level 1, stored coding, hit | OxiRoute tokio plane | 133,634 (129,222-138,045) | 460 | 27,096 | 2/2 | 1.1 / 0.8 |
| 64 KiB brotli level 1, stored coding, hit | OxiRoute io_uring | 122,526 (120,567-124,485) | 524 | 26,986 | 2/2 | 0.6 / 1.0 |
| 64 KiB gzip level 1, no cache | OxiRoute epoll (default) | 30,464 (29,878-31,050) | 1,914 | 167,796 | 2/2 | 1.2 / 1.6 |
| 64 KiB gzip level 1, no cache | OxiRoute tokio plane | 30,924 (30,588-31,261) | 2,176 | 167,794 | 2/2 | 1.8 / 1.2 |
| 64 KiB gzip level 1, no cache | OxiRoute io_uring | 32,676 (32,359-32,993) | 1,411 | 167,790 | 2/2 | 0.9 / 0.8 |
| 64 KiB gzip level 1, no cache | nginx | 34,090 (34,024-34,156) | 1,924 | 169,154 | 2/2 | 0.9 / 1.1 |
| 64 KiB gzip level 1, no cache | HAProxy | 21,773 (20,422-23,124) | 3,787 | 216,606 | 2/2 | 0.7 / 1.1 |

The 64 KiB hit is 7% above nginx (101,339 against 94,573) with ranges that do not overlap but are 3% wide each: treat it
as level. HAProxy's cache serves the 64 KiB body at 55,102 requests per second with 63,902 instructions per request.

### TLS and HTTP/2

32 connections, one worker, ECDSA P-256 certificate unless a lane says RSA-2048, rustls (aws-lc provider) default
backend. HTTP/2 over TLS: 8 connections of 4 streams. `io_uring` does not serve TLS: with `http_data_plane =
"io_uring"` the TLS and HTTP/2 listeners run the tokio path, so there is no separate `io_uring` column for them.

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| TLS 1.3 keep-alive, 1 KiB | OxiRoute default (tokio path) | 127,919 (127,597-128,241) | 449 | 25,722 | 2/2 | 0.2 / 0.5 |
| TLS 1.3 keep-alive, 1 KiB | OxiRoute, `http_data_plane = "tokio"` | 123,312 (120,481-126,143) | 426 | 25,720 | 2/2 | 1.1 / 0.8 |
| TLS 1.3 keep-alive, 1 KiB | OxiRoute, `http_data_plane = "epoll"` | 153,674 (148,421-158,928) | 388 | 19,288 | 2/2 | 0.6 / 0.8 |
| TLS 1.3 keep-alive, 1 KiB | nginx | 94,787 (92,640-96,934) | 524 | 36,578 | 2/2 | 0.6 / 0.7 |
| TLS 1.3 keep-alive, 1 KiB | HAProxy | 84,884 (83,087-86,681) | 712 | 50,230 | 2/2 | 0.7 / 0.6 |
| TLS 1.2 keep-alive, 1 KiB | OxiRoute default (tokio path) | 125,382 (120,974-129,791) | 474 | 25,922 | 2/2 | 0.8 / 1.1 |
| TLS 1.2 keep-alive, 1 KiB | OxiRoute, `http_data_plane = "tokio"` | 130,119 (128,742-131,496) | 392 | 25,920 | 2/2 | 0.7 / 1.1 |
| TLS 1.2 keep-alive, 1 KiB | nginx | 98,830 (97,302-100,357) | 573 | 36,299 | 2/2 | 0.9 / 1.0 |
| TLS 1.2 keep-alive, 1 KiB | HAProxy | 87,748 (86,751-88,744) | 626 | 49,786 | 2/2 | 0.8 / 0.8 |
| TLS 1.3 full handshake | OxiRoute default (tokio path) | 14,960 (14,876-15,044) | 6,352 | 648,204 | 2/2 | 1.1 / 1.7 |
| TLS 1.3 full handshake | OxiRoute, `http_data_plane = "tokio"` | 15,256 (14,828-15,683) | 4,294 | 648,102 | 2/2 | 1.0 / 1.4 |
| TLS 1.3 full handshake | nginx | 4,939 (4,862-5,016) | 13,716 | 1,559,678 | 2/2 | 1.2 / 1.7 |
| TLS 1.3 full handshake | HAProxy | 4,394 (4,277-4,511) | 9,062 | 1,610,762 | 2/2 | 1.6 / 1.4 |
| TLS 1.2 ECDHE full handshake | OxiRoute default (tokio path) | 17,268 (16,645-17,892) | 4,132 | 594,284 | 2/2 | 1.4 / 1.6 |
| TLS 1.2 ECDHE full handshake | OxiRoute, `http_data_plane = "tokio"` | 17,384 (16,929-17,840) | 3,430 | 594,215 | 2/2 | 1.7 / 1.6 |
| TLS 1.2 ECDHE full handshake | nginx | 5,178 (5,168-5,188) | 12,710 | 1,390,518 | 2/2 | 1.5 / 1.4 |
| TLS 1.2 ECDHE full handshake | HAProxy | 4,832 (4,828-4,837) | 9,653 | 1,447,792 | 2/2 | 1.3 / 1.2 |
| TLS 1.3 resumed (ticket) | OxiRoute default (tokio path) | 16,290 (16,264-16,316) | 4,736 | 542,834 | 2/2 | 0.5 / 1.2 |
| TLS 1.3 resumed (ticket) | OxiRoute, `http_data_plane = "tokio"` | 15,513 (15,357-15,669) | 6,226 | 543,198 | 2/2 | 0.7 / 1.4 |
| TLS 1.3 resumed (ticket) | nginx | 5,325 (5,191-5,459) | 8,119 | 1,480,426 | 2/2 | 0.8 / 0.6 |
| TLS 1.3 resumed (ticket) | HAProxy | 4,786 (4,778-4,795) | 7,704 | 1,521,045 | 2/2 | 0.7 / 0.7 |
| TLS 1.2 resumed (ticket) | OxiRoute default (tokio path) | 53,804 (53,279-54,330) | 1,130 | 117,657 | 2/2 | 0.8 / 1.2 |
| TLS 1.2 resumed (ticket) | OxiRoute, `http_data_plane = "tokio"` | 54,232 (53,195-55,270) | 1,172 | 117,260 | 2/2 | 0.6 / 1.4 |
| TLS 1.2 resumed (ticket) | nginx | 11,736 (11,412-12,061) | 4,733 | 272,019 | 2/2 | 0.8 / 0.8 |
| TLS 1.2 resumed (ticket) | HAProxy | 8,856 (8,274-9,437) | 7,010 | 307,864 | 2/2 | 0.7 / 0.9 |
| TLS 1.2 resumed (session id) | OxiRoute default (tokio path) | 54,438 (53,318-55,557) | 1,382 | 93,655 | 2/2 | 0.8 / 1.6 |
| TLS 1.2 resumed (session id) | OxiRoute, `http_data_plane = "tokio"` | 55,112 (53,092-57,131) | 1,578 | 93,777 | 2/2 | 1.1 / 1.6 |
| TLS 1.2 resumed (session id) | nginx | 12,102 (12,073-12,131) | 4,406 | 261,247 | 2/2 | 1.1 / 0.9 |
| TLS 1.2 resumed (session id) | HAProxy | 9,095 (8,756-9,434) | 5,718 | 295,914 | 2/2 | 0.9 / 0.9 |
| TLS 1.3 full handshake, RSA-2048 certificate | OxiRoute default (tokio path) | 1,934 (1,932-1,935) | 31,693 | 6,785,492 | 2/2 | 0.7 / 0.6 |
| TLS 1.3 full handshake, RSA-2048 certificate | OxiRoute, `http_data_plane = "tokio"` | 1,878 (1,837-1,919) | 35,594 | 6,785,878 | 2/2 | 0.7 / 0.9 |
| TLS 1.3 full handshake, RSA-2048 certificate | nginx | 1,612 (1,612-1,612) | 22,068 | 7,692,057 | 2/2 | 0.9 / 0.5 |
| TLS 1.3 full handshake, RSA-2048 certificate | HAProxy | 1,538 (1,519-1,558) | 29,684 | 7,740,086 | 2/2 | 0.6 / 0.4 |
| TLS 1.3 full handshake with a client certificate | OxiRoute default (tokio path) | 5,286 (5,269-5,304) | 13,155 | 2,092,810 | 2/2 | 0.5 / 0.8 |
| TLS 1.3 full handshake with a client certificate | OxiRoute, `http_data_plane = "tokio"` | 5,246 (5,231-5,261) | 13,326 | 2,092,866 | 2/2 | 0.6 / 0.8 |
| TLS 1.3 full handshake with a client certificate | nginx | 2,820 (2,794-2,847) | 16,261 | 3,229,053 | 2/2 | 0.8 / 0.7 |
| TLS 1.3 full handshake with a client certificate | HAProxy | 2,626 (2,617-2,634) | 20,946 | 3,295,514 | 2/2 | 0.7 / 0.5 |

The default HTTPS listener runs on the tokio path (25.7k instructions per request); with `http_data_plane = "epoll"` and
a rustls profile the same lane is 20% faster and 25% cheaper in user instructions.

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| HTTP/2 over TLS, 1 KiB | OxiRoute default (tokio path) | 130,661 (128,520-132,802) | 486 | 31,642 | 2/2 | 0.6 / 1.1 |
| HTTP/2 over TLS, 1 KiB | OxiRoute, `http_data_plane = "tokio"` | 134,110 (131,534-136,687) | 392 | 31,596 | 2/2 | 0.6 / 1.1 |
| HTTP/2 over TLS, 1 KiB | nginx | 98,260 (97,871-98,650) | 608 | 33,335 | 2/2 | 0.6 / 0.8 |
| HTTP/2 over TLS, 1 KiB | HAProxy | 80,351 (78,528-82,174) | 906 | 45,934 | 2/2 | 0.8 / 1.4 |
| HTTP/2 over TLS, 64 KiB | OxiRoute default (tokio path) | 28,646 (26,576-30,716) | 2,608 | 206,124 | 2/2 | 1.3 / 1.3 |
| HTTP/2 over TLS, 64 KiB | OxiRoute, `http_data_plane = "tokio"` | 28,834 (28,159-29,509) | 2,326 | 206,102 | 2/2 | 0.6 / 0.8 |
| HTTP/2 over TLS, 64 KiB | nginx | 17,723 (17,653-17,793) | 4,072 | 254,455 | 2/2 | 0.7 / 1.2 |
| HTTP/2 over TLS, 64 KiB | HAProxy | 20,327 (19,568-21,086) | 3,013 | 234,940 | 2/2 | 0.4 / 0.9 |
| HTTP/2 over TLS, POST 1 KiB | OxiRoute default (tokio path) | 113,983 (113,983-113,983) | 490 | 37,429 | 1/1 | 0.1 / 1.5 |
| HTTP/2 over TLS, POST 1 KiB | OxiRoute, `http_data_plane = "tokio"` | 110,957 (110,957-110,957) | 586 | 37,521 | 1/1 | 0.6 / 0.9 |
| HTTP/2 over TLS, POST 1 KiB | nginx | 67,226 (67,226-67,226) | 1,116 | 40,892 | 1/1 | 0.6 / 1.4 |
| HTTP/2 over TLS, POST 1 KiB | HAProxy | 72,437 (72,437-72,437) | 666 | 55,559 | 1/1 | 0.7 / 1.1 |
| Upstream HTTP/2, 1 KiB (HTTP/1.1 front end) | OxiRoute epoll (default) | 38,719 (37,598-39,840) | 1,602 | 98,188 | 2/2 | 1.4 / 1.2 |
| Upstream HTTP/2, 1 KiB (HTTP/1.1 front end) | OxiRoute tokio plane | 37,028 (36,825-37,231) | 1,720 | 98,252 | 2/2 | 1.5 / 1.4 |
| Upstream HTTP/2, 1 KiB (HTTP/1.1 front end) | OxiRoute io_uring | 37,230 (36,090-38,371) | 1,676 | 98,338 | 2/2 | 1.2 / 1.4 |
| Upstream HTTP/2, 1 KiB (HTTP/1.1 front end) | nginx | 81,740 (80,350-83,129) | 823 | 40,050 | 2/2 | 1.7 / 1.8 |
| Upstream HTTP/2, 1 KiB (HTTP/1.1 front end) | HAProxy | 81,526 (79,081-83,972) | 690 | 48,654 | 2/2 | 1.6 / 2.5 |
| Upstream HTTP/2, POST 1 KiB (HTTP/1.1 front end) | OxiRoute epoll (default) | 29,766 (28,401-31,130) | 2,120 | 124,480 | 2/2 | 2.5 / 2.0 |
| Upstream HTTP/2, POST 1 KiB (HTTP/1.1 front end) | OxiRoute tokio plane | 31,867 (31,352-32,382) | 1,763 | 123,458 | 2/2 | 1.3 / 0.9 |
| Upstream HTTP/2, POST 1 KiB (HTTP/1.1 front end) | OxiRoute io_uring | 30,134 (29,530-30,739) | 1,924 | 124,298 | 2/2 | 1.3 / 1.8 |
| Upstream HTTP/2, POST 1 KiB (HTTP/1.1 front end) | nginx | 72,552 (70,512-74,593) | 878 | 44,752 | 2/2 | 1.7 / 2.4 |
| Upstream HTTP/2, POST 1 KiB (HTTP/1.1 front end) | HAProxy | 81,580 (79,668-83,492) | 791 | 48,676 | 2/2 | 2.5 / 1.5 |

### HTTPS and HTTP/2 over TLS with many connections

One worker, `aged_per_worker` is the opt-in `connection_scheduling` mode; HTTP/2 rows use 4 streams per connection.

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | proxy CPUs used | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| TLS 1.3 keep-alive, 512 connections | OxiRoute epoll (default) | 108,326 (107,061-109,590) | 67,306 | 25,726 | 2/2 | 0.97 | 0.9 / 1.5 |
| TLS 1.3 keep-alive, 512 connections | OxiRoute epoll, aged | 101,444 (100,591-102,297) | 16,156 | 26,061 | 2/2 | 0.99 | 1.5 / 0.9 |
| TLS 1.3 keep-alive, 512 connections | nginx | 73,604 (70,218-76,991) | 25,256 | 36,990 | 2/2 | 0.99 | 1.0 / 0.8 |
| TLS 1.3 keep-alive, 512 connections | HAProxy | 60,712 (56,089-65,336) | 33,126 | 54,032 | 2/2 | 0.99 | 0.9 / 0.9 |
| TLS 1.3 keep-alive, 1,024 connections | OxiRoute epoll (default) | 107,192 (103,441-110,942) | 160,426 | 25,720 | 2/2 | 0.99 | 1.1 / 1.4 |
| TLS 1.3 keep-alive, 1,024 connections | OxiRoute epoll, aged | 77,329 (76,493-78,165) | 44,846 | 26,110 | 2/2 | 0.99 | 0.8 / 1.2 |
| TLS 1.3 keep-alive, 1,024 connections | nginx | 55,849 (53,864-57,834) | 40,466 | 37,208 | 2/2 | 0.99 | 0.8 / 1.2 |
| TLS 1.3 keep-alive, 1,024 connections | HAProxy | 50,045 (48,911-51,179) | 40,574 | 54,364 | 2/2 | 0.99 | 1.0 / 0.9 |
| TLS 1.3 keep-alive, 2,048 connections | OxiRoute epoll (default) | 94,124 (91,271-96,977) | 345,364 | 25,771 | 2/2 | 0.97 | 1.7 / 2.1 |
| TLS 1.3 keep-alive, 2,048 connections | OxiRoute epoll, aged | 69,512 (68,694-70,330) | 108,131 | 26,258 | 2/2 | 0.99 | 1.7 / 1.6 |
| TLS 1.3 keep-alive, 2,048 connections | nginx | 39,107 (37,864-40,350) | 102,342 | 37,154 | 2/2 | 0.99 | 1.7 / 1.5 |
| TLS 1.3 keep-alive, 2,048 connections | HAProxy | 37,063 (34,958-39,168) | 158,692 | 54,648 | 2/2 | 0.99 | 1.6 / 1.7 |
| HTTP/2 over TLS, 128 connections of 4 streams | OxiRoute epoll (default) | 106,750 (106,324-107,177) | 40,462 | 32,226 | 2/2 | 0.96 | 1.4 / 2.0 |
| HTTP/2 over TLS, 128 connections of 4 streams | OxiRoute epoll, aged | 91,974 (79,638-104,311) | 22,778 | 33,118 | 2/2 | 0.97 | 1.4 / 2.2 |
| HTTP/2 over TLS, 128 connections of 4 streams | nginx | 81,130 (80,025-82,234) | 15,952 | 33,746 | 2/2 | 0.96 | 1.6 / 2.1 |
| HTTP/2 over TLS, 128 connections of 4 streams | HAProxy | 72,582 (69,207-75,957) | 12,623 | 49,914 | 2/2 | 0.99 | 1.2 / 1.2 |
| HTTP/2 over TLS, 256 connections of 4 streams | OxiRoute epoll (default) | 94,526 (90,090-98,963) | 118,672 | 32,430 | 2/2 | 0.98 | 1.8 / 2.5 |
| HTTP/2 over TLS, 256 connections of 4 streams | OxiRoute epoll, aged | 86,672 (86,458-86,885) | 29,558 | 32,973 | 2/2 | 0.99 | 1.2 / 1.7 |
| HTTP/2 over TLS, 256 connections of 4 streams | nginx | 58,952 (57,042-60,863) | 37,791 | 33,932 | 2/2 | 0.99 | 1.2 / 1.1 |
| HTTP/2 over TLS, 256 connections of 4 streams | HAProxy | 55,558 (52,499-58,618) | 41,562 | 50,214 | 2/2 | 0.99 | 1.1 / 1.8 |
| HTTP/2 over TLS, 512 connections of 4 streams | OxiRoute epoll (default) | 101,104 (99,857-102,352) | 235,542 | 32,422 | 2/2 | 0.98 | 1.0 / 1.9 |
| HTTP/2 over TLS, 512 connections of 4 streams | OxiRoute epoll, aged | 70,369 (68,614-72,124) | 81,505 | 32,772 | 2/2 | 0.99 | 1.5 / 1.8 |
| HTTP/2 over TLS, 512 connections of 4 streams | nginx | 51,606 (49,834-53,378) | 76,764 | 34,010 | 2/2 | 0.99 | 1.6 / 1.8 |
| HTTP/2 over TLS, 512 connections of 4 streams | HAProxy | 49,104 (45,099-53,109) | 122,558 | 50,750 | 2/2 | 0.98 | 1.1 / 1.5 |

#### Static files

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 1 MiB | OxiRoute epoll (default) | 1,516 (1,513-1,518) | 44,110 | 8,600 | 2/2 | 2.0 / 1.9 |
| 1 MiB | nginx | 1,546 (1,508-1,584) | 36,988 | 13,265 | 2/2 | 1.7 / 1.6 |
| 1 MiB over TLS 1.3, kernel TLS and `sendfile` (nginx: user-space TLS) | OxiRoute epoll (default) | 1,378 (1,368-1,389) | 36,862 | 9,280 | 2/2 | 1.4 / 1.1 |
| 1 MiB over TLS 1.3, kernel TLS and `sendfile` (nginx: user-space TLS) | nginx | 706 (689-722) | 68,006 | 2,964,556 | 2/2 | 0.6 / 0.6 |
| 1 KiB | OxiRoute epoll (default) | 318,252 (315,513-320,991) | 222 | 6,553 | 2/3 | 1.7 / 2.6 |
| 1 KiB | nginx | 75,598 (74,262-76,934) | 862 | 13,205 | 2/2 | 1.8 / 2.0 |
| 1 KiB | nginx, `open_file_cache` | 88,855 (84,396-93,314) | 654 | 13,789 | 2/2 | 1.9 / 1.2 |

The 1 MiB lanes of this pass are steady across runs (1,513 to 1,518 plain, 1,368 to 1,389 over TLS), without the faster
first run seen in 7.0. The 1 KiB lane is 4.2 times nginx (318,252 against 75,598) and 3.6 times nginx with
`open_file_cache`; it kept two of three attempts.

### Scaling over two and four workers and aged scheduling

`listener_workers = N`, nginx `worker_processes = N`, HAProxy `nbthread N`. Two workers with every data plane, 1 KiB
(the 256-connection cell is a single run, see Conditions):

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | proxy CPUs used | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 KiB, 64 connections | OxiRoute epoll, default scheduling | 324,356 (320,604-328,108) | 404 | 11,812 | 2/2 | 1.89 | 0.8 / 1.3 |
| 1 KiB, 64 connections | OxiRoute tokio plane | 258,129 (250,421-265,837) | 456 | 18,857 | 2/12 | 1.90 | 0.8 / 1.3 |
| 1 KiB, 64 connections | OxiRoute io_uring | 299,490 (299,452-299,527) | 489 | 15,920 | 2/2 | 1.89 | 1.3 / 1.3 |
| 1 KiB, 64 connections | OxiRoute epoll, aged | 303,178 (286,064-320,293) | 538 | 12,640 | 2/2 | 1.83 | 0.6 / 1.7 |
| 1 KiB, 64 connections | nginx | 262,650 (259,557-265,742) | 424 | 19,917 | 2/2 | 1.95 | 0.7 / 1.5 |
| 1 KiB, 64 connections | HAProxy | 212,406 (204,404-220,408) | 588 | 35,474 | 2/2 | 1.92 | 0.3 / 1.5 |
| 1 KiB, 256 connections | OxiRoute epoll, default scheduling | 350,108 (350,108-350,108) | 1,635 | 11,799 | 1/1 | 1.97 | 0.0 / 0.3 |
| 1 KiB, 256 connections | OxiRoute tokio plane | 274,482 (274,482-274,482) | 1,434 | 18,739 | 1/1 | 1.98 | 0.1 / 0.2 |
| 1 KiB, 256 connections | OxiRoute io_uring | 336,451 (336,451-336,451) | 1,548 | 15,850 | 1/1 | 1.98 | 0.0 / 0.6 |
| 1 KiB, 256 connections | OxiRoute epoll, aged | 333,219 (333,219-333,219) | 2,376 | 12,594 | 1/1 | 1.95 | 0.1 / 0.4 |
| 1 KiB, 256 connections | nginx | 265,540 (265,540-265,540) | 1,928 | 20,098 | 1/1 | 1.99 | 0.1 / 0.0 |
| 1 KiB, 256 connections | HAProxy | 221,293 (221,293-221,293) | 1,550 | 37,435 | 1/1 | 1.98 | 0.0 / 0.0 |

With two workers and the default `work_stealing` scheduling the OxiRoute process used 1.89 and 1.97 CPUs of two in these
two lanes, against 0.99 in the 7.0 pass.

One, two and four workers at 512 to 2,048 connections, and the 512-connection lane in which 1 origin response in 500 is
delayed 200 ms:

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | proxy CPUs used | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| One worker, 512 connections | OxiRoute epoll (default) | 175,370 (172,422-178,319) | 6,754 | 11,746 | 2/2 | 0.97 | 0.5 / 0.9 |
| One worker, 512 connections | OxiRoute epoll, aged | 168,104 (167,742-168,466) | 6,695 | 12,521 | 2/2 | 0.98 | 1.2 / 1.5 |
| One worker, 512 connections | nginx | 114,182 (110,203-118,162) | 10,065 | 20,316 | 2/2 | 0.98 | 1.0 / 1.3 |
| One worker, 512 connections | HAProxy | 91,907 (91,175-92,639) | 18,811 | 39,602 | 2/2 | 0.99 | 0.9 / 1.3 |
| One worker, 2,048 connections | OxiRoute epoll (default) | 142,400 (136,625-148,175) | 22,354 | 11,741 | 2/2 | 0.99 | 0.7 / 1.0 |
| One worker, 2,048 connections | OxiRoute epoll, aged | 142,243 (140,688-143,798) | 19,642 | 12,514 | 2/2 | 0.99 | 0.3 / 1.0 |
| One worker, 2,048 connections | nginx | 62,593 (60,883-64,303) | 62,798 | 20,424 | 2/2 | 0.99 | 1.0 / 0.8 |
| One worker, 2,048 connections | HAProxy | 57,789 (56,595-58,983) | 114,897 | 40,033 | 2/2 | 0.98 | 0.9 / 0.9 |
| One worker, 512, 1 in 500 origin responses delayed 200 ms connections | OxiRoute epoll (default) | 167,645 (162,084-173,206) | 4,704 | 11,826 | 2/2 | 0.94 | 0.7 / 1.1 |
| One worker, 512, 1 in 500 origin responses delayed 200 ms connections | OxiRoute epoll, aged | 146,542 (141,441-151,643) | 12,422 | 12,612 | 2/2 | 0.91 | 0.5 / 1.7 |
| One worker, 512, 1 in 500 origin responses delayed 200 ms connections | nginx | 104,596 (100,335-108,858) | 19,176 | 20,408 | 2/2 | 0.94 | 1.0 / 1.2 |
| One worker, 512, 1 in 500 origin responses delayed 200 ms connections | HAProxy | 89,354 (84,643-94,065) | 17,484 | 39,328 | 2/2 | 0.93 | 0.6 / 0.8 |
| Two workers, 512 connections | OxiRoute epoll (default) | 331,811 (323,390-340,232) | 4,106 | 11,844 | 2/2 | 1.94 | 0.5 / 1.0 |
| Two workers, 512 connections | OxiRoute epoll, aged | 322,758 (322,424-323,092) | 4,995 | 12,602 | 2/2 | 1.94 | 0.6 / 1.1 |
| Two workers, 512 connections | nginx | 251,002 (242,833-259,170) | 3,287 | 20,120 | 2/2 | 1.97 | 0.3 / 1.2 |
| Two workers, 512 connections | HAProxy | 196,840 (181,195-212,485) | 6,601 | 37,648 | 2/2 | 1.97 | 0.6 / 1.2 |
| Two workers, 1,024 connections | OxiRoute epoll (default) | 316,118 (311,403-320,832) | 11,932 | 11,884 | 2/2 | 1.93 | 0.5 / 0.9 |
| Two workers, 1,024 connections | OxiRoute epoll, aged | 308,543 (299,606-317,480) | 10,058 | 12,634 | 2/2 | 1.97 | 0.4 / 1.3 |
| Two workers, 1,024 connections | nginx | 183,250 (171,317-195,184) | 13,860 | 20,153 | 2/2 | 1.97 | 0.5 / 0.8 |
| Two workers, 1,024 connections | HAProxy | 156,846 (155,434-158,258) | 9,858 | 38,626 | 2/2 | 1.98 | 0.3 / 0.7 |
| Two workers, 2,048 connections | OxiRoute epoll (default) | 253,178 (242,686-263,671) | 31,770 | 11,893 | 2/2 | 1.95 | 0.6 / 1.6 |
| Two workers, 2,048 connections | OxiRoute epoll, aged | 256,805 (246,740-266,870) | 24,692 | 12,580 | 2/2 | 1.94 | 0.6 / 1.2 |
| Two workers, 2,048 connections | nginx | 133,134 (128,567-137,702) | 22,204 | 20,162 | 2/2 | 1.98 | 0.6 / 1.0 |
| Two workers, 2,048 connections | HAProxy | 108,102 (102,957-113,248) | 37,208 | 38,774 | 2/3 | 1.98 | 0.9 / 0.9 |
| Two workers, 512, 1 in 500 origin responses delayed 200 ms connections | OxiRoute epoll (default) | 306,397 (303,406-309,388) | 4,302 | 11,904 | 2/2 | 1.84 | 0.5 / 1.2 |
| Two workers, 512, 1 in 500 origin responses delayed 200 ms connections | OxiRoute epoll, aged | 306,507 (298,573-314,441) | 3,263 | 12,666 | 2/2 | 1.83 | 0.5 / 1.1 |
| Two workers, 512, 1 in 500 origin responses delayed 200 ms connections | nginx | 242,702 (235,763-249,642) | 4,836 | 20,206 | 2/2 | 1.88 | 0.7 / 1.1 |
| Two workers, 512, 1 in 500 origin responses delayed 200 ms connections | HAProxy | 195,216 (192,958-197,473) | 6,744 | 36,926 | 2/2 | 1.88 | 0.9 / 1.1 |
| Four workers, 64 connections | OxiRoute epoll (default) | 336,284 (334,145-338,423) | 388 | 12,358 | 2/2 | 2.69 | 0.4 / 1.5 |
| Four workers, 64 connections | OxiRoute epoll, aged | 332,812 (332,694-332,929) | 380 | 13,662 | 2/2 | 2.70 | 0.9 / 1.3 |
| Four workers, 64 connections | nginx | 343,714 (332,673-354,754) | 395 | 20,222 | 2/2 | 3.24 | 0.5 / 1.3 |
| Four workers, 64 connections | HAProxy | 344,694 (337,213-352,176) | 389 | 35,155 | 2/2 | 3.67 | 0.8 / 1.5 |
| Four workers, 256 connections | OxiRoute epoll (default) | 338,358 (337,754-338,963) | 2,062 | 12,315 | 2/2 | 2.73 | 0.6 / 1.4 |
| Four workers, 256 connections | OxiRoute epoll, aged | 343,993 (340,887-347,099) | 1,886 | 13,584 | 2/2 | 2.72 | 0.6 / 1.5 |
| Four workers, 256 connections | nginx | 364,879 (357,317-372,441) | 1,811 | 20,116 | 2/2 | 3.31 | 0.4 / 1.2 |
| Four workers, 256 connections | HAProxy | 370,925 (364,086-377,764) | 1,928 | 35,424 | 2/2 | 3.80 | 0.5 / 1.1 |
| Four workers, 512 connections | OxiRoute epoll (default) | 332,304 (323,214-341,394) | 7,118 | 12,310 | 2/2 | 2.73 | 0.8 / 1.5 |
| Four workers, 512 connections | OxiRoute epoll, aged | 338,865 (335,893-341,837) | 6,924 | 13,624 | 2/2 | 2.81 | 1.0 / 1.3 |
| Four workers, 512 connections | nginx | 333,178 (328,740-337,615) | 7,024 | 20,145 | 2/2 | 3.21 | 0.5 / 1.8 |
| Four workers, 512 connections | HAProxy | 356,338 (349,427-363,250) | 5,896 | 35,884 | 2/2 | 3.78 | 0.8 / 1.6 |
| Four workers, 1,024 connections | OxiRoute epoll (default) | 292,146 (285,266-299,025) | 19,782 | 12,299 | 2/2 | 2.69 | 1.0 / 1.7 |
| Four workers, 1,024 connections | OxiRoute epoll, aged | 294,485 (291,570-297,400) | 20,139 | 13,516 | 2/2 | 2.76 | 0.9 / 1.5 |
| Four workers, 1,024 connections | nginx | 305,694 (295,438-315,950) | 19,448 | 20,243 | 2/2 | 3.19 | 0.6 / 1.6 |
| Four workers, 1,024 connections | HAProxy | 292,757 (291,873-293,641) | 18,612 | 35,986 | 2/2 | 3.60 | 0.7 / 1.7 |
| Four workers, 2,048 connections | OxiRoute epoll (default) | 271,710 (267,829-275,592) | 46,788 | 12,340 | 2/2 | 2.65 | 0.9 / 1.2 |
| Four workers, 2,048 connections | OxiRoute epoll, aged | 280,740 (273,969-287,510) | 45,236 | 13,651 | 2/2 | 2.70 | 0.9 / 1.3 |
| Four workers, 2,048 connections | nginx | 279,324 (277,892-280,755) | 43,870 | 20,176 | 2/2 | 3.20 | 0.4 / 1.7 |
| Four workers, 2,048 connections | HAProxy | 240,320 (223,004-257,635) | 40,766 | 36,660 | 2/2 | 3.77 | 0.6 / 1.7 |
| Four workers, 512, 1 in 500 origin responses delayed 200 ms connections | OxiRoute epoll (default) | 312,834 (296,570-329,099) | 5,820 | 12,396 | 2/2 | 2.58 | 1.0 / 1.5 |
| Four workers, 512, 1 in 500 origin responses delayed 200 ms connections | OxiRoute epoll, aged | 309,254 (300,038-318,470) | 6,350 | 13,674 | 2/2 | 2.59 | 0.7 / 1.4 |
| Four workers, 512, 1 in 500 origin responses delayed 200 ms connections | nginx | 317,714 (313,422-322,005) | 5,948 | 20,174 | 2/2 | 3.00 | 0.5 / 1.8 |
| Four workers, 512, 1 in 500 origin responses delayed 200 ms connections | HAProxy | 342,854 (325,619-360,088) | 3,998 | 35,512 | 2/2 | 3.61 | 0.7 / 1.4 |

### Memory

Proportional set size (PSS) of the proxy process with one worker, idle then with N idle keep-alive connections held
open, then after they are closed (the process is the same across the states, 32 first, then 1,024; three processes
each, median with min-max; MB):

| State | OxiRoute | nginx | HAProxy |
| --- | ---: | ---: | ---: |
| Idle | 11.5 (11.4-11.5) | 11.0 (10.9-11.0) | 31.2 (31.2-32.4) |
| 32 connections held | 12.1 (12.0-12.1) | 11.2 (11.2-11.3) | 31.6 (31.6-32.9) |
| 32 connections drained | 12.1 (12.1-12.1) | 11.2 (11.2-11.4) | 31.7 (31.6-32.9) |
| 1,024 connections held | 13.8 (13.7-13.8) | 20.4 (20.1-20.5) | 41.8 (40.3-42.2) |
| 1,024 connections drained | 13.8 (13.8-13.8) | 20.4 (20.1-20.6) | 40.4 (40.4-41.8) |

Memory after configuration reloads (quick pass, one run per proxy): one worker, 1,000 exact routes plus a catch-all,
summed PSS of the proxy's process tree in MB, measured by `benchmarks/r2-data-path/reload_memory.py` (OxiRoute through
the management API with the previous generation retired before measuring, nginx `SIGHUP`, HAProxy master-worker
`SIGUSR2`, each after the process tree stayed unchanged for one second). The script is new in this pass and differs from
the 7.0 script (different process accounting and configuration generation), so the rows are not comparable with the 7.0
rows (OxiRoute 41.6, nginx 26.2 and HAProxy 36.3 MB after 50 reloads).

| State | OxiRoute | nginx | HAProxy |
| --- | ---: | ---: | ---: |
| Idle | 33.1 | 12.8 | 27.0 |
| After 1 reload | 36.3 | 20.4 | 24.8 |
| After 10 reloads | 37.1 | 20.7 | 26.2 |
| After 50 reloads | 33.8 | 21.3 | 25.0 |

OxiRoute does not grow with reloads in this run (33.8 MB after 50 against 33.1 idle); nginx grows by 7.6 MB at the
first reload and by 0.9 MB after it, and HAProxy stays between 24.8 and 27.0 MB. PSI was 0.0.

Incremental resident memory per idle connection (resident memory with connections held minus empty process, divided by
the connection count) from the harness idle lanes: 5,000 connections for the first four rows, 1,000 for HTTP/2 after
eight closed streams each; 2 runs each, OxiRoute default (rustls) TLS backend.

| Idle workload | OxiRoute, bytes | nginx, bytes | HAProxy, bytes |
| --- | ---: | ---: | ---: |
| H1 keep-alive | 1,777 (1,774-1,780) | 9,903 (9,813-9,993) | 10,944 (9,713-12,176) |
| H1 keep-alive over TLS | 18,326 (17,323-19,328) | 34,805 (34,802-34,808) | 36,070 (35,813-36,326) |
| WebSocket | 12,520 (12,519-12,520) | 18,372 (18,371-18,372) | 6,484 (6,337-6,632) |
| WebSocket over TLS | 21,642 (21,641-21,642) | 59,830 (59,813-59,848) | 30,178 (30,118-30,237) |
| H2 over TLS | 27,852 (27,619-28,086) | 48,758 (48,267-49,250) | 47,796 (47,591-48,001) |

HAProxy is lower for idle plain WebSocket connections; OxiRoute is lower elsewhere in this table.

### HTTP/3

Comparison harness, one worker, loopback, shipped binary, 8 QUIC connections of 4 streams unless stated. The `cubic`
rows are the same OxiRoute listener with `http3_congestion_control = "cubic"`. nginx HTTP/2 over TCP is the same nginx
build on the same lanes (the new-connection row opens a new TLS connection per request). The nginx HTTP/2 POST row is a
single run.

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Proxy, 8 connections of 4 streams, 1 KiB | OxiRoute HTTP/3, `bbr` (default) | 65,808 (64,602-67,015) | 1,440 | 69,056 | 2/2 | 0.7 / 0.9 |
| Proxy, 8 connections of 4 streams, 1 KiB | OxiRoute HTTP/3, `cubic` | 69,843 (67,783-71,903) | 862 | 68,730 | 2/2 | 0.5 / 0.4 |
| Proxy, 8 connections of 4 streams, 1 KiB | nginx HTTP/3 | 62,538 (58,520-66,557) | 1,066 | 55,810 | 2/2 | 0.6 / 0.7 |
| Proxy, 8 connections of 4 streams, 1 KiB | nginx HTTP/2 over TCP | 98,260 (97,871-98,650) | 608 | 33,335 | 2/2 | 0.6 / 0.8 |
| Direct 1 KiB response | OxiRoute HTTP/3, `bbr` (default) | 174,649 (173,782-175,516) | 444 | 34,654 | 2/2 | 1.0 / 1.0 |
| Direct 1 KiB response | OxiRoute HTTP/3, `cubic` | 177,235 (173,949-180,521) | 354 | 34,370 | 2/3 | 0.3 / 0.6 |
| Direct 1 KiB response | nginx HTTP/3 | 117,212 (115,767-118,657) | 578 | 41,464 | 2/2 | 1.6 / 2.5 |
| Direct 1 KiB response | nginx HTTP/2 over TCP | 165,690 (164,329-167,051) | 404 | 22,421 | 2/2 | 0.9 / 1.3 |
| Proxy POST 1 KiB | OxiRoute HTTP/3, `bbr` (default) | 62,298 (62,104-62,493) | 831 | 67,481 | 2/2 | 1.1 / 1.4 |
| Proxy POST 1 KiB | OxiRoute HTTP/3, `cubic` | 55,711 (54,594-56,828) | 1,790 | 67,352 | 2/2 | 1.2 / 1.4 |
| Proxy POST 1 KiB | nginx HTTP/3 | 57,158 (56,305-58,012) | 998 | 59,866 | 2/2 | 1.1 / 1.9 |
| Proxy POST 1 KiB | nginx HTTP/2 over TCP | 67,226 (67,226-67,226) | 1,116 | 40,892 | 1/1 | 0.6 / 1.4 |
| New QUIC (HTTP/2: new TLS) connection per request, direct | OxiRoute HTTP/3, `bbr` (default) | 4,908 (4,638-5,179) | 4,140 | 1,883,132 | 2/2 | 1.6 / 1.6 |
| New QUIC (HTTP/2: new TLS) connection per request, direct | OxiRoute HTTP/3, `cubic` | 5,018 (4,818-5,219) | 3,771 | 1,881,752 | 2/2 | 1.0 / 1.7 |
| New QUIC (HTTP/2: new TLS) connection per request, direct | nginx HTTP/3 | 3,240 (3,232-3,249) | 4,868 | 2,551,035 | 2/2 | 1.1 / 1.5 |
| New QUIC (HTTP/2: new TLS) connection per request, direct | nginx HTTP/2 over TCP | 5,192 (5,165-5,219) | 2,434 | 1,573,148 | 2/2 | 1.4 / 1.3 |

OxiRoute HTTP/3 is level with nginx HTTP/3 through a proxy (65,808 against 62,538 requests per second, ranges
overlapping), 1.5 times it for a direct response (174,649 against 117,212) and 1.5 times it for new connections (4,908
against 3,240, with 1.9 against 2.6 million instructions per request). `bbr` and `cubic` are level on a loss-free
loopback except on POST (see above).

Lossy path (quick pass, one run per row): download of one 10 MiB file on a fresh connection at 40 ms round trip and 0.5%
packet loss in each direction, user and network namespace with `netem`, OxiRoute with `http3_segmentation_offload = false`,
server on CPUs 8-9 and curl on 12-15 (`benchmarks/r2-data-path/h3_impair.py download`). One run is the median and the p99.
The `cubic` and nginx HTTP/3 times vary strongly between runs on this path (the 7.0 pass measured 1.1 to 7.9 s and 25 to
30 s), so one run does not separate them from the 7.0 figures.

| Server | seconds | goodput MiB/s | server CPU ms per MiB | packets dropped by `netem` | PSI before / after |
| --- | ---: | ---: | ---: | ---: | ---: |
| OxiRoute HTTP/3, `bbr` (default) | 0.357 | 28.0 | 5.0 | 40 | 0.0 / 0.0 |
| OxiRoute HTTP/3, `cubic` | 13.112 | 0.8 | 7.0 | 37 | 0.0 / 0.0 |
| nginx HTTP/3 | 30.967 | 0.3 | 4.0 | 47 | 0.0 / 0.0 |
| nginx HTTP/2 over TCP | 2.664 | 3.8 | 0.0 | 17 | 0.0 / 0.0 |

The server CPU counters tick at 10 ms, so the CPU figures of a sub-second download are coarse.

### WebSocket, TCP relay, pools and other lanes (comparison harness)

32 connections unless stated; HAProxy and nginx run the same functional configuration (the nginx stream module for the
TCP relay). The two TCP relay rows are single runs.

| Lane | Proxy | req/s median (min-max) | p99 µs | instr/req | clean | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| WebSocket echo, 1 KiB message, 16 connections | OxiRoute epoll (default) | 174,132 (167,066-181,197) | 158 | 9,886 | 2/2 | 0.8 / 1.1 |
| WebSocket echo, 1 KiB message, 16 connections | nginx | 220,462 (219,698-221,226) | 132 | 1,530 | 2/2 | 1.2 / 1.4 |
| WebSocket echo, 1 KiB message, 16 connections | HAProxy | 179,158 (177,695-180,622) | 150 | 8,281 | 2/2 | 1.2 / 1.2 |
| TCP relay echo, 1 KiB, 16 connections | OxiRoute epoll (default) | 203,425 (203,425-203,425) | 151 | 5,045 | 1/1 | 0.5 / 0.6 |
| TCP relay echo, 1 KiB, 16 connections | nginx | 219,786 (219,786-219,786) | 87 | 1,983 | 1/1 | 0.1 / 0.0 |
| TCP relay echo, 1 KiB, 16 connections | HAProxy | 184,368 (184,368-184,368) | 153 | 7,310 | 1/1 | 0.2 / 0.2 |
| TCP relay echo, 64 KiB, 16 connections | OxiRoute epoll (default) | 78,700 (78,700-78,700) | 373 | 5,978 | 1/1 | 0.0 / 0.7 |
| TCP relay echo, 64 KiB, 16 connections | nginx | 36,437 (36,437-36,437) | 545 | 5,683 | 1/1 | 0.4 / 0.1 |
| TCP relay echo, 64 KiB, 16 connections | HAProxy | 26,120 (26,120-26,120) | 1,485 | 18,233 | 1/1 | 0.1 / 0.1 |
| Pool `least_connections`, 8 servers | OxiRoute epoll (default) | 170,064 (164,818-175,309) | 348 | 12,345 | 2/2 | 1.2 / 1.9 |
| Pool `least_connections`, 8 servers | nginx | 132,066 (130,704-133,427) | 394 | 20,490 | 2/2 | 1.3 / 1.1 |
| Pool `least_connections`, 8 servers | HAProxy | 113,108 (109,206-117,009) | 492 | 36,004 | 2/2 | 0.6 / 1.3 |
| Pool `weighted_round_robin`, 8 servers | OxiRoute epoll (default) | 179,268 (178,833-179,704) | 316 | 12,706 | 2/2 | 1.4 / 1.4 |
| Pool `weighted_round_robin`, 8 servers | nginx | 126,428 (125,749-127,108) | 457 | 20,296 | 2/2 | 1.3 / 1.5 |
| Pool `weighted_round_robin`, 8 servers | HAProxy | 107,705 (103,231-112,179) | 672 | 35,534 | 2/2 | 0.9 / 1.5 |
| HTTP/1.1 keep-alive, 1 connection | OxiRoute epoll (default) | 38,276 (37,878-38,674) | 47 | 12,747 | 2/2 | 1.2 / 1.4 |
| HTTP/1.1 keep-alive, 1 connection | nginx | 35,296 (34,463-36,129) | 54 | 20,845 | 2/2 | 1.0 / 0.9 |
| HTTP/1.1 keep-alive, 1 connection | HAProxy | 32,088 (31,131-33,045) | 60 | 37,300 | 2/2 | 1.1 / 1.1 |

### Feature lanes (quick pass, one run per cell)

One worker, 32 connections, 1 KiB response, the same functional configuration for the three proxies (header rules: the
same request and response header set, remove and add; 1,000 routes: 1,000 exact routes with a catch-all; 4 origin
servers with round robin; the access log is written to a file for OxiRoute and nginx and to stdout for HAProxy). Every
cell is one clean run (`1/1`). The basic authentication lane sends the same valid credentials on every request;
OxiRoute does not run bcrypt per request, nginx and HAProxy do (cost 10), so their rate is bounded by the
hash.

| Lane | Proxy | req/s | p99 µs | instr/req | clean | PSI before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 2 KiB request headers | OxiRoute epoll (default) | 138,878 | 389 | 46,051 | 1/1 | 0.0 / 0.1 |
| 2 KiB request headers | nginx | 99,170 | 424 | 48,444 | 1/1 | 0.0 / 0.0 |
| 2 KiB request headers | HAProxy | 104,974 | 428 | 55,023 | 1/1 | 0.0 / 0.0 |
| Access log on | OxiRoute epoll (default) | 173,218 | 316 | 13,656 | 1/1 | 0.0 / 0.1 |
| Access log on | nginx | 92,536 | 388 | 22,299 | 1/1 | 0.0 / 0.4 |
| Access log on | HAProxy | 79,786 | 491 | 41,564 | 1/1 | 0.1 / 0.2 |
| Request id on | OxiRoute epoll (default) | 187,179 | 220 | 11,736 | 1/1 | 0.0 / 0.0 |
| Request id on | nginx | 126,170 | 447 | 24,256 | 1/1 | 0.0 / 0.0 |
| Request id on | HAProxy | 106,459 | 424 | 44,515 | 1/1 | 0.0 / 0.0 |
| Request and response header rules | OxiRoute epoll (default) | 187,342 | 207 | 13,289 | 1/1 | 0.3 / 0.1 |
| Request and response header rules | nginx | 140,287 | 257 | 20,619 | 1/1 | 0.1 / 0.0 |
| Request and response header rules | HAProxy | 115,243 | 383 | 39,871 | 1/1 | 0.0 / 0.0 |
| 1,000 exact routes plus a catch-all | OxiRoute epoll (default) | 187,168 | 211 | 11,736 | 1/1 | 0.0 / 0.2 |
| 1,000 exact routes plus a catch-all | nginx | 140,877 | 262 | 20,356 | 1/1 | 0.1 / 0.0 |
| 1,000 exact routes plus a catch-all | HAProxy | 119,250 | 382 | 36,095 | 1/1 | 0.1 / 0.0 |
| 4 origin servers, round robin | OxiRoute epoll (default) | 187,035 | 212 | 11,761 | 1/1 | 0.0 / 0.0 |
| 4 origin servers, round robin | nginx | 140,047 | 277 | 20,189 | 1/1 | 0.0 / 0.0 |
| 4 origin servers, round robin | HAProxy | 120,160 | 373 | 35,479 | 1/1 | 0.0 / 0.0 |
| Literal 301 redirect, no upstream | OxiRoute epoll (default) | 397,412 | 107 | 5,386 | 1/1 | 0.1 / 0.3 |
| Literal 301 redirect, no upstream | nginx | 302,965 | 171 | 9,959 | 1/1 | 0.1 / 0.2 |
| Literal 301 redirect, no upstream | HAProxy | 264,907 | 232 | 16,415 | 1/1 | 0.1 / 0.2 |
| Basic authentication, bcrypt cost 10, same credentials | OxiRoute epoll (default) | 88,168 | 520 | 46,861 | 1/1 | 0.0 / 0.1 |
| Basic authentication, bcrypt cost 10, same credentials | nginx | 26 | 1,213,719 | 231,737,266 | 1/1 | 0.1 / 0.0 |
| Basic authentication, bcrypt cost 10, same credentials | HAProxy | 26 | 2,226,800 | 231,752,045 | 1/1 | 0.0 / 0.0 |

### Instruction-count harness (quick pass, OxiRoute only)

`benchmarks/r2-data-path/run.py`, one repetition per lane, 16 connections, one worker on CPUs 8-9, origin on 10-11, load
generator on 12-15, the published binary with `OXIROUTE_INTERNAL_TEST_DIRECT_RUNTIME=1`. There is no nginx or HAProxy
configuration for these lanes: nginx answers `CONNECT` with 405 and neither comparator has the breaker, outlier
ejection and capacity admission of the seven-feature lane. `instr/req` is user-space instructions per request over the
measured window with the idle baseline subtracted; every lane had 0 request errors and PSI 0.0 to 0.1.

| Lane | req/s | p99 µs | instr/req |
| --- | ---: | ---: | ---: |
| Reverse proxy, 7 production features (forwarded headers, access log, timeouts, passive health, breaker, outlier ejection, capacity), 1 KiB | 154,355 | 193 | 19,006 |
| Forward proxy, HTTP/1.1 absolute-URI request, 1 KiB | 39,742 | 517 | 56,820 |
| Forward proxy, CONNECT tunnel echo, 1 KiB | 173,693 | 137 | 14,572 |
| Forward proxy, new CONNECT tunnel per request, 1 KiB | 25,291 | 844 | 65,933 |

## Lanes not measured

These were not measured in the 7.1 pass, and no 7.1 figure exists for them.

- The instruction-count harness lanes other than the four of the quick pass (seven-feature reverse proxy and the three
  forward-proxy lanes): its own HTTP/3, WebSocket, TCP relay, pool, static, TLS and upstream HTTP/2 lanes. The comparison
  harness measures most of those workloads above, with nginx and HAProxy; the instruction-count harness was not run on them.
- RTMP: the harness has no RTMP load client and the nginx build has no RTMP module.
- An arm64 binary: no arm64 host.
- The forward proxy against nginx and HAProxy: nginx answers `CONNECT` with 405, and no forward-proxy configuration was
  written for either.
- The BBR fairness matrix: its tracked results are in the repository (`benchmarks/r2-data-path/fairness/results`).
- The OpenSSL TLS backend, and the `io_uring` column of the TLS and HTTP/2 lanes (the plane does not serve TLS).

## What is not covered

- One worker per proxy except in the scaling tables (two and four workers). None is part of the claims.
- nginx and HAProxy run minimal proxy configurations on the same loopback; their tuning was not explored.
- Four cells of the main pass and every cell of the quick pass have a single clean run (see Conditions).
- Foreign work on the pinned CPUs was not a discard criterion in this pass; the rows measure the shipped binary on a
  shared host and are not strictly comparable with the 7.0 rows.
- Closed-loop load, loopback, shared host: results show relative behavior of builds on that host, not capacity.
