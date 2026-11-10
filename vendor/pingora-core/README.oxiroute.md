# Vendored pingora-core patch

This directory contains the published `pingora-core 0.8.1` source from crates.io
(registry checksum `6a7ffe2f5acf9f94fd255cfd1438866bc9124f8f0c7d42562bd3f853df2094b7`)
under its upstream Apache-2.0 license.

OxiRoute's delta is intentionally limited to an OpenSSL-derived, per-peer TLS configure hook,
pre-handshake application admission and per-application handshake timeout, a non-truncating certificate subject conversion, correct
HTTP/1 HEAD informational-response framing, owned HTTP/1 response preread payloads, borrowed HTTP/1
upstream request writes, parser-owned HTTP/1 header names, an inline HTTP response task batch, an
HTTP/2 test lifecycle correction, a lazily allocated read buffer, pooled gzip deflate state, a per-thread
header-name cache, an idle upstream connection pool without per-connection tasks, a listener-level
`TCP_NODELAY`:
header-name cache, pooled body buffers handed out without a copy, a coalescing write buffer, and stage-1
connection-pool experiments:

- `protocols/tls/mod.rs` declares the public `TlsConfigureHook` type.
- `upstreams/peer.rs` stores the optional hook in `PeerOptions`, omits it from
  `Debug`, and exposes it through `Peer`.
- `connectors/tls/boringssl_openssl/mod.rs` invokes it after CA, verification,
  hostname, and ALPN setup, but before clearing the OpenSSL error stack and
  starting the handshake. Hook failures become `TLSHandshakeFailure` errors.
- `apps/mod.rs` exposes an opaque connection-admission guard and a transport-handshake timeout hook,
  defaulting to 60 seconds, and
  `services/listening.rs` acquires it after TCP accept and retains it through the handshake and
  application connection lifetime while applying the owning app's timeout.
- `apps/mod.rs` keeps accepting HTTP/2 streams after the service's shutdown signal sends GOAWAY (the connection
  loop calls `graceful_shutdown` once and goes on, ending when `accept` reports the connection closed) instead of only
  waiting for the close: a stream the client opened before it saw the GOAWAY was never accepted, so it was never
  answered and kept the connection, and the generation that owned it, open until the client gave up.
  `apps/h2_shutdown_tests.rs` covers it.
- `utils/tls/boringssl_openssl.rs` uses OpenSSL's non-truncating subject string
  conversion instead of the deprecated conversion that stops at interior NULs.
- `protocols/http/v1/client.rs` classifies informational responses before applying HEAD no-body
  framing, so a 100/103 response does not prevent the final response from being read.
- `protocols/http/v1/body.rs` can retain immutable content-length and close-delimited preread bytes
  from the HTTP/1 response header allocation. The client moves those bytes into response tasks
  without copying them; chunked and socket-read payloads keep the upstream implementation's
  reusable-buffer behavior.
- `protocols/http/v1/client.rs` exposes `write_request_header_ref` for serializing a borrowed
  request while retaining `write_request_header(Box<RequestHeader>)` as a compatibility wrapper.
  After a successful write, the session retains only derived HEAD framing, request keepalive, and
  upgrade metadata for borrowed writes; request-body framing remains in `BodyWriter`. The legacy
  boxed API additionally retains its owned request until the session is dropped, preserving
  extension and resource lifetimes.
- `protocols/http/v1/{common,server,client}.rs` stores parsed downstream request and upstream
  response fields directly in the case-insensitive `HeaderMap`, retaining zero-copy values but no
  second original-case map or shared name slices. Emitted HTTP/1 names use canonical title case for
  `Accept-Ranges`, `Age`, `Cache-Control`, `Connection`, `Content-Type`, `Content-Encoding`,
  `Content-Length`, `Date`, `Transfer-Encoding`, `Host`, `Server`, and `Set-Cookie`; every other name
  is lowercase. HTTP field names are case-insensitive; non-compliant peers that require original
  wire casing will observe this normalization. Programmatically built case-preserving headers,
  generic mutation APIs, and HTTP/2 behavior remain unchanged.
- `protocols/http/mod.rs` defines the additive four-task `HttpTaskBatch` backed by `smallvec`.
  `ServerSession` and the HTTP/1 server expose additive batch response APIs; HTTP/1 shares one
  implementation with the existing `Vec` API, while HTTP/2, subrequest, and custom sessions retain
  their existing `Vec` behavior through a compatibility conversion.
- `protocols/http/v2/server.rs` closes both stream halves in the empty request DATA/EOS test before
  dropping client handles, matching `h2` 0.4.15 cancellation behavior without changing production.
