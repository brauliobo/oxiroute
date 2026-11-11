# OxiRoute 6.4.0

OxiRoute 6.4.0 makes the connection and protocol layers cheaper. Plain HTTP/1.1 requests of fixed-response and plain
proxy routes are served on a compiled fast lane and need about 30% to 55% fewer user-space instructions than in 6.3.1
(1 MiB responses 33% fewer), an idle keep-alive connection holds no session and no read buffer, HTTP/2 over TLS needs
10% to 23% fewer instructions and TLS handshakes run on their own threads, TCP relays and large static files move their
bytes in the kernel (`splice`, `sendfile`: 29% to 75% less CPU per request), and the vendored Pingora is rebased from 0.8.1
onto 0.9.1, which closes three advisories. Settings that were environment variables are configuration options (log
verbosity, the management token file, the audit log), in every source format. The release remains pre-alpha and retains
conservative compatibility claims.

## Summary

- **HTTP/1.1 fast lane.** A request of a fixed-response or redirect route or of a proxy route (header rules, forwarded
  headers, retries of `GET` and `HEAD`, queue classes, a TLS HTTP/1.1 origin, a service access log, a body that arrives
  with its head) is answered on the connection itself, without a session, with the same bytes, headers, framing and
  connection handling as before. Header rules and access lines cost about a third of what they did on the classic
  path. Rescue pools, cache, affinity, rewrites, gzip, streamed bodies and `Expect` take the classic path.
  `http_fast_lane = false` turns it off.
- **Idle connections.** A keep-alive connection that waits for its next request holds no session, no header buffer and
  no request future (10,000 idle connections: 17.5 kB to 2.2 kB of resident memory each with the allocator decay set to
  zero; 2,048 busy connections: 140 MB to 86 MB).
- **TLS and HTTP/2.** Handshakes run on `tls_handshake_threads` threads away from the workers; the HTTP/2 server is a
  purpose-built session (`oxiroute-h2`) instead of the `h2` crate: 10% to 23% fewer instructions per request.
- **HTTP/3.** Reverse `http3` listeners are served by a purpose-built HTTP/3 layer (`oxiroute-h3`) instead of the generic
  `h3` crate: 33% fewer instructions per 1 KiB request, one system call per request fewer, stricter RFC 9114 and 9204
  handling. A route of an `http3` listener can proxy to HTTP/1.1 and HTTP/2 origins like an `https` listener.
- **Kernel offload.** Bulk data of a TCP relay between plain sockets is moved with `splice`, and static files of 64 KiB or
  more over plain HTTP/1.1 are sent with `sendfile`.
- **Kernel TLS (on where the host has it).** A TLS profile sends large static files (about 256 KiB and up) over TLS 1.3
  with `sendfile` and the kernel's TLS offload: 46% to 51% fewer CPU cycles per 1 MiB response, 94% fewer user-space
  instructions. `ktls = "auto"` is the default and `"off"` turns it off. It needs the `tls` kernel module (`modprobe tls`,
  `/etc/modules-load.d/`) and kernel 6.5 or newer; without them nothing changes and the log says so once.
- **Pingora 0.9.1.** The vendored Pingora crates are rebased; the advisories RUSTSEC-2026-0388, RUSTSEC-2026-0390 and
  RUSTSEC-2026-0253 are no longer reported against the build.
- **rustls backend (opt-in).** `tls_profiles[].policy.backend = "rustls"` runs the handshakes and records of a profile's
  listeners with rustls and aws-lc-rs instead of OpenSSL; `openssl` stays the default. See
  [the rustls backend](developer/PERF_6_4_RUSTLS.md).
- **Configuration.** New options: `tls_profiles[].policy.backend`, `tls_profiles[].policy.ktls`, `tls_handshake_threads`, `http_fast_lane`, `connection_scheduling`,
  `connection_age_ms`, `log_filter`, `management.token_file` and the `audit` block.

## Behaviour changes to know before upgrading

- **Stricter request heads.** A request with two `Host` fields (identical or not), userinfo in `Host`, or an
  absolute-form target whose authority differs from `Host` is refused with 400 while its head is parsed; the access log
  has no record of it (before, the product refused it after routing and logged it). HTTP/2 requests with ambiguous
  authority fields are reset.
