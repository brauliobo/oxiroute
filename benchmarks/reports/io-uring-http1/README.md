# HTTP/1 engine on io_uring: short measurement

`http_data_plane = "io_uring"` is opt-in. This is a short first measurement of the same engine on the two drivers, next
to the tokio plane and nginx. It is not the full matrix of `docs/developer/BENCHMARK_VS_NGINX_HAPROXY.md`.

## Method

- One host, one release build of OxiRoute for every OxiRoute column, selected per column by `http_data_plane`
  (`epoll` is the default and was left unset, `io_uring`, `tokio`). nginx 1.30 with the settings of the
  benchmark document. The same in-tree origin and load generator, closed loop.
- 1 worker (`listener_workers = 1`, nginx `worker_processes 1`), proxy pinned to CPUs 8-9, origin 10-11, load
  generator 12-15.
- 1 KiB body. Scenarios: keep-alive with 32 connections, keep-alive with 256 connections, a new connection per request
  with 32 connections.
- Rows of 4 s after a 2 s warm-up, at most 3 attempts per row; the median of the clean attempts is reported
  (`clean/runs`). The harness's foreign-CPU guard was left as it is and discarded attempts that it rejected. The host
  was shared with other jobs. `/proc/pressure/cpu` `some` avg10 was recorded before and after every attempt and is
  shown as a range; it was not used to wait, discard or retry.
- Instructions and cycles are user-space (`perf stat`, `instructions:u`, `cycles:u`) per request. PSS is the sum
  over the proxy processes after the run.

## Throughput, latency, CPU cost, memory

| scenario | conns | plane | clean/runs | req/s | p50 us | p99 us | instr/req | cycles/req | PSS MB | PSI some avg10 |
| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| keep-alive | 32 | engine epoll | 3/3 | 166,708 | 188 | 302 | 15,485 | 8,455 | 10.7 | 0.0-0.2 |
| keep-alive | 32 | engine io_uring | 3/3 | 170,280 | 181 | 301 | 18,129 | 8,502 | 11.9 | 0.0-0.6 |
| keep-alive | 32 | tokio plane | 2/3 | 121,292 | 254 | 590 | 24,602 | 17,406 | 9.5 | 0.0-0.8 |
| keep-alive | 32 | nginx | 1/3 | 132,513 | 232 | 480 | 19,871 | 13,030 | 10.4 | 0.0-0.8 |
| keep-alive | 256 | engine epoll | 1/3 | 103,910 | 2,434 | 2,783 | 25,454 | 15,079 | 16.3 | 0.0-0.2 |
| keep-alive | 256 | engine io_uring | 3/3 | 152,632 | 1,605 | 3,286 | 18,474 | 10,090 | 19.4 | 0.0-0.5 |
| keep-alive | 256 | tokio plane | 3/3 | 117,898 | 2,109 | 2,800 | 24,537 | 17,670 | 14.1 | 0.0-0.2 |
| keep-alive | 256 | nginx | 3/3 | 134,108 | 1,861 | 3,017 | 20,252 | 13,209 | 12.5 | 0.0-0.2 |
| new connection | 32 | engine epoll | 2/3 | 82,606 | 348 | 788 | 18,065 | 12,240 | 10.9 | 0.2-1.1 |
| new connection | 32 | engine io_uring | 3/3 | 54,129 | 443 | 1,863 | 22,400 | 15,100 | 13.2 | 0.1-1.1 |
| new connection | 32 | tokio plane | 3/3 | 67,375 | 443 | 969 | 29,421 | 23,119 | 9.6 | 0.2-0.6 |
| new connection | 32 | nginx | 2/3 | 68,995 | 424 | 862 | 21,499 | 18,610 | 11.1 | 0.2-1.0 |

A second pass over the 256 connection cell measured the epoll engine at 110,486 req/s (2/3 clean); every io_uring
attempt of that pass was discarded by the foreign-CPU guard.

## System calls per request

`strace -f -c` slope between 10,000 and 30,000 requests, keep-alive, 32 connections, 1 worker. strace slows the proxy
and changes batching, so read these as the shape, not as a rate.

| plane | system calls per request |
| --- | ---: |
| engine epoll | 4.06 (2.00 `recvfrom`, 2.00 `sendmsg`, 0.06 `epoll_pwait`) |
| engine io_uring | 0.06 (`io_uring_enter`) |
| tokio plane | 4.07 |
| nginx | 5.06 |

## What this shows

- io_uring is not faster everywhere. At 32 keep-alive connections it is level with the epoll engine (+2%, inside the
  noise of the shared host) and uses more user-space instructions per request (18.1k against 15.5k): the engine
  copies received and staged bytes between its own buffers and the ring's.
- At 256 keep-alive connections it was faster in the clean attempts (152.6k against 103.9k to 110.5k for the epoll
  engine, with fewer instructions and cycles per request). The epoll engine's cost per request rises with the
  connection count here; the io_uring one does not. The epoll cell had few clean attempts, so treat the size of the
  gap as indicative.
- With a new connection per request it is slower (54.1k against 82.6k) and its tail is worse; the proxy did not
  saturate its CPU (0.81 CPUs used against 0.98), so the engine's accept and connect path waits on completions where
  epoll does not. This is the first thing to look at before recommending the setting for that traffic.
- It replaces about 4 system calls per request with 0.06 (it submits and reaps in batches); the user-space
  instruction count does not fall because the copies remain.
- Memory: 1 to 3 MB more PSS than the epoll engine (the receive buffer pool of the ring).

64 KiB bodies and two workers were not run.

## Reproducing

The harness is `benchmarks/scripts/vs_nginx_haproxy.py` (`VS_ALT_BINS="label=/path/to/oxiroute@VS_DATA_PLANE=io_uring"`
selects the plane of a column through a per-build variable once the harness writes `http_data_plane` from it; PSS and
`/proc/pressure/cpu` recording per attempt were added the same way) and `benchmarks/scripts/vs_syscalls.py`.