- `connectors/mod.rs` no longer performs a release-side readiness read before a protocol-approved
  reusable stream enters the pool. The existing pre-visibility mutex guard, checkout identity and
  readiness checks, idle watcher, LRU eviction, timeout handling, and connection-lifetime
  notification remain unchanged.
- `upstreams/peer.rs` provides an additive cached-physical-address identity hook for standard peers.
  `connectors/l4.rs` records the connected TCP, pathname Unix, or CONNECT proxy next hop when known,
  while custom L4 connections retain descriptor fallback. Transport and HTTP/2 checkout use that
  opt-in identity before the existing descriptor syscall without changing readiness, pool keys,
  release behavior, or retained connection objects.
- `protocols/l4/stream.rs` constructs accepted TCP, Unix, and virtual streams with one-byte read and
  write buffers instead of zero-capacity ones, which tokio 1.54 rejects. Both sizes stay below every
  read and write, so the streams remain unbuffered until `set_buffer`.
- `protocols/l4/lazy_read.rs` replaces `tokio::io::BufStream` as the accepted stream's buffer. Reads go
  through one 64 KiB scratch buffer per thread and only the bytes the caller did not ask for stay with the
  connection, in a buffer that is released as soon as the socket would block, so an idle keep-alive
  connection holds no read buffer (it held 64 KiB). Reads of 64 KiB or more still bypass the scratch;
  the write buffer is the same `BufWriter`. The module's tests need only `tokio`.
- `protocols/l4/stream.rs` `Drop` no longer probes the socket with `getsockopt(TCP_NODELAY)` before the
  final flush: a broken socket fails the flush, which is logged at debug level.
- `apps/mod.rs` `AcceptGate` counts the threads blocked in `AcceptGateClose::wait` and notifies the
  `quiesced` condition variable only while one exists, under the mutex that guards the count. Every
  accepted connection releases an ownership claim, so an unconditional `notify_all` was one `futex` call
  per connection.
- `protocols/http/compression/gzip.rs` frames gzip around a raw `flate2::Compress` deflate state that returns
  to a per-thread pool (at most four, one per level) when the response ends, instead of creating a
  `GzEncoder` that allocates and clears about 380 KiB for every response. The stream carries the same header
  and trailer as `GzEncoder`; the deflate state is reset between uses.
- `protocols/http/v1/common.rs` remembers parsed header names per thread (at most 512 names of up to 64
  bytes, cleared when full), so a name outside the `http` standard table is allocated once per thread instead
  of once per request.
