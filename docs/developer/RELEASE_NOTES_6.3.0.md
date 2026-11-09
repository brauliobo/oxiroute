# OxiRoute 6.3.0 (draft)

Draft assembled from the release-note lines of the `PERF_6_3_*.md` notes and the integration verification of
`release/6.3`. When the release is cut, move it to `docs/RELEASE_NOTES_6.3.0.md` (the release checks look for it
there) and set the final measurements.

OxiRoute 6.3.0 makes the request path of 6.2 cheaper: a proxied HTTP/1.1 request needs about 42% fewer user-space
instructions, a direct response 54% fewer, a TLS 1.3 keep-alive request 43% fewer, an HTTP/2-over-TLS request 34%
fewer, and large bodies, uploads, WebSocket messages and TCP relays 30% to 80% fewer. Selection by least connections
and weighted round robin, and pool membership changes, no longer grow with the number of servers. One product bug
is fixed: an HTTP/2 request that reaches a connection while its generation is being replaced is answered instead of
waiting for the client to give up. The release remains pre-alpha and retains conservative compatibility claims.

## Summary

- **Request path.** One loop relays every HTTP/1 request that does not touch the cache (with or without a body,
  upgrades included); fixed responses are one buffer and one write; the request id, the framing facts, the clock
  readings and the names of the in-flight view are taken once per request instead of once per hook; idle origin
  connections are tracked by one pool task instead of one task each.
- **Connections.** One connection per request costs about 40% fewer instructions; the accepted socket inherits
  `TCP_NODELAY` from the listener, and the HTTP/1 connection loop no longer boxes the request future or registers
  for two shutdown signals per request.
- **Bodies.** Large response and upload chunks are the buffers they were read into (no copy), a response head and a
  large body leave in one `writev`, TCP relays move bulk data in larger transfers.
- **TLS and HTTP/2.** A response head and body leave as one TLS record, a dry socket is not read through OpenSSL again,
  and a server that cannot resume sessions sends no TLS 1.3 tickets.
- **Routing and pools.** Least connections and weighted round robin keep an index once a pool has more than eight
  selectable servers (2,000 servers: 163,722 to 1,616 and 241,045 to 2,921 instructions per selection); adding or
  removing one server costs 4x to 10x less; a request bound to an idle pooled connection now reserves it.
- **Fix.** HTTP/2 requests that arrive while a generation is replaced are no longer lost (see Fixes).
- **Release binaries are profile-guided.** See the PGO note.

## Measured effect

User-space instructions (`perf stat -e instructions:u`) per request from `benchmarks/r2-data-path`, one proxy
thread, 16 keep-alive connections unless the lane says otherwise, 6.2.0 and `release/6.3` built the same way (shipped
release profile, no PGO) and measured one after the other on the same host. The full table, the idle, startup and
routing rows and the comparison with nginx and HAProxy are in
[the 6.3 performance summary](PERF_6_3_SUMMARY.md).

| Lane | 6.2.0 | 6.3 | Change |
| --- | ---: | ---: | ---: |
| HTTP/1.1 proxied, 1 KiB | 73,071 | 42,000 | -42.5% |
| HTTP/1.1 proxied, 1 KiB, new connection per request | 91,236 | 54,288 | -40.5% |
| Direct (fixed) response | 38,503 | 17,604 | -54.3% |
| Proxied with forwarded headers and host rewrite | 90,892 | 52,744 | -42.0% |
| Proxied with the access log | 81,679 | 49,157 | -39.8% |
| Proxied 1 MiB response | 329,477 | 65,459 | -80.1% |
| POST 1 KiB / 64 KiB / 1 MiB | 80,312 / 116,019 / 297,612 | 46,570 / 76,038 / 90,361 | -42% / -34% / -70% |
| Static file 1 KiB | 40,457 | 25,247 | -37.6% |
| WebSocket echo 1 KiB | 19,297 | 12,072 | -37.4% |
| TCP relay echo 1 KiB / 256 KiB | 11,371 / 125,468 | 6,447 / 31,260 | -43% / -75% |
| TLS 1.3 keep-alive, 1 KiB | 95,312 | 54,803 | -42.5% |
| HTTP/2 over TLS, 1 KiB | 101,932 | 67,680 | -33.6% |
| TLS 1.3 full handshake plus one request | 1,787,344 | 1,687,350 | -5.6% |

