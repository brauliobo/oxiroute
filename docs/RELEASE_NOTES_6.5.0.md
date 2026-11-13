# OxiRoute 6.5.0

Released 2026-11-13.

OxiRoute 6.5.0 makes open connections cheaper, closes the gap of the rustls TLS backend, stops serving requests nobody
waits for, and tightens the test suite. The release remains pre-alpha and retains conservative compatibility claims. No
configuration change is needed and no default changed.

## Headlines

- **Memory per idle connection is about halved.** Resident memory per idle connection against 6.4.0, nginx and
  HAProxy in one invocation: plain WebSocket 13.1 kB (6.4.0 31.6 kB, nginx 18.4 kB, HAProxy 6.1 kB), WebSocket over TLS
  35.1 kB (53.6, 60.1, 31.5), HTTP/1 keep-alive 8.7 kB (15.0, 9.6, 11.6), HTTP/1 over TLS 29.9 kB (36.2, 34.6, 35.6) and
  HTTP/2 over TLS 41.5 kB (50.2, 50.3, 46.0). Four reloads of a 1,000-route configuration grow the process by 16.2 MB
  instead of 47.8 MB, and the process settles at about half of the resident memory it used to keep after connection
  bursts.
- **HTTP/2 over TLS serves more requests per second and costs less CPU.** 1 KiB responses on one worker: 86.1k requests
  per second against 65.1k for 6.4.0 in the first invocation (nginx 87.1k, HAProxy 78.7k), user+sys CPU 7.8 us per
  request against 8.3 us, p99 2.3 ms against 3.3 ms. The noisier rerun measured 1.03x of 6.4.0's rate and the same CPU
  ratio (0.94x); the host was shared and loaded, so read the CPU column and the ratios inside one invocation first.
  Summary.
- **A request whose client has left is dropped from the queue.** A request waiting for an origin when its client closes
  the connection (or resets the stream) leaves the pool queue at once and never reaches the origin. The new counter
  `oxiroute_pool_queue_cancellations_total{pool}` counts them.
- **The rustls TLS backend accepts every TLS policy option** (`cipher_list`, `dh_parameters_path`) and behaves like
  OpenSSL for session tickets, session timeout and TLS 1.2 cipher names, so a profile switches backends by changing one
  line. An idle WebSocket over TLS holds 22.6 kB on rustls (35.0 kB on OpenSSL).
- **Reverse HTTP/3 scales across workers and streams long answers.** `listener_workers` above 1 runs one QUIC endpoint per
  worker, and an origin answer without a declared length, or above 256 KiB, reaches the client as it arrives.
- **The proxied request costs 5% to 10% fewer user instructions** than in 6.4.0 (HTTP/1 1 KiB 27.1k to 25.2k, new
  connection 33.2k to 29.9k, TLS keep-alive 37.7k to 35.8k, direct response 8.0k to 7.4k); user+sys CPU per request is
  0.91x to 0.98x of 6.4.0 on the proxied, TLS keep-alive, HTTP/2 and direct rows. The direct response is level with nginx
  in rate and CPU.
