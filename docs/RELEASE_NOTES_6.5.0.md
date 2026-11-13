# OxiRoute 6.5.0 (Unreleased)

## Memory

- An idle plain WebSocket connection costs 16.6 kB of resident memory instead of 31.6 kB (nginx 18.4 kB, HAProxy 6.3 kB), an
  idle WebSocket over TLS 38.6 kB instead of 53.5 kB (nginx 60.1 kB, HAProxy 30.9 kB) and an idle HTTP/1 keep-alive
  connection 11.9 kB instead of 17.8 kB (nginx 9.2 kB, HAProxy 11.2 kB). An upgraded connection no longer keeps the
  request's state machine: the handshake ends the request, the connection continues as a small tunnel and the hooks and
  the access log run when it ends.
- A close-delimited HTTP/1 body (and so an upgraded connection) no longer allocates its 128 KiB chunk buffer before it
  reads.
- jemalloc returns freed pages from a background thread (`background_thread:true`). After connection bursts and reloads
  the process settles at about half of the resident memory it used to keep (97.9 MB to 48.8 MB in the compressed churn of
  `benchmarks/r65-memory/churn.py`). `_RJEM_MALLOC_CONF` still overrides the setting.
- A connection's task and request context are smaller (the connection counter wraps the future without copying it,
  the cache state of a request is boxed until a cacheable request needs it).
- Instructions per request on the fast lane, HTTP/1 1 MiB, HTTP/1 over TLS and HTTP/2 over TLS are within +0.7% of 6.4.0.

## TLS

- **The rustls backend accepts every TLS policy option.** `policy.cipher_list` and `policy.dh_parameters_path` were
  refused with `backend = "rustls"`; they are accepted now, so an nginx import or a certbot-style profile can switch
  backends by changing one line. `cipher_list` keeps its OpenSSL meaning: the string is evaluated by OpenSSL and the TLS
  1.2 suites the rustls backend implements (ECDHE with AES-GCM or ChaCha20-Poly1305) are offered in that order. Suites it
  does not implement (DHE, CBC, RSA key exchange) are named once in the log; a profile whose list leaves no TLS 1.2 suite
  fails to start unless `min_version` is `1.3`. `dh_parameters_path` is accepted and ignored with a warning (no
  finite-field DHE).
- **Session resumption behaves the same on both backends.** A full TLS 1.3 handshake sends one session ticket (the
  OpenSSL backend sent two, the rustls backend four; each further ticket costs a full handshake of server work), and none
  when the profile has neither tickets nor a cache. The default session timeout of the rustls backend is 7200 s as on
  OpenSSL (it was 300 s), and the ticket lifetime hint carries the `session_timeout_seconds` of the profile on both.
  The rustls backend keeps one session state per profile, so a session resumes on every listener of the profile.
- **TLS 1.2 cipher names in the access-log digest and in variables are the OpenSSL names on the rustls backend**
  (`ECDHE-ECDSA-AES128-GCM-SHA256` instead of `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256`), as on the OpenSSL backend.
- **Fixed: a proxied WebSocket held about 117 KB more memory on the rustls backend than on the OpenSSL backend.** Every
  read zero-filled the whole spare buffer of the caller. Idle TLS WebSocket connections now hold 38 to 40 KB each on the
  rustls backend (170 KB before; 53 KB on OpenSSL).
- The rustls backend no longer zero-fills the read buffer on every read: -1% to -2% instructions per keep-alive,
  HTTP/2 and static request over TLS, and lower cycles.
- SNI lookups no longer allocate for lower-case names.

## HTTP/2 and HTTP/3

- The HTTP/2 lane serves services with an access log; the line is the classic path's, with `protocol` `h2`.
- The HTTP/2 lane serves requests whose body has no `content-length` (the origin gets it chunked, as the classic
  path sends it) and requests with an `Expect` field (forwarded as a plain field). A body without a length over the
  route's request body limit is refused with `413` as soon as the limit is passed.
