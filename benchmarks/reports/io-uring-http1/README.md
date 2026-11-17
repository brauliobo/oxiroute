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
- 64 KiB bodies were the weak row (28.6k against 46.6k for the epoll engine, 46.5k instructions per request against
  15.3k). The extra user time was two copies of the body and four times as many receive completions. The engine now
  frames the body in place in the buffer the ring received it into and the ring sends the response head and that
  buffer in one `sendmsg`; the buffer returns to the ring when the send completes, so the body is not copied.
  Sockets that receive full 16 KiB completions switch to a second pool of 64 KiB buffers. The plane now serves 55.4k
  req/s against 46.0k for the epoll engine and 35.1k for nginx (1 worker), 3.1k against 3.0k at 1 MiB, and 89.8k
  against 45.7k at 64 KiB with 2 workers. Instructions per request fell from 46.7k to 20.4k.
- A wait for completions now keeps entering the ring until the completion queue stops growing: with `DEFER_TASKRUN`
  the kernel hands over only a few deferred completions per entry, which delivered batched completions to the engine
  in the wrong order and failed the scheduling order tests on this driver.
- The io_uring plane replaces about 4 system calls per request with 0.06 (it submits and reaps in batches); the
  user-space instruction count stays higher than epoll's at small bodies (18.2k against 14.2k at 1 KiB).
- Memory: the second pool is allocated but not touched until a socket streams. Idle cost per connection is 10.1 kB
  against 13.7 kB before. Resident set at 64 KiB bodies is 36.4 MB (22.5 MB proportional) against 33.5 MB (19.1 MB)
  before, and 37.1 MB proportional against 29.1 MB before at 1024 keep-alive connections with 1 KiB bodies (single
  samples, not yet explained).

## Body relay after the copy removal

Pooled clean attempts, 1 worker unless stated, same method as above. `before` is the io_uring plane without the change.

| scenario | plane | clean/attempts | req/s | p50 us | p99 us | instr/req | cycles/req |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 64 KiB, 32 conns | engine epoll | 3/4 | 45,995 | 680 | 1,066 | 15,659 | 11,446 |
| 64 KiB, 32 conns | engine io_uring | 3/4 | 55,408 | 541 | 1,027 | 20,417 | 10,184 |
| 64 KiB, 32 conns | engine io_uring before | 3/3 | 28,632 | 1,082 | 1,655 | 46,744 | 30,801 |
| 64 KiB, 32 conns | tokio plane | 3/3 | 46,089 | 660 | 1,240 | 26,389 | 24,906 |
| 64 KiB, 32 conns | nginx | 3/5 | 35,092 | 869 | 1,404 | 38,192 | 28,271 |
| 1 MiB, 32 conns | engine epoll | 3/4 | 3,005 | 10,041 | 16,928 | 37,799 | 53,665 |
| 1 MiB, 32 conns | engine io_uring | 3/4 | 3,084 | 9,647 | 16,068 | 53,482 | 48,473 |
| 1 MiB, 32 conns | engine io_uring before | 3/5 | 1,736 | 17,780 | 26,299 | 497,816 | 407,281 |
| 1 MiB, 32 conns | tokio plane | 3/5 | 3,709 | 8,613 | 27,404 | 39,270 | 81,961 |
| 1 MiB, 32 conns | nginx | 3/4 | 1,898 | 16,872 | 31,670 | 262,019 | 209,834 |
| 1 KiB, 32 conns | engine epoll | 3/3 | 169,566 | 185 | 256 | 14,151 | 7,813 |
| 1 KiB, 32 conns | engine io_uring | 3/3 | 164,150 | 186 | 356 | 18,175 | 8,534 |
| 1 KiB, 32 conns | engine io_uring before | 3/8 | 167,210 | 181 | 358 | 16,679 | 8,209 |
| 1 KiB, 256 conns | engine epoll | 3/3 | 127,414 | 1,929 | 3,654 | 17,059 | 10,485 |
| 1 KiB, 256 conns | engine io_uring | 3/5 | 141,952 | 1,724 | 3,056 | 18,755 | 9,153 |
| 1 KiB, 256 conns | engine io_uring before | 3/6 | 147,514 | 1,566 | 3,435 | 17,382 | 9,713 |
| 64 KiB, 2 workers, 64 conns | engine epoll | 3/3 | 45,682 | 1,366 | 2,086 | 15,856 | 11,459 |
| 64 KiB, 2 workers, 64 conns | engine io_uring | 3/5 | 89,756 | 616 | 2,079 | 20,609 | 11,864 |
| 64 KiB, 2 workers, 64 conns | engine io_uring before | 3/6 | 51,499 | 1,189 | 2,192 | 46,677 | 34,951 |
| 64 KiB, 2 workers, 64 conns | tokio plane | 3/3 | 86,042 | 705 | 1,719 | 26,574 | 26,174 |
| 64 KiB, 2 workers, 64 conns | nginx | 3/5 | 64,069 | 936 | 2,070 | 37,985 | 28,097 |

The 1 KiB rows are within the noise of the shared host (PSI some avg10 up to 9). A confirmation run of the final build
at 64 KiB gave 55.2k req/s for io_uring against 43.6k for epoll.

## Reproducing

The harness is `benchmarks/scripts/vs_nginx_haproxy.py` (`VS_ALT_BINS="label=/path/to/oxiroute@VS_DATA_PLANE=io_uring"`
selects the plane of a column through a per-build variable once the harness writes `http_data_plane` from it;
`/proc/pressure/cpu` recording per attempt was added the same way) and `benchmarks/scripts/vs_syscalls.py`.
