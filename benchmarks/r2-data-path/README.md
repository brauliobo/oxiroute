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
| `h3_impair.py` | HTTP/3 and TCP+TLS of OxiRoute and nginx over an impaired link (netem delay, loss and rate in an unprivileged user and network namespace), or on a plain namespace loopback: download time, goodput and server CPU per MiB, or the 1 KiB request rate. |
| `tool/` | `r2tool`: in-tree upstream (0 B, 1 KiB, 64 KiB, 1 MiB, chunked, slow, sink, upgrade echo, TCP echo) and a fixed-count load generator (h1, h1+TLS, h2c, h2+TLS, HTTP/3, WebSocket-style upgrade, TCP echo; keep-alive or new connection per request; header padding; request bodies; p50/p99/p99.9). `alloc_count.c` is an `LD_PRELOAD` shim counting allocator and `clock_gettime` calls. |
| `table.py` | Compact table of a `results.json`, optionally with instruction deltas against a previous run; `table.py --markdown DIR LANE...` prints the release comparison table of `compare.sh`. |
| `compare.sh` | Instructions per request of an older release (`--base`, run with `--baseline` so no option it does not know is generated), the new binary with the fast lane off and the new binary, one lane after the other; scratch on disk under `~/.local/state/oxiroute-bench`. |
| `profile_summary.py` | Component and symbol summary of a `--profile` `perf.data`. |
| `soak.py` | Mixed-traffic soak of one process (keep-alive, new connection, POST, 1 MiB upload, slow response, WebSocket-style upgrade over plain and TLS, TLS HTTP/1.1, HTTP/2, TCP echo, paced) with RSS, descriptor and thread samples, a configuration reload through the management API in the middle, an origin stop and a SIGTERM at the end. |
| `memcpy_profile.py` | Copies of 48 bytes or more per request, by caller (`tool/memcpy_profile.c` preloaded): shows async state machines moved to the heap or into another future. |
| `reload_memory.py` | PSS of OxiRoute, nginx and HAProxy after 1, 10 and 50 configuration reloads of a 1,001-route configuration, one worker each (management API, `SIGHUP`, `SIGUSR2`); needs `nginx` and `haproxy` on `PATH` and takes `--bin`, `--tool`, `--counts`, `--proxy-cpus`, `--origin-cpus`. |
| `heapsum.py` | Live bytes of a jemalloc heap profile dump grouped by allocation site (what an idle connection holds); its header says how to take the profile. |

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
* `--mem`: RSS per idle keep-alive connection, sampled while the connections are open (`rss_bytes_per_conn`) and after the
  load tool closed them (`rss_after_close_bytes_per_conn`: what the allocator keeps, not a footprint).
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
alloc_bytes_per_req clock_gettime_per_req`, `heap_live_bytes_per_conn rss_bytes_per_conn rss_after_close_bytes_per_conn`.

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
  tls_hs_resume_{cache_tls12,cache_tls13,tickets_tls13,client_only} tls_hs_browser{,_kx}_tls13 tls_h1_b1m h2_b1k_c4s16 h2_b64k_c4s16 h2_b0_c1s1 h2_post1k_c4s16 h2_post1m_c4s4`; the `_oldwin` variants (`h2_post1m_c4s4_oldwin`,
  `h2_post1k_c4s16_oldwin`) set both HTTP/2 windows to the 65,535 of releases before 6.4 for the A/B.
* `tls_hs_browser_tls13` and `tls_hs_browser_kx_tls13` use a client hello like a browser's (`--tls-groups browser`: shares for
  `X25519MLKEM768` and `X25519`); the second sets `key_exchange_groups = { "x25519", "secp256r1" }` on the profile
  (`tls_kx` feature), which drops the hybrid group.
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

## HTTP/3 on impaired links

`h3_impair.py` re-executes itself under `unshare --user --map-root-user --net`, so it needs no privileges and changes
nothing on the host: inside the namespace it raises the loopback device with a 1500-byte MTU, adds a `netem` qdisc
(delay of half the round trip in each direction, loss in each direction, optional `rate` and queue length) and starts
the server, the client and the qdisc, all of which end with the namespace. It needs `unshare`, `tc`, `ip`, `ethtool`,
`openssl`, `curl` built with HTTP/3 (ngtcp2) and, for the nginx rows, an nginx built with `http_v3`.

```sh
export OXIROUTE_BIN=/path/to/oxiroute R2TOOL_BIN=/path/to/r2tool
benchmarks/r2-data-path/h3_impair.py download --server oxiroute --proto h3 --rtt-ms 40 --loss-pct 0.5 --size-mib 10 --runs 5 \
  --set http3_segmentation_offload=false --set 'http3_congestion_control="bbr"'
benchmarks/r2-data-path/h3_impair.py download --server nginx --proto h3 --rtt-ms 100 --loss-pct 1 --nginx-stream-buffer 4m
benchmarks/r2-data-path/h3_impair.py small --server oxiroute --server-cpus 8-9 --client-cpus 12-15
```

* Every run is one fresh connection that downloads one static file (at most 64 MiB, the H3 response bound) with curl;
  `--client r2tool` uses the quinn client instead. The record holds p50, p99 (with five runs, the slowest run), goodput
  (size over p50), the server's CPU milliseconds per MiB (user plus system of every server process, from `/proc`),
  the bytes, packets and drops the qdisc saw, and the `some avg10` CPU pressure before and after the measured section.