- `protocols/tls/boringssl_openssl/{stream,server}.rs` lets an accepted OpenSSL stream (`prepare_tls_stream`) share TLS
  records: plain writes are held (up to 1460 bytes, the size of the accepted socket's own write buffer) and vectored
  writes (HTTP/2 frames, chunked bodies) are held until the next `flush` or `shutdown`, so a response head plus body
  or an HTTP/2 HEADERS plus DATA leaves as one record instead of two. Vectored bytes are packed into full 16 KiB records
  (an HTTP/2 DATA frame of 16384 bytes no longer costs a 9 byte record for its frame header), in a buffer taken from a
  per-thread spare while the connection is writing. A plain write that does not fit flushes the held bytes first and goes
  out by itself. Client streams (upstream TLS) are unchanged. Writers already had
  to flush for the socket buffer; unflushed small writes were never sent before either.
- `protocols/mod.rs` `Peek::poll_read_ready` (default: ready, meaning unknown) lets a transport say whether a read could make
  progress without reading; `protocols/l4/stream.rs` answers from the socket's readiness (or its buffered bytes).
  An accepted OpenSSL stream uses it after a read that ran dry: it returns pending from the transport's answer without
  calling `SSL_read` again until the socket is readable, instead of running the record layer once per wake-up for the
  same "want read". `SSL_write`, handshakes and clients are unchanged.
- `protocols/http/v2/server.rs` `write_response_header` moves the header map into the HTTP/2 response instead of cloning it,
  so `response_written()` of an HTTP/2 session keeps the status line and no headers (nothing in OxiRoute reads them there).
- `connectors/idle.rs` replaces the HTTP/1 upstream keep-alive pool (`pingora_pool::ConnectionPool` behind
  `Arc<Mutex<Stream>>`) in `TransportConnector`. A released connection used to get a task that read the idle
  socket and a `oneshot`, `Notify`, LRU node and mutex handshake with the next checkout (two scheduler round trips
  per request). Now a release polls the socket once with a waker that queues the connection on a shared event list
  and wakes one pool-wide task, which drops connections that closed, sent data or passed `idle_timeout`; a checkout
  pops an entry from a per-thread stripe, then runs the same identity and readiness checks. Capacity is per stripe
  (as the per-thread LRU it replaces was) and evicts the connection released longest ago. `idle_timeout` of zero
  keeps nothing. The HTTP/2 pool still uses `pingora_pool`. The `connectors::tests` cases that count socket reads
  expect the extra read at release.
- `listeners/l4.rs` sets `TCP_NODELAY` on the listening socket and no longer on every accepted stream: Linux copies
  it to the accepted socket. Keepalive and DSCP settings are still applied per accepted stream.
- `utils/chunk_pool.rs` keeps up to eight 128 KiB buffers per thread. `protocols/http/v1/body.rs` takes its body
  buffer from it and returns it when the next body starts or the reader drops. `read_body_bytes` hands a
  content-length or close-delimited read of 8 KiB or more out as the buffer it was read into (the buffer returns to
  the pool of the thread that drops the last clone) instead of copying it into a new `Bytes`; chunked and smaller
  reads are copied as before. `finish_body_buf` only splits the buffer when bytes were read past the body.
  `oxiroute-server` also reads static file chunks into the same pool.
- `protocols/http/v1/server.rs` writes a body task of 4 KiB or more from its own buffer after the bodies queued
  before it, instead of copying it into the batch buffer (`queue_body_data`).
- `protocols/l4/lazy_read.rs` replaces `tokio::io::BufWriter` with `CoalescingWriter`: same buffering and
  pass-through rules, but a write of at least the buffer size that finds bytes buffered goes out in one `writev`
  with them, so a response head and a large body are one system call and one TCP send instead of two.
- `protocols/l4/stream.rs` no longer times how long reads and writes stay pending (two clock reads for every pending
  poll): `get_read_pending_time`, `get_write_pending_time` and so `Session::upstream_write_pending_time` return zero.
  OxiRoute does not use them.
- `protocols/http/v1/server.rs` adds `write_rendered_response`: a response head rendered by the caller (a fixed response's
  status line and headers) goes out with the `Date` and `Connection` lines in one buffer, with no `ResponseHeader`.
  `http_resp_header_to_buf` writes `Date` and `Connection` inline instead of inserting them into the header map, so a
  proxied response head is no longer grown and rehashed for them; both reuse a per-session buffer of at most 64 KiB.
  `protocols/http/server.rs` exposes it as `Session::write_rendered_response` (`Ok(false)` for HTTP/2 and HTTP/3,
  which take the header-map path) and `protocols/http/date.rs` adds `put_cached_date`.
- `protocols/http/v1/server.rs` and `v1/client.rs` size the parsed request header map for four more headers and the
  response header map for three more (`REQUEST_HEADER_HEADROOM`, `RESPONSE_HEADER_HEADROOM`): a map built exactly full
  reallocates and rehashes when the first header is added after parsing.
- `protocols/http/v1/{server,client}.rs` parse a head into uninitialised header slots (`header_slots`, `httparse`'s
  `parse_with_uninit_headers`) instead of `[EMPTY_HEADER; 256]`, which stored 8 KiB on every parse attempt.
- `protocols/http/v1/client.rs` `read_resp_header_parts` hands the parsed `ResponseHeader` to the caller instead of
  cloning its header map. The session keeps the status, version and the `Connection`, `Keep-Alive` and
  `Transfer-Encoding` facts that keepalive needs (`ResponseFacts`), so `resp_header()` and `get_header()` return `None`
  once the header was taken; `get_status()` and `respect_keepalive()` are unchanged.
- `protocols/http/v1/server.rs` `read_body_bytes` returns the body reader's `Bytes` (a large content-length chunk is the
  read buffer itself, see `chunk_pool`) instead of copying the slice, and `protocols/http/body_buffer.rs`
  (`FixedBuffer`, the retry buffer) keeps chunks of 64 KiB or more as shared `Bytes` and copies shorter ones into a
  joined tail, so a replayable upload is not copied and a slow one never pins its 128 KiB read buffers.
- `protocols/http/v1/body.rs` writes the chunked-encoding size line from a stack buffer (`chunk_size_line`) instead of a
  `format!` string.
- `protocols/http/v1/client.rs` `try_read_response_task` returns the next response task only when it needs no wait for
  the network (the end of a response, or a body chunk already buffered or returned at once by a read), so the inline
  relay in `pingora-proxy` batches ready chunks without arming a read timer for each chunk that is not there yet.
- `apps/mod.rs` `HttpServerApp::process_new_http_inline`: the HTTP/1 request loop awaits an application's request
  future where it stands instead of boxing it (`async_trait` copied 8 KiB and more of state machine to the heap for every
  request). The default calls `process_new_http`.