- **Content-Length.** Several identical `Content-Length` fields (or `5, 5`) are accepted and treated as one; conflicting
  or invalid values are refused, as before.
- **`bytes_sent`** of access records counts body bytes only (the response head was counted before).
- **Origin responses.** Obsolete line folding in an origin response head becomes one space; an upstream connection is no
  longer reused after a failed request write or an origin response that was not read to its end.
- **Fast lane.** An origin that answers `101 Switching Protocols` to a request that asked for no upgrade is a 502 on the
  lane (the classic path relayed it; requests with `Upgrade` never take the lane). `100 Continue` and other informational
  heads reach the client as before but the passive health records do not count them as responses. The lane does not watch
  the client socket while it waits for the origin: a client that leaves is noticed when the response is written. A request
  that arrives while a response still streams is read after the response (the classic path closes the connection).
- **HTTP/2 server.** A request whose stream the client reset before the proxy accepted it is not delivered (the proxy
  used to start the upstream request and cancel it a moment later). A request body that ends with an empty DATA frame
  carrying END_STREAM ends without an empty chunk. Graceful shutdown waits for the client's PING reply before the closing
  GOAWAY; a client that never answers is ended by the retirement drain or the idle timeout. Initial windows stay at the
  protocol default.
- **Idle keep-alive wait.** The idle wait of an HTTP/1 keep-alive connection ends at the listener's
  `keepalive_timeout_ms` and does not count against `request_timeout_ms`, which runs from the first byte of the next
  request. TLS connections wait as before (their idle memory did not change).
- **TCP relays** between plain TCP sockets move bulk data with `splice`: CPU per large message drops, user-space
  instructions per large message rise, small messages are unchanged. TLS, Unix-socket and PROXY-protocol-prefixed
  streams keep the copy path until the prefix is consumed. Up to two descriptors per bulk direction are open while it
  runs; past the user's pipe quota the relay keeps the copy loop.
- **Static files** of 64 KiB or more over plain HTTP/1.1 are sent with `sendfile` when the file is cached: the head and
  the body are two writes instead of one `writev`; TLS, HTTP/2, HTTP/3 and gzip-compressed responses are unchanged, as are
  file systems without `RWF_NOWAIT` other than tmpfs and ramfs. One more system call (`preadv2` probe) per large static
  request.