- Reverse HTTP/3 listeners run one QUIC endpoint per worker when `listener_workers` is above 1 (`SO_REUSEPORT`
  sockets on the listener's address); one worker behaves as before.
- Reverse HTTP/3 sends an origin answer that has no declared length, or a declared length above 256 KiB, as it
  arrives instead of buffering it: such an answer has no `content-length` unless the origin gave one, is not bound by
  the 64 MiB response limit, and a failure after the head resets the stream. Cache fills and `HEAD` stay buffered.
- An idle HTTP/2 connection holds less memory: the HPACK tables size their buffers to their entries and a connection
  that has closed its streams gives its stream slots back.

## Request floor (HTTP/1 lane)

- A proxied HTTP/1.1 keep-alive request on the fast lane needs about 6% to 7% fewer user instructions (shipping build:
  27.2k to 25.5k for a 1 KiB response, 28.9k to 27.2k with header rules, 28.2k to 26.6k for a 1 KiB POST, 8.0k to 7.5k
  for a direct response) and makes 4 fewer allocator calls. The direct response is now ahead of nginx in rate and CPU
  per request; the proxied rows still trail it by 10% to 16% in rate on the same host. Details:
  [results](developer/PERF_NEXT_REQUEST_FLOOR_RESULTS.md).
- Deadlines of the HTTP/1 and HTTP/2 lanes reuse timers kept per worker thread; the lane applies the origin read limits
  itself. No behaviour change.
- New build feature `alloc-count` (test and benchmark builds only) and the `lane_allocations` test that holds the
  allocator calls per request of each route shape to a ceiling.
- Benchmark tooling: `vs_syscalls.py` (system calls per request of OxiRoute, nginx and HAProxy; OxiRoute makes the
  fewest on the proxied rows), `source_profile.py`, `run.py --alloc-internal`.

## Connections

- **A new connection costs about 1.2k fewer user-space instructions (-3.5%, 34.6k to 33.4k for a proxied 1 KiB request on a connection per
  request)** and 12 kB less copying: the connection future is boxed once where it is spawned, the layers of an HTTP listener hand
  each other their futures instead of a box each, and the accept loop is one loop for every listener. Behaviour, limits, drain
  and socket options are unchanged.
- No scheduling default changed. Measured against nginx and HAProxy on one worker (`docs/developer/PERF_NEXT_CONNECTIONS_RESULTS.md`):
  `connection_scheduling = "aged_per_worker"` serves 1.2x to 1.5x of nginx's rate at 1,024 and 2,048 connections with a p99 of 1.6x and 1.4x
  nginx's (work stealing: 3.3x to 5.5x), at 0.7x of work stealing's rate at 1,024 connections on one worker (1.0x at 2,048 and at 256);
  from two workers up the two are within noise. It stays opt-in (and `auto` selects it from two workers).

## Tools

- `benchmarks/r65-memory/` (churn, time to first byte of a parked connection, live libc allocation tracer) and
  `benchmarks/scripts/memsample.py` measure the memory of a proxy including its kernel socket memory;
  `benchmarks/scripts/vs_nginx_haproxy.py` has the scenarios `idle_h1`, `idle_h1tls` and `idle_h2tls` next to `idle_ws` and
  `idle_wstls` and reports anonymous memory, socket buffers and kernel socket structures per connection.
- The `heap-profile` cargo feature of `oxiroute` builds jemalloc with profiling and statistics, for memory investigations
  only.
- `benchmarks/scripts/vs_nginx_haproxy.py` compares the TLS rows: TLS 1.2 ECDHE handshakes, resumption by ticket and by
  session id, client certificates, `VS_TLS_VARIANTS` for OxiRoute columns (OpenSSL, rustls, inline or offloaded
  handshakes), and `vs_tls_tables.py` renders them. `r2tool` can present a client certificate; the R2 `--mem` mode runs on
  hosts without the `tls` kernel module.
- `benchmarks/scripts/vs_nginx_haproxy.py`: the connection-count scenarios cover 64, 256, 512, 1,024 and 2,048 connections and HAProxy;
  `VS_PERF_EXTRA` adds counters such as `cache-misses:u` per request to every run; `VS_ALT_BINS` builds can carry their own environment
  (`label=/path@KEY=VALUE`); runs disturbed on the SMT siblings of the proxy CPUs are discarded (`--sibling-max`).