- `apps/mod.rs` and `apps/idle_wait.rs`: after a reusable response the HTTP/1 loop of `process_new` waits for the next
  request holding no `Session` and no request future (`wait_for_next_request`: `poll_read_ready` of the stream, the
  shutdown watch, and the keep-alive timeout), then builds the session. The timeout is
  `HttpServerApp::idle_keepalive_timeout()` (new, default `None`) or else the keep-alive seconds of the last response
  (`HttpPersistentSettings::idle_timeout`). A stream that cannot tell if it is readable (TLS) is ready at once, and the
  request read that follows waits as before.
- `apps/mod.rs` `process_new`: the HTTP/1 request future lives on the heap per request (`apps/pooled_future.rs`:
  the block is written in place and kept by the thread for the next request, 64 per thread at most) so the future of
  the connection, which lives as long as the connection, no longer contains it, and the HTTP/2 loop is the boxed free
  function `process_h2` (same code, `self` renamed to `app`).
- `services/listening.rs` boxes the transport handshake future inside the per-connection task, so the task of a plain
  connection does not hold the TLS handshake state.
- `protocols/l4/lazy_read.rs` `CoalescingWriter` takes its 1,460-byte buffer from a per-thread pool when the first
  bytes are staged and gives it back once they are sent (flush, or a large write behind the staged head), instead of
  one buffer per connection for its whole life.
- `protocols/http/v1/common.rs` `RequestFacts` / `ResponseFacts`: while a head is parsed, the HTTP/1 server and client note
  whether it carries `Transfer-Encoding`, `Connection` and `Upgrade` and how many `Content-Length` headers it has, so the
  keepalive, framing, duplicate `Content-Length` and upgrade checks read flags instead of looking the headers up again. Header values are appended to
  the map after one validity check (`is_header_value`, eight bytes at a time) with no second copy.
  `protocols/http/v1/server.rs` keeps the facts in the session; `req_header_mut()` clears them (the caller may change
  what they describe) and `req_header_mut_keeping_facts()` keeps them. `protocols/http/server.rs` exposes both,
  `request_facts()` (`RequestFacts::UNKNOWN` for HTTP/2 and HTTP/3, which makes the caller look the headers up) and
  `modules/http/mod.rs` `HttpModuleCtx::is_empty`. `protocols/http/v1/client.rs` `write_request_header_noted` writes a
  borrowed request with the facts the caller already has (`write_request_header_ref` computes them).
- `Cargo.toml` declares `prometheus 0.14`, `x509-parser 0.18`, `lru 0.19`, `daggy 0.9`,
  `strum`/`strum_macros 0.28`, `brotli 9`, and `parking_lot 0.13`, which compile unchanged. `sfv`
  stays at `0.10` because later releases change the parser and integer APIs used by compression.

- Tests: the unit tests no longer depend on the Internet, fixed ports, a stored key pair or host sysctls. `connectors/test_servers.rs`
  provides loopback servers that own a kernel-assigned port (HTTP redirect, TLS with HTTP/1.1 or HTTP/2, a refused address, a
  blackhole whose accept queue is full), `test_tls.rs` generates a throwaway self-signed certificate per test, and the listener tests
  bind port 0. The TLS tests (and the `openssl` dev-dependency that generates the certificate) cover the OpenSSL backend only.
  Streams coalesce writes, so a test server must flush. `scripts/test-vendored.sh` runs every vendored suite against these sources
  in a network namespace with only loopback.

The normalized crates.io `Cargo.toml` is retained. Its optional `Cargo.toml.orig` reference is
upstream packaging commentary; that non-build manifest is intentionally omitted here.

When upgrading Pingora, replace this directory from the newly locked published crate, reapply and
review the HTTP/2 shutdown accept loop, the parked idle wait and pooled request future, the header slots, request and response facts, the noted request write, inline request loop, the zero-copy request body and retry buffer, `try_read_response_task`, the connector hook, admission guard and handshake-timeout hook, rendered response write, inline Date and Connection lines, header map headroom, single-clock timeouts, subject conversion, HTTP/1 HEAD informational-response,
owned-preread, borrowed-request-write, parsed-header ownership, response-task batch, lazy read buffer, pooled
gzip state, header-name cache, HTTP/2 test lifecycle, connection-pool changes, the idle pool and the listener `TCP_NODELAY`, then rerun the vendored
gzip state, header-name cache, body buffer pool and hand-over, direct large body writes, coalescing write buffer,
HTTP/2 test lifecycle, and connection-pool changes, then rerun the vendored
connector pool, BodyReader, H1 client, and H2 server suites, OxiRoute TLS and HTTP wire tests, and strict clippy checks.