- **Flaky tests are fixed at their cause.** The supervisor replacement tests, the lane differential corpus and a pooled
  timer test no longer depend on how late a loaded host schedules them (see [Tests](#tests)); no assertion was loosened.

## Behaviour changes to know before upgrading

- **Abandoned requests leave the queue.** A request that waits for a pool slot, a retry delay or an origin connection
  is dropped when its client closes the connection. Before, it stayed queued, was sent to the origin when a slot freed
  up and ran there for nobody. An HTTP/1.1 client that half-closes its connection while its request is queued is treated
  as gone. A request that was sent to the origin already is handled as before. See [Fixes](#fixes).
- **TLS session tickets.** A full TLS 1.3 handshake sends one session ticket on both backends (OpenSSL sent two, rustls
  four) and none when the profile has neither tickets nor a cache. The rustls default session timeout is 7200 s, as on
  OpenSSL (it was 300 s).
- **TLS 1.2 cipher names on rustls** in the access-log digest and in variables are the OpenSSL names.
- **rustls accepts `cipher_list` and `dh_parameters_path`** (validation refused them before). The cipher list keeps its
  OpenSSL meaning; suites rustls does not implement are named once in the log, and a list that leaves no TLS 1.2 suite
  fails to start unless `min_version` is `1.3`.
- **Reverse HTTP/3.** With `listener_workers` above 1 the listener binds one `SO_REUSEPORT` socket per worker. A response
  without a declared length, or above 256 KiB, is streamed: it has no `content-length` unless the origin gave one, is not
  bound by the 64 MiB response limit, and a failure after the head resets the stream. Cache fills and `HEAD` stay
  buffered.
- **HTTP/2 lane.** It now serves services with an access log (line identical to the classic path, `protocol` `h2`),
  requests with a body but no `content-length` (chunked to the origin; `413` once the route's limit is passed) and
  requests with `Expect` (forwarded as a plain field).
- **Allocator.** jemalloc returns freed pages from a background thread that starts once the process has adopted its
  control endpoint. A `background_thread` key in `_RJEM_MALLOC_CONF` still decides for itself.
- **Cached responses** own their header values: 3.1 kB instead of 6.1 kB per cached 1 KiB response.
- **Routing internals.** A request's route is selected by position from the host as written instead of a lookup by route
  identity; the routes chosen are the same.
- **Aged scheduler fairness.** With `connection_scheduling = "aged_per_worker"`, a flood of always-ready connections can
  no longer delay a lighter connection beyond the age.

## Decisions left to operators

None of these defaults changed in 6.5.0. The measurements are in the summary.

| Option | Default in 6.5.0 | State |
| --- | --- | --- |
| `backend` (per TLS profile) | `openssl` | **Not flipped to rustls.** rustls costs 58% fewer instructions per full handshake and 3% to 14% fewer per keep-alive or HTTP/2 request, with every option, resumption and log name at parity. It has no kernel TLS for large downloads, no FIPS module and no DHE-only TLS 1.2 clients. Set `backend = "rustls"` per profile to opt in. See the TLS recommendation. |
| `connection_scheduling` | `work_stealing` when omitted | **Aged scheduler stays opt-in.** `aged_per_worker` (and `auto`, which selects it from two workers) serves 1.2x to 1.5x of nginx's rate at 1,024 and 2,048 connections on one worker with a p99 of 1.6x and 1.4x nginx's (work stealing: 3.3x to 5.5x), at 0.7x of work stealing's rate at 1,024 connections. From two workers up the orders are within noise. |
| `worker_scaling` | `static` | `dynamic` saves wake-ups only at 8 or more workers under light load; unchanged. |
| `h2_initial_stream_window`, `h2_initial_connection_window` | 1 MiB and 8 MiB | Unchanged; see the window note for the memory bound per listener. |

## Known limits

- **A request already sent to the origin is cancelled only where the path watches its client.** The HTTP/1.1 classic
  path and HTTP/2 (classic and stream lane) cancel it early. The HTTP/1.1 fast lane does not while it waits for the
  response head of a request without a body (a request with a body notices through the upload), and HTTP/3 does not after
  the route is selected; there the request finishes at the origin and the departure shows when the response is written.
  Cancellation of a queued request covers every path.
- **A chunked request with a bad chunk size** gets the same `400` and a closed connection on the fast lane and the
  classic path, but the lane has already forwarded the request head to the origin when it reads the bad size, and the
  classic path forwards nothing. The amount of a request forwarded before it fails is not promised. See
  the note.
- **No kernel TLS on the rustls backend:** large static files over TLS use `sendfile` with kernel TLS only on the OpenSSL
  backend.
- **The OpenSSL full handshake costs about 1.4x of nginx's CPU** (1.64M instructions, 1.40M of them inside OpenSSL) and
  1.12x of HAProxy's, unchanged from 6.4.0. The rustls backend uses 58% fewer instructions for it.
- nginx is still ahead on the proxied HTTP/1 rows: 1.04x to 1.23x of its CPU per request and 0.86x to 0.90x of its rate
  on one worker (POST, direct response and TLS keep-alive are level).

## Tests

- The supervisor master replacement tests no longer time out because a loaded host is slow, a probe that gave up no
  longer stops the worker from serving, and a crashing candidate is a crash however late the test polls.
- The lane differential corpus compares only requests an origin received whole, and the origin notes a request cut short
  when the proxy closes its upstream connection, so a cut request no longer appears in the next case.
- The pooled-timer test no longer counts the waker its own work timer keeps.
- New regression tests hold the bytes of an idle connection, the growth over reloads and the allocator calls per request
  of each route shape under ceilings, and `queue_cancel_wire` covers queue cancellation over HTTP/1.1, HTTP/2 and HTTP/3 on the lanes and the classic paths.

## Upgrade notes

- No configuration change is needed; a 6.4.0 configuration loads unchanged.
- Dashboards that alert on pool queue length can add `oxiroute_pool_queue_cancellations_total{pool}`.
- A profile that switches to `backend = "rustls"` keeps its `cipher_list`; check the startup log for the suites rustls
  does not implement.

## Memory

- An idle plain WebSocket connection costs 16.6 kB of resident memory instead of 31.6 kB (nginx 18.4 kB, HAProxy 6.3 kB), an
  idle WebSocket over TLS 38.6 kB instead of 53.5 kB (nginx 60.1 kB, HAProxy 30.9 kB) and an idle HTTP/1 keep-alive
  connection 11.9 kB instead of 17.8 kB (nginx 9.2 kB, HAProxy 11.2 kB). An upgraded connection no longer keeps the
  request's state machine: the handshake ends the request, the connection continues as a small tunnel and the hooks and
  the access log run when it ends.
- A close-delimited HTTP/1 body (and so an upgraded connection) no longer allocates its 128 KiB chunk buffer before it
  reads.
- jemalloc returns freed pages from a background thread, started once the process has adopted its control endpoint (a
  supervised worker must still be single-threaded then). After connection bursts and reloads the process settles at about
  half of the resident memory it used to keep (97.9 MB to 48.8 MB in a compressed churn run). A `background_thread` key in `_RJEM_MALLOC_CONF` still decides for itself.
- A connection's task and request context are smaller (the connection counter wraps the future without copying it,
  the cache state of a request is boxed until a cacheable request needs it).
- An idle HTTP/1 keep-alive connection holds no read buffer while it waits: 7.1 kB instead of 12.0 kB (nginx 8.7 kB), and
  an idle plain WebSocket 13.2 kB instead of 17.6 kB (an upgraded connection no longer keeps the first 64 KiB of the
  tunnel, its response header's spare slots or a path-sized address per socket digest). With the opt-in rustls backend an
  idle WebSocket over TLS is 22.6 kB (OpenSSL 35.0 kB).
- A retired generation process returns its memory when it is reaped: the growth of the process over four reloads of a
  1,000-route configuration is 15.9 MB instead of 32.4 MB one second after the last one.
- A cached response owns its header values instead of pinning the 4 KiB buffer it arrived in: 3.1 kB instead of 6.1 kB of
  memory per cached 1 KiB response.
- Regression tests keep the bytes of an idle connection (`idle_connection_memory.rs`) and the growth over many reloads
  (`reload_memory.rs`) under ceilings.
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
  results.
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
- No scheduling default changed. Measured against nginx and HAProxy on one worker:
  `connection_scheduling = "aged_per_worker"` serves 1.2x to 1.5x of nginx's rate at 1,024 and 2,048 connections with a p99 of 1.6x and 1.4x
  nginx's (work stealing: 3.3x to 5.5x), at 0.7x of work stealing's rate at 1,024 connections on one worker (1.0x at 2,048 and at 256);
  from two workers up the two are within noise. It stays opt-in (and `auto` selects it from two workers).

## Fixes

- A request whose client closes its connection while the request waits for an origin is dropped at once. The wait
  ends and its place in the pool queue and its slot are freed (the retry delay, the queue for a free slot, the
  connection to the origin), and the origin never receives the request. Before, an abandoned request stayed queued,
  was sent to the origin when a slot freed up and ran there for nobody. This holds for HTTP/1.1 (fast lane and
  classic path, including a request whose body was announced but not sent), HTTP/2 (stream reset and connection
  close) and HTTP/3. `oxiroute_pool_queue_cancellations_total{pool}` counts the requests that left the queue this
  way, and the debug target `oxiroute::queue` names each one (request id, route and pool). A request that was sent
  to the origin already is handled as before. An HTTP/1.1 client that half-closes its connection (`shutdown` of the
  write side) while its request is queued is treated as gone, as the classic path always treated it once the
  request was sent.

## Measured on the integrated tree

- User+sys CPU per request against 6.4.0 is 0.91x to 0.98x on the proxied, TLS keep-alive, HTTP/2 and direct rows. A full TLS 1.3
  handshake costs the same CPU as 6.4.0 (the 9% to 13% first measured did not reproduce over 25 interleaved runs), and the
  higher resident set of an idle worker first measured was the page-cache state of a freshly linked binary, not the code
  (regression note). Against HAProxy CPU is lower on every row except new connections (1.04x);
  against nginx the proxied HTTP/1 rows stay at 1.04x to 1.23x.
- Idle memory against 6.4.0, nginx and HAProxy in one invocation: a WebSocket holds 13.1 kB (6.4.0 31.6 kB, nginx 18.4 kB, HAProxy
  6.1 kB), over TLS 35.1 kB (53.6, 60.1, 31.5), an HTTP/1 keep-alive connection 8.7 kB (15.0, 9.6, 11.6), over TLS 29.9 kB (36.2,
  34.6, 35.6) and an HTTP/2 connection over TLS 41.5 kB (50.2, 50.3, 46.0). Four reloads of a 1,000-route configuration grow the
  process by 16.2 MB (6.4.0: 47.8 MB).

## Tools

- The `heap-profile` cargo feature of `oxiroute` builds jemalloc with profiling and statistics, for memory investigations
  only.