- **HTTP/3 listeners.** See [the HTTP/3 section](#http3) for the stricter request handling, the origins behind `http3`
  listeners and the configuration that is now refused.
- **Kernel TLS** (`tls_profiles[].policy.ktls`, `"auto"` default or `"off"`): with `"auto"` and a kernel that passes the
  startup probe, a TLS 1.3 AES-GCM HTTP/1.1 connection that sends its first large static file (about 256 KiB and up)
  is converted once to kernel TLS (one extra `KeyUpdate` record, then `sendfile`); every later response on that
  connection is encrypted by the kernel too, which costs 1% to 4% more instructions for 1 KiB responses than user space.
  Set `"off"` to keep TLS in user space. A host without the `tls` module behaves exactly as `"off"`. TLS 1.2,
  ChaCha20, HTTP/2, HTTP/3 and proxied bodies are unchanged. The profile checks the kernel when a generation starts and
  logs once (info) whether kernel TLS is active or why TLS stays in user space (the `tls` module is not loaded and an
  unprivileged service cannot load it: add `tls` to `/etc/modules-load.d/`; kernel older than 6.5; unsupported cipher). The vendored OpenSSL is now built with kernel TLS
  support. Details: [kernel TLS](developer/PERF_6_4_KTLS.md).
- **Timeouts** beyond a threshold use Tokio's timer instead of the clock thread (Pingora 0.9).
- **Environment variables** that now have a configuration option keep working as fallbacks that the configuration
  overrides: `OXIROUTE_MANAGEMENT_TOKEN_FILE` (`management.token_file`), `OXIROUTE_AUDIT_DIR` and `OXIROUTE_AUDIT_MAX_*`
  (`audit.*`), `RUST_LOG` (`log_filter`). Supervised workers run with a cleared environment and do not inherit `RUST_LOG`;
  set `log_filter` to change their verbosity. The log filter follows every reload.
- **Costs.** A 1 KiB static file and a WebSocket message cost about 5% to 21% more user-space instructions than in 6.3.1
  (the 0.9.1 base, and the lane's look at a request of a classic route in a service that also has lane routes; see the
  summary). Requests that use header rules, retries, cache, access logs or a TLS origin pay only the 0.9.1 base (1% to
  8%).

## Measured effect

User-space instructions per request from `benchmarks/r2-data-path`, 6.3.1 and 6.4 built the same way (shipped profile
without fat LTO, no profile-guided optimization), three repetitions, alternated on one shared host. The full table, the
CPU columns of the kernel-offload lanes, the handshake storm and the method are in
[the 6.4 performance summary](developer/PERF_6_4_SUMMARY.md).

| Lane | 6.3.1 | 6.4 | Change |
| --- | ---: | ---: | ---: |
| HTTP/1.1 proxied, 1 KiB | 53,035 | 36,572 | -31.0% |
| Fixed response | 22,638 | 10,361 | -54.2% |
| New connection per request | 69,817 | 49,641 | -28.9% |
| 2,048 keep-alive connections | 54,898 | 38,031 | -30.7% |
| Proxied 64 KiB / 1 MiB response | 56,697 / 83,551 | 39,824 / 55,638 | -29.8% / -33.4% |
| WebSocket echo 1 KiB | 15,473 | 16,259 | +5.1% |
| TLS 1.3 keep-alive, 1 KiB | 66,327 | 49,889 | -24.8% |
| HTTP/2 over TLS, 1 KiB / 64 KiB / POST 1 KiB | 87,314 / 183,825 / 99,397 | 74,001 / 142,127 / 89,261 | -15.2% / -22.7% / -10.2% |
| Static file 1 KiB / 1 MiB | 32,738 / 84,113 | 39,707 / 124,024 | +21.3% / +47.4% (CPU of 1 MiB: -65% to -75%) |
| TCP relay echo 1 KiB / 256 KiB | 8,919 / 38,240 | 9,156 / 59,489 | +2.7% / +55.6% (CPU of 256 KiB: -29%) |

## Request path and connections

Details: [fast lane](developer/PERF_6_4_REQUEST_PIPELINE_IMPL.md), [fast lane coverage](developer/PERF_6_4_LANE_COVERAGE_2.md), [footprint](developer/PERF_6_4_FOOTPRINT.md).

- The fast lane scans the head with `oxiroute-h1-head`, forwards a request (with its body when that arrived with the
  head) to an HTTP/1.1 server of a pool and relays the response; whatever it cannot prove equivalent (streamed or
  chunked bodies, `Expect`, rescue pools, cache, affinity, rewrites, buffering, HTTP/2 upstreams, gzip, pipelining,
  unusual heads) is handed to the classic session with the bytes already read, so nothing is read twice. Both data
  paths are compared on the same bytes by `tests/lane_differential`. A reload applies `http_fast_lane` to the new
  connections of the new generation.
- An idle HTTP/1 keep-alive connection awaits readiness of its socket, the shutdown watch and the keep-alive timeout, and
  builds its session when the next request arrives. The request future lives in a block the thread keeps for the next
  request, and the write buffer is taken from a per-thread pool when a response is staged.

## TLS and HTTP/2

Details: [TLS handshake offload and the HTTP/2 session](developer/PERF_6_4_TLS_H2.md).

- **TLS key exchange groups (opt-in).** `tls_profiles[].policy.key_exchange_groups` lists the groups a profile accepts
  (`x25519`, `secp256r1`, `secp384r1`, `x25519mlkem768`), in all configuration formats and the management UI. Absent
  keeps the default, which prefers the post-quantum hybrid group that browsers offer. `{ "x25519", "secp256r1" }`
  leaves the hybrid out: a browser handshake costs 29% fewer instructions and its key exchange is no longer protected
  against a future quantum computer. Details and the resumption options: [TLS options](developer/PERF_6_4_TLS_OPTIONS.md).
- Every TLS listener hands the accepted socket to `tls_handshake_threads` threads (default one per 8 CPUs, 1 to 4; `0`
  keeps handshakes on the workers). Certificate selection, SNI, ALPN, client certificates, session cache and tickets are
  unchanged. Handshakes waiting for a thread count against `max_connections` and the handshake timeout. A reload applies
  a new value to the new generation.
- `oxiroute-h2` is a frame codec, stream table, flow control and HPACK in one connection object, with regression tests for
  rapid reset, CONTINUATION floods, header-list and HPACK bombs, control-frame floods, stream limits and window
  overflow, a differential test against the `h2` crate and two fuzz targets (`hpack`, `h2_frames`). The `h2` crate stays
  for the HTTP/2 upstream client.

- **rustls backend.** A profile with `policy.backend = "rustls"` serves certificate selection, SNI, ALPN, TLS-ALPN-01,
  client certificates, session cache, tickets and certificate reloads as the OpenSSL backend does. It rejects
  `cipher_list` and `dh_parameters_path` (OpenSSL-only) at validation, offers only the TLS 1.3 and ECDHE AEAD suites
  (no DHE, CBC or RSA key exchange), answers a TLS 1.0 or 1.1 client with `handshake_failure` instead of
  `protocol_version`, reports TLS 1.2 cipher names in IANA form, and does not request a client certificate on a
  TLS-ALPN-01 challenge connection. `key_exchange_groups` restricts the groups of the rustls backend like those of
  OpenSSL (the hybrid group stays preferred when the list is absent); when a client sends no key share for a listed group,
  the retry names the client's first listed group, not the list's first as on OpenSSL. Upstream TLS, ACME and HTTP/3 keep their libraries. HTTP/3 now reads its server
  certificate from a key that is built once per certificate generation instead of on every handshake.

## HTTP/3

Details: [HTTP/3 layer and origins](developer/PERF_6_4_HTTP3.md).

- **Cheaper requests.** The reverse `http3` listener reads and answers requests with a purpose-built layer over QUIC
  streams (`oxiroute-h3`): a 1 KiB fixed response needs 33% fewer user-space instructions (70,279 to 46,672), a request on
  an otherwise idle connection 32% fewer (88,352 to 59,689), 64 KiB responses 5% fewer and a new QUIC connection per
  request 3% fewer (the handshake dominates), with 1.24 instead of 2.58 system calls per request. The upstream HTTP/3
  client and `forward_http3` still use the `h3` crate.
- **Origins behind `http3` listeners.** A proxy route of a service attached to an `http3` listener may use a pool that
  speaks HTTP/1.1 or HTTP/2 (plain TCP, TLS or Unix socket), with the pool's connection reuse, TLS policy, timeouts,
  retries, passive health and the route's header, path, response and cache policy. The response is buffered, bounded by
  the 64 MiB HTTP/3 response limit (a larger one is a 502), and HTTP/1.1 origin trailers are not forwarded. A service
  that uses an HTTP/1.1 or HTTP/2 pool can be shared between an `http3` listener and the TCP listeners; a service with an
  exact HTTP/3 pool still cannot.
- **Refused at validation.** A route of a service attached to an `http3` listener that configures a retry budget,
  rescue, pool selection, affinity, a queue class or queue headers is refused (the HTTP/3 path never applied them, and
  used to ignore them silently).
- **Malformed requests are reset.** A request with a connection-specific field (`Connection`, `Keep-Alive`,
  `Proxy-Connection`, `Upgrade`, `Transfer-Encoding`, or `TE` other than `trailers`), a repeated `:method` or a response
  pseudo-header field is reset with `H3_MESSAGE_ERROR` instead of being served or forwarded to the origin.
- **Oversized field sections.** A request head over 16 KiB is answered 431 with the listener's own error body (the `h3`
  crate answered with an empty one); every field counts 32 bytes besides its name and value, so hundreds of tiny fields
  are refused too.
