# R2 data-path harness

Deterministic, instruction-count-first measurements of the OxiRoute proxy data path (HTTP/1.1, HTTP/2,
TLS termination, static files, compression, cache, WebSocket and TCP passthrough) and of the marginal cost
of each per-request feature. Wall-clock numbers on a shared host are unreliable, so the primary metric is
**user-space instructions per request**, attached to the proxy for exactly the measured request window.

## Pieces

| File | Purpose |
| --- | --- |
| `run.sh` | Entry point; `run.sh --help` lists every option. Wraps `run.py`. |
| `run.py` | Lane table, process orchestration, `perf stat` / strace / allocator-shim / `perf record` measurements. |
| `gen_config.py` | Renders the Lua configuration of a lane from independent feature switches. |
| `tool/` | `r2tool`: in-tree upstream (0 B, 1 KiB, 64 KiB, 1 MiB, chunked, slow, sink, upgrade echo, TCP echo) and a fixed-count load generator (h1, h1+TLS, h2c, h2+TLS, HTTP/3, WebSocket-style upgrade, TCP echo; keep-alive or new connection per request; header padding; request bodies; p50/p99/p99.9). `alloc_count.c` is an `LD_PRELOAD` shim counting allocator and `clock_gettime` calls. |
| `table.py` | Compact table of a `results.json`, optionally with instruction deltas against a previous run. |
| `profile_summary.py` | Component and symbol summary of a `--profile` `perf.data`. |
| `soak.py` | Mixed-traffic soak of one process (keep-alive, new connection, POST, 1 MiB upload, slow response, WebSocket-style upgrade over plain and TLS, TLS HTTP/1.1, HTTP/2, TCP echo, paced) with RSS, descriptor and thread samples, a configuration reload through the management API in the middle, an origin stop and a SIGTERM at the end. |
| `memcpy_profile.py` | Copies of 48 bytes or more per request, by caller (`tool/memcpy_profile.c` preloaded): shows async state machines moved to the heap or into another future. |

## Method

* The load generator warms up (connections plus `--warmup` requests), runs a synchronous hook (`--hook-start`),
  sends exactly `--requests` requests, then runs `--hook-stop`. There are no timers used for synchronisation.
* `perf stat -D -1 --control fifo:...` is attached to the proxy; the hooks enable and disable it, so the counted
  window is the measured requests only. `instructions:u` and `cycles:u` of an idle window (2 s, same process) are
  scaled by the elapsed time and subtracted. Result: instructions/request with a typical spread below 1% (the
  `instr_spread_pct` column is `(max-min)/median` over `--reps` repetitions, default 5).
* Voluntary and involuntary context switches are the delta of `/proc/PID/task/*/status` over the same window.
* `--sys`: syscalls per request are the slope of `strace -f -c` totals between two request counts (`--sys-low`,
  `--sys-high`), so startup syscalls cancel. strace perturbs timing, so batching (`epoll_wait`, `futex`) is
  indicative; per-request syscalls such as `recvfrom` and `write` are exact.
* `--alloc`: allocator calls, requested bytes and `clock_gettime` calls per request from the `LD_PRELOAD` shim
  (valgrind does not run on glibc 2.44 with AVX-512, so callgrind/dhat are unavailable on the reference host).
* `--mem`: heap and RSS per idle keep-alive connection.
* `--profile`: `perf record -e instructions:u -c PERIOD` (flat, per-function instruction sampling; instruction
  sampling skids onto the instruction after long-latency atomics, so lock-heavy libc spots are over-weighted).
  With `--profile-bin` pointing at a frame-pointer build (`RUSTFLAGS='-C force-frame-pointers=yes'`) it records
  `--call-graph fp`, and `perf report --children` gives inclusive costs. DWARF unwinding is not permitted by
  `perf_event_paranoid=2`. `--profile-call-graph MODE` overrides the mode (`lbr` needs a CPU and kernel that allow
  branch-stack sampling of user space, which a virtualised host often does not).
* Every timed section runs under `flock /tmp/oxiroute-bench.lock`. Ports are taken from `20300-20499` (the
  ephemeral range overlaps it, so a block is probed first). Everything binds `127.0.0.1`.
* The proxy runs with `OXIROUTE_INTERNAL_TEST_DIRECT_RUNTIME=1` (single process, no supervisor); the data-path code
  is identical to the supervised worker.
* Proxy, upstream and load generator are pinned with `taskset` to `R2_PROXY_CPUS` (default `8-11`),
  `R2_UPSTREAM_CPUS` (`12-13`) and `R2_LOAD_CPUS` (`14-15`). Each Pingora listener is one service with one worker
  thread, so scaling lanes use `listeners=N` and need `N + 1` proxy CPUs (`R2_PROXY_CPUS=4-11`).