## Request path

Details: [fast path](PERF_6_3_FAST_PATH.md), [HTTP/1 path](PERF_6_3_H1_PATH.md),
[request context](PERF_6_3_REQUEST_CONTEXT.md), [bodied relay](PERF_6_3_BODIED_RELAY.md).

- Proxied HTTP/1.1 requests cost about 19% fewer instructions (1 KiB response: 58.5k to 47.4k per request) from the
  single relay loop and native `async fn` hooks, a further 8% to 18% from per-request context (1 KiB proxied: 47.5k to
  42.1k; with forwarded headers and a host rewrite: 64.2k to 52.9k), and fixed responses cost about 15% fewer from the
  one-buffer write. No change in behaviour on the wire: framing, refusals and header forwarding are pinned by new wire
  tests.
- Proxied uploads, WebSocket and other upgraded connections, and HTTP/2 clients cost fewer instructions: a 1 KiB POST
  8% fewer, a 1 MiB POST about half, a WebSocket message 30% fewer. TCP relays cost 40% fewer instructions for small
  messages and about 70% fewer for large transfers.
- A request that carries a single valid `x-request-id` keeps it as it was received (header case included); a missing
  or malformed one is replaced, as before, and every response, proxied or local, carries the id.

## Connections and upstream reuse

Details: [connection lifecycle](PERF_6_3_CONNECTION_LIFECYCLE.md).

- Idle HTTP/1 upstream connections are tracked by one pool task instead of one task per connection; closed or noisy
  idle connections are still dropped promptly, a connection the peer closed before it is pooled is not pooled, and
  idle connections are closed when the service that owns them stops.
- A worker thread keeps up to 1024 idle upstream connections (was 128), so a proxy with hundreds of busy clients stops
  closing and reopening origin connections; up to 1024 idle origin sockets per worker thread can stay open until the
  origin or an idle timeout closes them.
- Correlation ids of access records are created when the record is written.
- TCP relay buffers start at 16 KiB per direction and grow to 64 KiB per direction on a connection that moves bulk data.

## Bodies, static files, TLS and HTTP/2

Details: [bodies and static files](PERF_6_3_BODIES_AND_STATIC.md), [TLS and HTTP/2](PERF_6_3_TLS_H2.md).

- Proxied response bodies reach the client in chunks of up to 128 KiB instead of 64 KiB (first-byte latency is
  unchanged: a read returns what is available). Each worker thread keeps up to 1 MiB of idle body buffers.
- A TLS 1.3 server with `session_tickets = false` and no `session_cache` no longer sends session tickets (clients
  could not have used them). A profile with a session cache or `session_tickets = true` is unchanged.
- The `x-request-id` response header is an HPACK never-indexed literal on HTTP/2 (the value and the header are
  unchanged). An accepted HTTPS connection holds up to about 2 KiB of pending plaintext after a response.

## Routing and pools

Details: [routing and pools](PERF_6_3_ROUTING_AND_POOLS.md).

- Least connections and weighted round robin keep an index once a pool has more than eight selectable servers; the
  chosen server is the same one the one-pass code chose (a randomized comparison test replays the original scan).
- With `connection_reuse safe` and capacity-limited servers, a request bound to a server's idle pooled connection now
  reserves it, so the requests selected next queue for another server instead of binding to the same connection (the
  6.2 known limit).
- Adding or removing one pool member resolves names with a small index instead of rebuilding two hash tables.

## Fixes

- **HTTP/2 requests during a reload were lost.** A stream that the client opened just before it saw the GOAWAY of a
  connection that was shutting down was never accepted, so it was never answered, and it kept the connection and the
  generation that owned it open until the client gave up (found by the reload soak:
  `benchmarks/r2-data-path/soak.py`; present in 6.2.0). The connection now goes on accepting streams until it is
  closed. Requests of every other kind were already answered through a reload; HTTP/1 clients still see an idle
  keep-alive connection closed once when a generation is replaced, as documented.

## Behaviour changes to know before upgrading

- Response bodies arrive in chunks of up to 128 KiB (was 64 KiB), a response head and a large body are sent in one
  system call, and a TLS response head and body are one record. Nothing on the wire changes except the segment sizes.
- Up to 1024 idle upstream connections per worker thread can stay open (was 128), and 1 MiB of body buffers per
  worker thread stay allocated while idle.