- **Request bodies.** An empty `DATA` frame inside a request body reaches the origin instead of failing the request with
  400.
- **Protocol violations close the connection** as RFC 9114 and RFC 9204 mandate: a duplicate `SETTINGS` identifier
  (`H3_SETTINGS_ERROR`), a decreasing `MAX_PUSH_ID` or a `CANCEL_PUSH` that was never granted (`H3_ID_ERROR`), a push
  stream opened by the client (`H3_STREAM_CREATION_ERROR`), a QPACK acknowledgment or insert count increment for nothing
  (`QPACK_DECODER_STREAM_ERROR`), a QPACK table capacity above the announced 0 (`QPACK_ENCODER_STREAM_ERROR`) and an
  invalid Huffman string (`QPACK_DECOMPRESSION_FAILED`). Before, the `h3` crate ignored most of them.
- **QPACK without a dynamic table.** The listener announces a QPACK table capacity of 0: a client's request fields are
  sent as literals every time (cookies repeat on the wire); responses are unaffected.
- **Unsendable responses.** A response whose head cannot be sent aborts the stream with `H3_REQUEST_CANCELLED` instead of
  finishing it without a response.
- **Requests while a configuration generation drains** are refused with `H3_REQUEST_REJECTED`, so clients send them
  again on the new generation.