## Build

```sh
# proxy (the shipped release profile; debug line tables do not change code generation)
CARGO_PROFILE_RELEASE_DEBUG=line-tables-only CARGO_PROFILE_RELEASE_STRIP=false \
  nice cargo build --release --locked -p oxiroute --bin oxiroute -j4
# harness tool (separate workspace, offline-capable)
(cd benchmarks/r2-data-path/tool && nice cargo build --release -j2)
```

## Run

```sh
export OXIROUTE_BIN=target/release/oxiroute
benchmarks/r2-data-path/run.sh --lanes default --out /tmp/r2-before          # instructions, latency, RSS
benchmarks/r2-data-path/run.sh --lanes f_base,f_access_log --no-perf --alloc --sys --out /tmp/r2-before
benchmarks/r2-data-path/run.sh --lanes f_base --no-perf --mem --out /tmp/r2-before
benchmarks/r2-data-path/run.sh --lanes f_base --no-perf --profile --out /tmp/r2-before
python3 benchmarks/r2-data-path/table.py /tmp/r2-after/results.json /tmp/r2-before/results.json   # deltas
```

`run.sh --list` prints all lanes. Output is one `key=value` line per lane (also stored as `results.json`):

`instr_per_req cycles_per_req vctxsw_per_req nvctxsw_per_req rps p50_us p99_us p999_us connects errors rss_kb
hwm_kb threads fds` and, with the optional modes, `syscalls_per_req sys_*`, `mallocs_per_req frees_per_req
alloc_bytes_per_req clock_gettime_per_req`, `heap_live_bytes_per_conn rss_bytes_per_conn`.

## Lanes

* Bodies: `h1_b0 h1_b1k h1_b64k h1_b1m h1_stream h1_slow`; connection counts `h1_b1k_c{1,16,256,512,1024,2048,4096}`;
  `h1_newconn_b1k`; headers `h1_hdr{2048,8192}`; request bodies `h1_post{4,64,1024}k`, `h1_post1k`, `h1_post{1,64}k_chunked` (`--chunk-bytes`: chunked request
  framing), `h1_post64k_reqbuf`, `h1_post64k_retrybuf`.
* Feature A/B on one workload (16 keep-alive connections, 1 KiB): `f_base f_fixed f_forwarded f_access_log f_timeouts f_retry
  f_queue f_capacity f_passive f_breaker f_outlier f_nginx_host f_all_6_0`, plus `dns_on_connect`, `dns_startup`,
  `pool_s2`.
* Other actions: `gzip_html64k nogzip_html64k cache_hit_b1k cache_hit_b64k static_f{1k,64k,1m} ws_echo_{64,1k,16k} l4_echo_{1k,16k,64k,256k}, l4_echo_{16k,64k,256k}_paced`
  (a `ws_echo` request is one message echoed through an upgraded connection).
* TLS (OpenSSL via Pingora) and HTTP/2: `tls_h1_keepalive tls_hs_full_{tls13,tls12,rsa2048}
  tls_hs_resume_{cache_tls12,cache_tls13,tickets_tls13,client_only} tls_h1_b1m h2_b1k_c4s16 h2_b64k_c4s16 h2_b0_c1s1 h2_post1k_c4s16`.