- TLS 1.3 session tickets are not sent by a server that cannot resume sessions (see above).
- `Session::upstream_write_pending_time()` and the stream pending times of the vendored Pingora return zero.
- Idle cost: a minimal idle worker executes about 11.5k user-space instructions per second (9.0k in 6.2.0) because
  the per-thread request counters (one copy per worker slot, so recording a request is cheaper) are summed each time
  the worker reports its status; a farm-size instance is within 5%.
  `config check` costs about 0.8M instructions more per run.
- Internal: the vendored `ProxyHttp` trait no longer uses `async_trait`; hooks are written `fn .. -> impl Future` (an
  `async fn` in an implementation still works). `HttpServerApp` has `process_new_http_inline`. The vendored
  `pingora-core` HTTP/1 session exposes `request_facts()` and `req_header_mut_keeping_facts()`;
  `req_header_mut()` clears the noted facts. `InflightArrival` borrows instead of owning its text.

## Upgrade notes

- No configuration change is needed and no setting was added. A 6.2 configuration loads unchanged.
- Restart or reload as usual. A reload closes each idle HTTP/1 keep-alive connection of the replaced generation once;
  busy connections finish their request with `Connection: close`. HTTP/2 connections get a GOAWAY and finish their
  streams.
- Anything outside this repository that implements the vendored `ProxyHttp` trait must follow the hook signature
  change above.

## Profile-guided optimization

The release binaries attached to the GitHub release are built with profile-guided optimization from the training
workload of the data-path harness (14% to 27% fewer instructions per request than the plain build; see
[build levers and allocator](PERF_6_3_BUILD_AND_ALLOCATOR.md)) and the archive records whether a build is
profile-guided. The Arch package and the source archive are built without a profile and are unchanged. The numbers
in these notes are from plain builds.

## Vendored Pingora

`vendor/pingora-core/README.oxiroute.md`, `vendor/pingora-proxy/README.oxiroute.md` and
`vendor/pingora-timeout/README.oxiroute.md` list every delta. New in 6.3:

- `pingora-core`: idle upstream connection pool without per-connection tasks (`connectors/idle.rs`); listener-level
  `TCP_NODELAY`; pooled 128 KiB body buffers handed out without a copy; a coalescing write buffer for accepted
  sockets and TLS records; request and response framing facts noted at parse; rendered fixed responses and inline
  `Date` and `Connection` lines; header-map headroom; a response head handed to the caller instead of cloned;
  `process_new_http_inline`; `try_read_response_task`; the retry buffer keeps large chunks as shared `Bytes`;
  `Peek::poll_read_ready`; HTTP/2 shutdown keeps accepting streams until the connection is closed; loopback-only unit
  tests (`connectors/test_servers.rs`, `test_tls.rs`).
- `pingora-proxy`: native `impl Future` hooks; the single-loop HTTP/1 relay (`proxy_h1_inline.rs`); the HTTP/2 upstream
  response queue; a single shutdown wait per request head.
- `pingora-timeout`: a pending timeout reads the clock once and boxes its timer once.
- `scripts/test-vendored.sh` runs every vendored suite in a network namespace with only loopback.

## Known limits

- Against nginx and HAProxy at one worker OxiRoute is still behind on the small-request lanes: about 0.7x to 0.75x
  the requests per second of nginx and 0.75x to 0.9x those of HAProxy (it was 0.5x and 0.6x), and about 1.9x to 2.7x
  the instructions of nginx and 1.0x to 1.6x those of HAProxy. It is ahead on 64 KiB bodies, the static file and 2048
  keep-alive connections. See [the comparison](BENCHMARK_VS_NGINX_HAPROXY.md#630-re-measured).
- Least-connections selection in a pool where no server is ever saturated costs about 30% to 40% more per selection
  than the 6.2 one-pass code (the index pays for itself as soon as no server is idle).
- A client's `Connection: close` still takes the request-rewrite path.
- HTTP/2 DATA frames of exactly 16384 bytes still use two TLS records each; HPACK encoding costs about 3k
  instructions per response.
- The cargo audit status is unchanged from 6.0.0: RUSTSEC-2026-0388 and RUSTSEC-2026-0390 in the Pingora 0.8.1
  line, plus `unsafe-libyaml` (unmaintained) and `lru` 0.16.4 (unsound) through it.