- **Retries.** A request that never left the proxy because the connection to the server was refused (any method) is sent
  to the next server of the pool; the `h3` crate path answered 502 for a POST.
- **Abuse limits.** A connection that keeps resetting requests (more than 1,000 within 10 seconds) is closed with
  `H3_EXCESSIVE_LOAD`; a `HEADERS` frame over four times the field section limit resets its stream before the payload is
  read.
- **Not changed.** 0-RTT and connection migration stay off, one endpoint still runs on one thread, and compression,
  upgrades and CONNECT-UDP over HTTP/3 stay unsupported.

## Kernel offload

Details: [kernel offload](developer/PERF_6_4_KERNEL_OFFLOAD.md). `splice` and `sendfile` are Linux-only; nothing is
configured. Large static files over TLS use `sendfile` too when the host has kernel TLS (`ktls = "auto"`, the default):
[kernel TLS](developer/PERF_6_4_KTLS.md) (1 MiB over TLS 1.3: 1.78M / 1.50M cycles per request in user space against
0.87M / 0.81M, user-space instructions 1,064k against 66k; 256 KiB: -30% to -32% cycles).

## Connection scheduling

Details: [connection scheduling](developer/PERF_6_4_CONNECTION_SCHEDULING.md).

- `connection_scheduling = "aged_per_worker"` gives every listener worker its own runtime, queue and I/O driver, places
  each new connection on the worker with the fewest open connections, and serves the newest ready connection first and
  the oldest once it has waited the age. The age is adaptive unless `connection_age_ms` fixes it: a multiple of the time
  of one pass over the worker's ready queue, from one pass at 64 or fewer ready connections to three at 256 or more.
- **The default stays `work_stealing`.** Measured on a shared host, per-worker mode raises the request rate 4% to 58% and
  lowers p99 for most lanes with two or more workers, but loses rate with one worker (0.6x to 0.8x at 512 and 2,048
  busy connections). Use it with `listener_workers` of two or more and many busy keep-alive connections.
- **HTTP/2 streams stay on their connection's worker** in per-worker mode (they used to run on Tokio's shared queue).
- A slow handler delays the other connections of its worker (p99.9 is the delay itself, 203 to 211 ms in the slow-handler
  lane); work stealing hides that.
- `worker_scaling = "dynamic"` (needs `aged_per_worker`) keeps only as many of a listener's `listener_workers` active as
  its load needs: new connections go to the active workers, idle kept-alive HTTP/1 connections move off released or
  crowded workers between two requests, and workers are activated within a few 20 ms ticks when the active ones saturate.
  It saves the wake-ups and kernel entries that a lightly loaded listener with many workers pays per request. TLS
  connections cannot move, so they keep spreading over every worker (no saving, no loss); cleartext HTTP/2 is not
  distinguished from HTTP/1 at accept time and may pack. The default stays `static`: the saving is measured only at 8
  or more workers under light load, and the p99 gate was missed by 0.5 ms at the edge. See
  [worker consolidation](developer/PERF_6_4_WORKER_CONSOLIDATION.md).
