# HTTP/1 engine on io_uring: short measurement

`http_data_plane = "io_uring"` is opt-in. This is a short measurement of the same engine on the two drivers, next to
the tokio plane, nginx and HAProxy. It is not the full matrix of `docs/developer/BENCHMARK_VS_NGINX_HAPROXY.md`.

## Method

- One host, one release build of OxiRoute (fat LTO) for every OxiRoute column, selected per column by
  `http_data_plane` (`epoll`, `io_uring`, `tokio`). `before` is the io_uring plane of the build before the new
  connection fix (same engine, same host). nginx 1.30 and HAProxy with the settings of the benchmark document. The
  same in-tree origin and load generator, closed loop.
- 1 worker (`listener_workers = 1`, nginx `worker_processes 1`) and 2 workers; proxy pinned to CPUs 8-9, origin 10-11,
  load generator 12-15. 1 KiB and 64 KiB bodies; keep-alive with 32 and 256 connections, and a new connection per
  request with 32 connections.
- Rows of 4 s after a 2 s warm-up, 3 attempts per harness pass; the median of the clean attempts is reported
  (`clean/attempts`). The foreign-CPU guard was left as it is. The host was shared with other jobs and the guard
  discarded many attempts, so the keep-alive and 64 KiB rows pool the attempts of up to three passes of the same
  builds. `/proc/pressure/cpu` `some` avg10 was recorded before and after every attempt and is shown as a range; it
  was not used to wait, discard or retry.
- Instructions and cycles are user-space (`perf stat`, `instructions:u`, `cycles:u`) per request.

## Throughput and CPU cost

| scenario | conns | plane | clean/attempts | req/s | p50 us | p99 us | instr/req | cycles/req | PSI some avg10 |
| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| new connection | 32 | engine epoll | 2/3 | 86,839 | 338 | 728 | 15,984 | 10,818 | 0.2-1.7 |
| new connection | 32 | engine io_uring | 3/3 | 86,031 | 342 | 706 | 19,383 | 11,828 | 0.6-1.0 |
| new connection | 32 | engine io_uring before | 2/3 | 59,994 | 412 | 1,780 | 20,984 | 13,864 | 0.6-2.4 |
| new connection | 32 | tokio plane | 2/3 | 66,506 | 448 | 987 | 28,516 | 23,412 | 0.4-2.2 |
| new connection | 32 | nginx | 2/3 | 69,716 | 424 | 769 | 21,460 | 18,470 | 0.2-2.2 |
| keep-alive | 32 | engine epoll | 3/8 | 154,521 | 192 | 399 | 14,038 | 8,456 | 0.0-39.8 |
| keep-alive | 32 | engine io_uring | 4/9 | 167,649 | 178 | 335 | 16,666 | 7,860 | 0.0-42.0 |
| keep-alive | 32 | engine io_uring before | 3/7 | 171,041 | 179 | 340 | 16,639 | 7,808 | 0.0-48.5 |
| keep-alive | 32 | tokio plane | 4/9 | 125,366 | 254 | 416 | 23,728 | 17,038 | 0.0-39.5 |
| keep-alive | 32 | nginx | 3/8 | 133,821 | 229 | 386 | 19,899 | 12,880 | 0.1-45.7 |
| keep-alive | 256 | engine epoll | 5/9 | 108,302 | 2,413 | 3,509 | 20,455 | 13,094 | 0.0-3.2 |
| keep-alive | 256 | engine io_uring | 4/8 | 150,952 | 1,554 | 2,886 | 17,461 | 9,571 | 0.0-5.4 |
| keep-alive | 256 | engine io_uring before | 7/9 | 146,340 | 1,601 | 3,184 | 17,614 | 10,046 | 0.0-5.1 |
| keep-alive | 256 | tokio plane | 5/6 | 118,552 | 2,080 | 3,746 | 23,701 | 18,182 | 0.0-4.6 |
| keep-alive | 256 | nginx | 6/9 | 132,986 | 1,867 | 3,006 | 20,295 | 13,603 | 0.0-5.5 |
| keep-alive 64 KiB | 32 | engine epoll | 3/6 | 46,611 | 677 | 1,010 | 15,323 | 11,556 | 0.0-5.1 |
| keep-alive 64 KiB | 32 | engine io_uring | 2/6 | 28,583 | 1,052 | 3,254 | 46,488 | 30,886 | 0.1-5.0 |
| keep-alive 64 KiB | 32 | engine io_uring before | 2/6 | 28,952 | 1,064 | 1,724 | 46,292 | 30,256 | 0.1-4.8 |
| keep-alive 64 KiB | 32 | tokio plane | 4/6 | 47,896 | 645 | 1,109 | 26,388 | 25,008 | 0.1-4.4 |
| keep-alive 64 KiB | 32 | nginx | 4/6 | 35,238 | 880 | 1,390 | 37,786 | 28,468 | 0.1-5.2 |
| keep-alive, 2 workers | 64 | engine epoll | 4/4 | 155,654 | 380 | 792 | 14,390 | 8,666 | 0.0-0.7 |
| keep-alive, 2 workers | 64 | engine io_uring | 4/5 | 318,918 | 189 | 344 | 16,714 | 8,517 | 0.0-1.0 |
| keep-alive, 2 workers | 64 | engine io_uring before | 3/5 | 315,453 | 188 | 393 | 16,666 | 8,734 | 0.1-0.9 |
| keep-alive, 2 workers | 64 | tokio plane | 1/4 | 219,239 | 283 | 520 | 23,827 | 18,570 | 0.1-0.4 |
| keep-alive, 2 workers | 64 | nginx | 1/3 | 266,057 | 234 | 418 | 19,935 | 13,321 | 0.1-0.6 |