* UDP and TCP segmentation offload must be off on an impaired link: `netem` drops one whole segmented batch, so
  loss would arrive in bursts of a whole batch (up to 64 KiB). The script turns TSO, GSO and GRO off on the namespace loopback
  for TCP; for QUIC set `http3_segmentation_offload = false` (`--set`). `--keep-offload` leaves the loopback features on.
* `small` and every run without impairment hold `/tmp/oxiroute-bench.lock` while they measure; use the pinned CPU
  sets of the harness (`--server-cpus 8-9 --client-cpus 12-15`). Impaired runs take `--server-cpus` and `--client-cpus`
  from outside those sets.
* The server CPU counters tick at 10 ms: a measured section has to burn several hundred milliseconds to give a stable
  CPU per MiB or per request (`--requests 200000` for `small`).

## BBR measurements

The measurements behind the `http3_congestion_control` default (`bbr`, with `cubic` selectable) are in `fairness/`. Raw
records and the Markdown table of every fairness cell are in `fairness/results/`.

```sh
export OXIROUTE_BIN=/path/to/oxiroute R2TOOL_BIN=/path/to/r2tool
benchmarks/r2-data-path/fairness/gate.sh out/gate.jsonl 3
benchmarks/r2-data-path/fairness/matrix.sh out/fairness.jsonl 3 16-21
python3 benchmarks/r2-data-path/fairness/summarize.py out/fairness.jsonl
```

* `gate.sh` runs `h3_impair.py` for cubic and bbr alternately: 10 MiB downloads at 40 ms with 0.5% loss, at 100 ms with 1%
  loss and at 40 ms without loss, then on the plain loopback the 48 MiB download and the 1 KiB request rate (`gate.jsonl`).
* `lane.py` (driven by `matrix.sh`) builds two network namespaces in one unprivileged user namespace, joined by a veth pair:
  the servers on one side, one curl client per flow on the other. Data crosses a `netem` delay line and a `tbf` queue of 0.5,
  1 or 4 times the bandwidth-delay product at 100 Mbit/s; acknowledgements cross a second delay line. Flows are an
  OxiRoute HTTP/3 listener with `bbr`, one with `cubic` (UDP segmentation offload off, so the counters see single
  packets), and an HTTP/2 over TLS listener (kernel cubic). Each flow downloads a 48 MiB file repeatedly; after 15 s of
  warmup a 25 s window gives per flow the goodput share, loss (packets entering the queue and not leaving it; TCP:
  retransmitted segments), CPU per MiB delivered, the queue occupancy and Jain's index. Every cell has three runs; the
  table in `summary.md` holds the medians. No privileges and no change to the host.
* Every record carries the `some avg10` CPU pressure before and after the run (at most 4.4% during the matrix).

Medians over three runs, rounded; `bbr` and `cubic` alone are the same listener with the setting changed.

| Measurement | cubic | bbr |
|---|---|---|
| 10 MiB, 40 ms, 0.5% loss | 15.0 s | 0.37 s |
| 10 MiB, 100 ms, 1% loss | 61.8 s | 0.80 s |
| 10 MiB, 40 ms, no loss | 0.55 s | 0.31 s |
| LAN 48 MiB download | 1,100 MiB/s, 0.96 ms CPU per MiB | 226 MiB/s, 2.9 ms CPU per MiB |
| LAN 1 KiB, 8 connections of 4 streams | 237,000 per s, p99 263 us, 6.2 us CPU per request | 247,000 per s, p99 219 us, 6.0 us CPU per request |
| Shared 100 Mbit/s, bbr against cubic flow | cubic keeps 0.4% to 0.7% | bbr takes 99.3% to 99.7%, Jain 0.50 |
| Shared 100 Mbit/s, bbr against TCP flow | TCP keeps 0.3% to 0.5% | bbr takes 99.5% to 99.7%, Jain 0.50 |
| Shared 100 Mbit/s, cubic against TCP flow | Jain 0.90 to 0.98 (QUIC share 37% to 60%) | |
| Alone through the 100 Mbit/s bottleneck | loss under 0.1%, 7.1 to 12.1 ms CPU per MiB | loss 28% to 70%, 10.1 to 17.3 ms CPU per MiB (0.8 to 2.0 times, median 1.7) |

Why `bbr` behaves this way, from quinn 0.11's controller (`quinn-proto` `congestion/bbr`, which exposes only
`initial_window`): the pacer of quinn-proto is driven by the congestion window (1.25 times the window per smoothed round
trip), not by BBR's own pacing rate, and the window is 2 times the estimated bandwidth-delay product plus the largest
ack aggregation seen. With an empty queue that sends at about 2.5 times the estimated bandwidth, so a buffer under about
a bandwidth-delay product overflows continuously (the loss above) and a competing loss-based flow, which backs off at
every loss, is starved. On a LAN the estimated bandwidth-delay product (about 250 MB/s times a 15 us minimum round trip,
under 4 KB) is below the controller's fixed floor of 4 datagrams: the window sits at 5,808 bytes for the whole transfer
and one system call carries 3 datagrams (`fairness/results/connection_stats/`, sampled every 100 ms from a scratch
build that logged `Connection::stats`), against a window that grows without bound with `cubic`. The floor, the gains and
the pacing source are constants of the library; nothing in our transport configuration changes them (the one exposed
parameter, the initial window, only matters before the first bandwidth samples and made no difference at 32 packets).

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