- With `aged_per_worker` a pooled upstream connection taken by another worker is registered with that worker's I/O
  driver, so its readiness wakes the thread that serves the request (it woke the thread that had pooled it).
- The idle upstream connection pool has one stripe per CPU (at least eight) instead of eight, so listeners with more than
  eight workers no longer share stripe locks.

## Release binaries

Details: [binary-level work](developer/PERF_6_4_BINARY_LEVEL.md), [releasing](developer/RELEASING.md).

- **Wider profile-guided training.** The default training mix is the ten 6.3 lanes (weight 4) plus fourteen feature lanes
  (HTTP/2 64 KiB and POST, WebSocket, TCP relay, cache hit, gzip, 1 MiB, TLS 1 MiB and 1.2, all 6.0 features,
  least-connections pool, large headers, forward proxy). Core lanes stay within 3%; lanes absent from the old mix gain
  12% to 17%.
- **Allocator configuration in the binary.** The jemalloc thread-cache sizes are embedded
  (`tcache_nslots_small_min:400,tcache_nslots_small_max:2000,tcache_nslots_large:400,tcache_max:131072`): 2% to 6% fewer
  instructions on HTTP/2 and 64 KiB uploads. `_RJEM_MALLOC_CONF` still overrides every key at run time.
- **BOLT stage (optional).** The release script can rewrite the binary with BOLT (`BOLT=0` skips it) and runs the whole
  workspace tests against the rewritten bytes. On the measuring host it gave no cycle gain that survives the noise; it is
  a release decision whether to publish BOLT output.
- **C dependencies at `-O3`.** The release build compiles the vendored OpenSSL and the other C libraries with `-O3`:
  7% to 10% fewer instructions per TLS handshake, per-request cost unchanged. A separate `x86-64-v3` artefact
  (`oxiroute-VERSION-TARGET-x86-64-v3.tar.gz`, AVX2 and BMI2 CPUs only, started manually) saves 2.5% to 4% more.
- **Abort flavour.** A separate artefact (`oxiroute-VERSION-TARGET-abort.tar.gz`) is built with `panic = "abort"` (3% to
  7% fewer instructions): the first panic ends the worker process, the supervisor fails the generation as for a killed
  worker, and recorder panics are no longer reported as `worker_panicked`. Arch, the source archive and the default
  artefact keep unwinding.

## Pingora 0.9.1

Details: [rebase](developer/PERF_6_4_PINGORA_REBASE.md). The vendored `pingora-core`, `pingora-proxy`, `pingora-timeout`
and `pingora-load-balancing` are rebased from 0.8.1 onto the published 0.9.1; `pingora-http` is vendored (pristine plus
one accessor) and `pingora-rustls` is no longer vendored. The delta records are in each crate's
`README.oxiroute.md`.

## Security

- The audit no longer reports RUSTSEC-2026-0388 (HTTP/2 `:authority` that differs from `Host`; the product already refused
  it), RUSTSEC-2026-0390 (hop-by-hop request fields, fixed in the product in 6.3.1) or RUSTSEC-2026-0253 (`lru`). The
  `unsafe-libyaml` warning remains.

## Configuration: environment variables are options

Details: [environment variables and the configuration](developer/ENV_TO_CONFIG.md). Precedence: the configuration key,
then the environment variable, then the default. The fallbacks log no warning.

## New options

All are optional, available in KDL, restricted Lua, UCI and HOCON, in the management UI field registry and in
`coverage/canonical.json`. An absent option changes nothing.