Interleaved new-connection rows with the same builds on a quiet host (PSI 0.1-1.4) gave 91.7k (epoll), 92.7k
(io_uring), 53.1k (io_uring before) and 71.9k (tokio) req/s as medians of five rounds; the io_uring plane alone held
91.3k over 14 further rounds. One interleaved round of both io_uring builds stalled (about 150 req/s, p99 seconds):
the host's connection tracking table was at 83% of its limit afterwards, which drops new connections of any plane,
and the stall did not recur in the 14 further rounds. It is noted as unexplained by the ring.

The epoll engine's 2-worker row is the median of attempts that mostly served every connection from one thread (0.99
CPUs used of 2); one attempt used both threads and reached 322k req/s. That is the epoll engine's balance, not the
ring's: the io_uring threads used 1.9 to 2.0 CPUs in every attempt.

## System calls per request

`strace -f -c` slope between 10,000 and 30,000 requests, keep-alive, 32 connections, 1 worker, measured on the build
before the new connection fix; strace slows the proxy and changes batching, so read these as the shape, not as a
rate.

| plane | system calls per request |
| --- | ---: |
| engine epoll | 4.06 (2.00 `recvfrom`, 2.00 `sendmsg`, 0.06 `epoll_pwait`) |
| engine io_uring | 0.06 (`io_uring_enter`) |
| tokio plane | 4.07 |
| nginx | 5.06 |

## What this shows

- New connection per request: the slowdown was teardown cost, not a slow accept path. The proxy thread saw the
  client's FIN only after it had finished with the connection and then paid the kernel's teardown of both ends
  (`tcp_rcv_state_process`, `tcp_fin`, `tcp_time_wait` in its own profile, and in no other plane's). The ring now ends
  the sending direction as soon as the last response is written and hangs up a closed connection's receiving side, so
  the client closes first and its context pays. The plane went from 60.0k to 86.0k req/s, level with the epoll
  engine (86.8k), ahead of nginx (69.7k) and the tokio plane (66.5k), at 9% more user cycles per request than epoll.
- Keep-alive at 32 and 256 connections and with two workers did not change (167.6k against 171.0k before at 32,
  inside the noise of the shared host; cycles per request equal). At 256 connections it stays well ahead of the epoll
  engine (151.0k against 108.3k) and nginx (133.0k).
- 64 KiB bodies are the weak row: 28.6k against 46.6k for the epoll engine, with 46.5k instructions and 30.9k user
  cycles per request against 15.3k and 11.6k. The extra user time is two copies of the body (the provided receive
  buffer into the engine's buffer, the engine's buffer into the ring's staging buffer for the send; `memcpy` is 9% of
  samples and absent from the epoll profile) and four times as many receive completions, because the provided
  buffers are 16 KiB. Provided buffers of 64 KiB raised it to 35.9k against 44.4k for epoll in a same-session pair of
  non-LTO builds, at four times the receive pool. Removing the copies needs the engine to relay buffers the ring owns
  instead of copying through its own, which is a change of the engine's relay, not of the ring.
- The io_uring plane replaces about 4 system calls per request with 0.06 (it submits and reaps in batches); the
  user-space instruction count does not fall because the copies remain.
- Memory: the receive buffer pool of the ring costs 1 to 3 MB more than the epoll engine at 16 KiB buffers.

## Reproducing

The harness is `benchmarks/scripts/vs_nginx_haproxy.py` (`VS_ALT_BINS="label=/path/to/oxiroute@VS_DATA_PLANE=io_uring"`
selects the plane of a column through a per-build variable once the harness writes `http_data_plane` from it;
`/proc/pressure/cpu` recording per attempt was added the same way) and `benchmarks/scripts/vs_syscalls.py`.