* HTTP/3 listener (see [HTTP/3](#http3)): `h3_fixed_b1k_c8s4 h3_fixed_b1k_c1s1 h3_fixed_b64k_c8s4 h3_newconn_fixed_b1k`
  (answered by the listener itself), `h3_b1k_c8s4 h3_post1k_c8s4` (proxied to the HTTP/1.1 origin; they need a proxy that
  supports that, an older binary is reported as `skipped=...` instead of measured).
* HTTP/2 to the origin (`upstream_h2` feature: the upstream tool serves TLS with ALPN `h2` and a throwaway certificate
  made with `openssl`, the pool trusts it): `h1_b1k_h2up h1_b64k_h2up h1_post1k_h2up`.
* Scaling over listeners: `scale_l{1,2,4,8}` and `scale_all_l{1,2,4,8}` (all 6.0 features on), and `scale4_<feature>`
  (one feature at 4 listeners) to bisect which feature breaks scaling. Pool size: `pool_s{2,64}`, `pool_s64_passive`,
  `pool_s1_passive`.
* Scaling over the worker threads of one listener (`listener_workers`): `scale_w{1,2,4,8}` and `scale_all_w{1,2,4,8}`;
  they need `N + 1` proxy CPUs (`R2_PROXY_CPUS=4-11`). Run them with `--sys` to read the futex calls per request
  (lock waits) next to `vctxsw_per_req` (voluntary context switches: a thread that slept on a lock or a wake-up).
* TLS handshake offload: `tls_storm{1000,3000}_h2_c1s1` serve one HTTP/2 stream while `r2tool storm` opens that many TLS
  1.3 handshakes per second on other CPUs (`R2_STORM_CPUS`), and the `_inline` variants set `tls_handshake_threads = 0`
  (handshakes on the worker thread) for the comparison. The p99 and p99.9 columns are the result.
* Load-aware pools: `pool_{lc,wrr}_s{8,64,200}` (`least_connections` and `weighted_round_robin` over N servers).
* `--lock-batch N` holds the shared benchmark lock across N consecutive lanes (re-entrant) so long runs are not starved.

## HTTP/3

`r2tool load --proto h3` is a quinn + h3 client: `--conns` QUIC connections, each on its own UDP socket (one quinn
endpoint per connection), `--streams` concurrent request streams per connection (cloned h3 `SendRequest`s, so one
worker task per stream), keep-alive connections, GET without a body or, with `--method POST --body-bytes N`, a request
body followed by FIN. `--new-conn` opens a new QUIC connection (full TLS 1.3 handshake, no resumption) for every request
and closes it afterwards. Warm-up, hooks, the 2xx/length validation (`--expect-status`, `--expect-len`) and the latency
columns are the ones of the other protocols; `connect_p50_us` is the QUIC handshake time. The client runs on
`R2_LOAD_CPUS`, the proxy on `R2_PROXY_CPUS`: the H3 listener runs on its own thread of the proxy process, so
the proxy CPU set must hold at least two CPUs.

The listener uses a `self_signed_development` certificate for `bench.example.com` (TLS 1.3, ALPN `h3`) like the TLS
lanes, and the client does not verify it for the same reason. HTTP/3 listeners require request buffering and a bounded
body on every route, so `gen_config.py --feature h3` sets both; `fixed_b1k` / `fixed_b64k` answer `/fixed` with that
body and leave out the proxy route; `h3_b1k_c8s4` and `h3_post1k_c8s4` keep it and proxy to the HTTP/1.1 origin
pool (they need a binary whose `http3` listener accepts such routes; an older one is reported as skipped). Readiness is a UDP probe (a datagram of an unknown QUIC version is answered with
Version Negotiation), not a TCP connect.

Limits: no certificate verification, no 0-RTT or session resumption, one client process with two runtime threads
(raise `--threads` for a large stream count), loopback only, quinn's default flow-control windows and idle timeout, and
`--reconnect-closed` only notices a connection that quinn already knows is closed.

```sh
export OXIROUTE_BIN=/path/to/oxiroute-before R2TOOL_BIN=/path/to/r2tool
benchmarks/r2-data-path/run.sh --lanes h3_fixed_b1k_c8s4,h3_fixed_b1k_c1s1 --reps 3 --out /tmp/h3-before
benchmarks/r2-data-path/run.sh --lanes h3_fixed_b64k_c8s4,h3_newconn_fixed_b1k --reps 3 --out /tmp/h3-before2
benchmarks/r2-data-path/run.sh --lanes h3_fixed_b1k_c8s4 --no-perf --sys --out /tmp/h3-before-sys
benchmarks/r2-data-path/run.sh --lanes h3_fixed_b1k_c8s4 --no-perf --profile --flat --out /tmp/h3-before-prof
```

Compare before and after with the same load tool, lane and CPU sets, on binaries built with the same profile:
`python3 benchmarks/r2-data-path/table.py /tmp/h3-after/results.json /tmp/h3-before/results.json` prints the
instruction delta per lane. `instr_per_req` counts every proxy thread (the H3 listener thread, runtime helpers); read the
spread columns before trusting a delta below 1%.

## Soak

```sh
python3 benchmarks/r2-data-path/soak.py --bin target/release/oxiroute --duration 60 --reload-at 30   # about 90 s
python3 benchmarks/r2-data-path/soak.py --duration 40 --reload-at 3 --reload-every 2 --mix h2         # reload stress
```

Everything is loopback, with a management token created for the run. It exits 0 only when no load run failed or hung,
each persistent connection was closed at most once per reload (`reconnects` of the load tool: an idle connection the
server closed, or a stream a graceful GOAWAY refused, is opened again, not counted as an error), the descriptors
returned to their level before the load (the pooled origin connections excluded), no retired generation was left,
the pool closed every idle origin connection when the origin stopped, every response class carried `x-request-id`,
and the process exited cleanly on SIGTERM. `r2tool load` gained `--pace-ms`, `--reconnect-closed` and the `wstls`
protocol for it.

## Limits

Forward-proxy mode and RTMP are not covered by this harness; HTTP/3 covers the reverse listener, answering itself or proxying to an HTTP/1.1 origin (no HTTP/3 or HTTP/2 origin lane).