| Option | Default | Range | Reload |
| --- | --- | --- | --- |
| `tls_profiles[].policy.key_exchange_groups` | backend default list (hybrid group preferred) | nonempty list of `x25519`, `secp256r1`, `secp384r1`, `x25519mlkem768`; hybrid-only needs `min_version` 1.3 | new generation |
| `tls_handshake_threads` | one per 8 CPUs, 1 to 4 | 0 to 64 (`0`: handshakes on the workers) | new generation |
| `http_fast_lane` | `true` | boolean | new connections of the new generation |
| `connection_scheduling` | `work_stealing` | `work_stealing`, `aged_per_worker` | new generation |
| `connection_age_ms` | adaptive: a multiple of the time of one pass over the ready queue, 1 to 3 passes (200 us to 1 s) | 1 to 1000 fixes it, only with `aged_per_worker` | new generation |
| `worker_scaling` | `static` | `static`, `dynamic` (needs `aged_per_worker`) | new generation |
| `min_active_workers` | 1 | 1 to `listener_workers`, only with `dynamic` | new generation |
| `target_busy_percent` | 40 | 10 to 90, only with `dynamic` | new generation |
| `scale_down_delay_ms` | 5000 | 100 to 3600000, only with `dynamic` | new generation |
| `log_filter` | `RUST_LOG`, then errors only | `RUST_LOG` syntax without regex filters, at most 1024 bytes | every accepted reload |
| `management.token_file` | `OXIROUTE_MANAGEMENT_TOKEN_FILE` | absolute normalized path of a regular `0400` or `0600` file | read when a generation starts |
| `audit.dir` | `audit` next to the token file, else `OXIROUTE_AUDIT_DIR`, else in memory | absolute directory path | read when a generation starts |
| `audit.max_records` | 10000 | 1 to 100000 | same |
| `audit.max_record_bytes` | 16384 | 1 to 65536, at most `max_file_bytes` | same |
| `audit.max_file_bytes` | 1048576 | 1 to 16777216 | same |
| `audit.max_total_bytes` | 8388608 | 1 to 134217728, at least `max_file_bytes` | same |
| `audit.max_rotated_files` | 7 | 1 to 7 | same |

## Metrics

- `oxiroute_connection_scheduling_info` (`mode`, `age_ms`) with value 1, and `oxiroute_aged_queue_tasks`,
  `oxiroute_aged_queue_ready`, `oxiroute_aged_queue_oldest_ready_seconds`, `oxiroute_aged_queue_age_seconds`,
  `oxiroute_aged_queue_polls_total` and `oxiroute_aged_queue_aged_polls_total`, the last six only with
  `connection_scheduling = aged_per_worker`. See [operations](OPERATIONS.md).
- `oxiroute_worker_scaling_info` (`mode`), and with `worker_scaling = dynamic` `oxiroute_worker_scaling_workers`,
  `oxiroute_worker_scaling_active_workers` and `oxiroute_worker_scaling_relocations_total` per listener runtime.

## Fixes

- With per-worker (non-stealing) runtimes, a replaced generation's workers left their runtime in thread-local state, so the
  next generation's listener tasks could land on a dead runtime; reachable only with
  `connection_scheduling = aged_per_worker`.
- On a route with a cache, a request the origin revalidated (a 304, or a stale answer after an upstream failure) was
  answered correctly, but the proxy closed an HTTP/1.1 client connection afterwards, so the next request on a keep-alive
  connection was reset (the second revalidation of an always-stale entry, plain or TLS). The connection now stays open
  for any number of revalidations, with the HTTP/1.1 fast lane on or off, and the request no longer logs an error line.
  HTTP/2 clients were not affected.

## Upgrade notes

- No configuration change is needed. A 6.3.1 configuration loads unchanged; the new options are optional.
- A configuration whose `http3` service has a route with a retry budget, rescue, pool selection, affinity or queue
  class no longer validates; remove the setting (it had no effect on HTTP/3).
- Restart or reload as usual. `http_fast_lane = false` returns every request to the classic path.
- Operators who set `RUST_LOG`, `OXIROUTE_AUDIT_*` or `OXIROUTE_MANAGEMENT_TOKEN_FILE` keep working; set the options when
  convenient. Supervised workers do not inherit `RUST_LOG`: use `log_filter`.

## Vendored Pingora

`pingora-core`: the HTTP/1 connection-level front (`Http1Front`, the fast lane's hook), the parked idle wait and pooled
request future, rendered response head functions, the raw response head read and raw request write, the HTTP/2 session
ported onto `oxiroute-h2`, per-worker connection placement and `ServerConf::connection_age_ms`, the stream buffer
accessors and the `sendfile` body write. `pingora-proxy`: the shared HTTP/1 connector and the borrowed request future.
`pingora-runtime`: aged queues. See each crate's `README.oxiroute.md`.
