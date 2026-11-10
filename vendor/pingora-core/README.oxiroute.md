# Vendored pingora-core patch

This directory contains the published `pingora-core 0.9.1` source from crates.io
(registry checksum `a535f47f65fdcb003f9e1df04cd18e614c128dfe9388acce3f211ae38ac14d68`)
under its upstream Apache-2.0 license. The 0.8.1 to 0.9.1 rebase (a three-way merge from the pristine 0.8.1 tree) clears
RUSTSEC-2026-0388 (HTTP/2 `:authority`/`Host` mismatch, userinfo) for this crate; the repository history holds the pristine
0.9.1 import as its own commit, so `git diff` against it shows exactly the OxiRoute delta listed below.

OxiRoute's delta over the pristine release is listed below. Changes the 0.9.x line took (the non-truncating certificate
subject conversion, HEAD informational-response framing, the HTTP/2 shutdown accept loop, `daemonix`, the dependency
bumps for `lru`/`parking_lot`/`brotli`/`daggy`/`strum`/`x509-parser`, and the tokio 1.54 zero-capacity stream buffers)
were dropped from the delta. `apps/h2_shutdown_tests.rs` stays and passes against upstream's loop.

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
- `connectors/mod.rs` no longer performs a release-side readiness read before a protocol-approved
  reusable stream enters the pool. The existing pre-visibility mutex guard, checkout identity and
  readiness checks, idle watcher, LRU eviction, timeout handling, and connection-lifetime
  notification remain unchanged.
- `upstreams/peer.rs` provides an additive cached-physical-address identity hook for standard peers.
  `connectors/l4.rs` records the connected TCP, pathname Unix, or CONNECT proxy next hop when known,
  while custom L4 connections retain descriptor fallback. Transport and HTTP/2 checkout use that
  opt-in identity before the existing descriptor syscall without changing readiness, pool keys,
  release behavior, or retained connection objects.
- `protocols/l4/lazy_read.rs` replaces `tokio::io::BufStream` as the accepted stream's buffer. Reads go
  through one 64 KiB scratch buffer per thread and only the bytes the caller did not ask for stay with the
  connection, in a buffer that is released as soon as the socket would block, so an idle keep-alive
  connection holds no read buffer (it held 64 KiB). Reads of 64 KiB or more still bypass the scratch;
  the write buffer is the same `BufWriter`. Streams are created with zero-capacity buffers as upstream does (a read size
  of 0 reads straight from the socket, which tokio 1.54 allows here). The module's tests need only `tokio`.
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
  pops an entry from a per-thread stripe, then runs the same identity and readiness checks. Capacity is the global
  `keepalive_pool_size` (0.9 sizes it per worker times the thread count); a release at capacity evicts the oldest
  connection of its own stripe, else of the next non-empty one, and reports how long it had been idle to
  `keepalive_pool_callback`. Idle sockets that deliver data count in the 0.9 unexpected-data counter. `idle_timeout` of zero
  keeps nothing. The HTTP/2 pool still uses `pingora_pool`. The `connectors::tests` cases that count socket reads
  expect the extra read at release.
- `listeners/l4.rs` sets `TCP_NODELAY` on the listening socket and no longer on every accepted stream: Linux copies
  it to the accepted socket. Keepalive and DSCP settings are still applied per accepted stream.
- `utils/chunk_pool.rs` keeps up to eight 128 KiB buffers per thread; a taken buffer is empty with that capacity
  (it is filled by `read_buf`, as 0.9's body reader does). `protocols/http/v1/body.rs` takes its body
  buffer from it and returns it when the next body starts or the reader drops. `read_body_bytes` hands a
  content-length or close-delimited read of 8 KiB or more out as the buffer it was read into (the buffer returns to
  the pool of the thread that drops the last clone) instead of copying it into a new `Bytes`; chunked and smaller
  reads are copied as before. `finish_body_buf` only splits the buffer when bytes were read past the body.
  `oxiroute-server` also reads static file chunks into the same pool (it sizes the buffer first).
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
- `protocols/http/v1/common.rs` `RequestFacts` / `ResponseFacts`: while a head is parsed, the HTTP/1 server and client note
  whether it carries `Transfer-Encoding`, `Connection` and `Upgrade` and how many `Content-Length` headers it has, so the
  keepalive, framing, duplicate `Content-Length` and upgrade checks read flags instead of looking the headers up again. Header values are appended to
  the map after one validity check (`is_header_value`, eight bytes at a time) with no second copy.
  `protocols/http/v1/server.rs` keeps the facts in the session; `req_header_mut()` clears them (the caller may change
  what they describe) and `req_header_mut_keeping_facts()` keeps them. `protocols/http/server.rs` exposes both,
  `request_facts()` (`RequestFacts::UNKNOWN` for HTTP/2 and HTTP/3, which makes the caller look the headers up) and
  `modules/http/mod.rs` `HttpModuleCtx::is_empty`. `protocols/http/v1/client.rs` `write_request_header_noted` writes a
  borrowed request with the facts the caller already has (`write_request_header_ref` computes them).
  The facts also note whether a hop-by-hop field (`is_hop_by_hop`: `Keep-Alive`, `Proxy-Connection`, `Proxy-Authenticate`,
  `Proxy-Authorization`, `TE`, `Trailer`) is present (`hop_by_hop`), decided once per header name in the header-name cache,
  and `set_request_facts` (`ServerSession`, `HttpSession`; a no-op for sessions that answer `UNKNOWN`) records the facts of
  a request whose headers the caller changed and whose resulting facts it knows, so the product strips the hop-by-hop
  fields of a request once at its head and the session does not look the headers up again.
- `apps/mod.rs` lets an application put a connection-level front before the HTTP/1 session loop: `HttpServerApp::http1_front`
  returns an `Http1Front`, which sees the stream of a connection first and serves the requests it can by itself
  (`Http1Connection` carries the keepalive state and the request limit between its requests and the sessions'), then hands
  the connection back as `Http1FrontOutcome::Handoff` with the bytes of the next request head it already read. The loop builds
  the session with `protocols/http/server.rs` `new_http1_preloaded` and `protocols/http/v1/server.rs`
  `new_with_preloaded`, which sets the session's pipelined prefix: `read_request` parses the preloaded bytes before it reads from the stream, so nothing is read twice.
  Without a front the loop is the upstream one. `Http1Front::cleanup` runs with the application's cleanup.
- `protocols/http/v1/client.rs` can read a response head raw: `read_raw_head` reads to the first empty line with the limits, timeouts
  and errors of `read_response`, `accept_fast_head` takes the facts (`FastHead`, `FastFraming`) of a head the caller scanned
  itself as the session's response (status, version, `Connection`, framing; no `ResponseHeader` is built), and
  `reparse_raw_head` leaves the bytes to `read_response` for a head the caller does not take. Body reading, keepalive and reuse
  then work as for a parsed response.
- `protocols/http/v1/client.rs` `write_request_head_raw` writes a request head the caller rendered itself (a request without a body and
  without a `Connection` field) and leaves the session as the noted write leaves it. `protocols/http/v1/server.rs`
  `render_final_response_head` and `render_informational_head` render a response head as a session writes it (title-cased names,
  `Date` and `Connection` last for a final response) for a caller that writes the response itself.
- `connectors/http/mod.rs` keeps its HTTP/1 connector in an `Arc` and hands it out (`Connector::h1`; `HttpProxy::upstream_h1_connector` in
  `pingora-proxy`), so a caller that drives HTTP/1 upstream sessions itself reuses the idle connections of the proxy's pool.
  `validate_content_length_facts` gives upstream's Content-Length verdicts (identical duplicates are accepted and
  reconciled, conflicting or invalid values refused) but skips the header lookup when the facts rule `Content-Length`
  out. `RequestHeader` and `ResponseHeader` have no `DerefMut` in 0.9, so the parsers fill a plain `HeaderMap` (with the
  name cache) and `request_header_with_headers` / `ResponseHeader::from(Parts)` build the head around it.
- `protocols/http/v1/server.rs` `prepare_response_header` returns the serialised buffer so the inline `Date` and
  `Connection` lines and the reusable buffer survive upstream's proxy-task writer; response head bytes no longer count
  in `body_bytes_sent`, and `write_rendered_response` counts only the body (upstream's accounting).
- `apps/mod.rs` keeps `mark_reused_connection` / `is_reused_connection`; `services/mod.rs` `ServiceReadyNotifier` keeps
  `require_explicit` and `with_ready_callback` (the callback is `Send + Sync`); `PeerOptions` keeps `body_read_timeout`
  and the manual `Debug` that omits the TLS hook.
- `Cargo.toml` is the published 0.9.1 manifest plus `foreign-types` (OpenSSL feature), `smallvec`, the `openssl`
  dev-dependency, newer `lru`, `parking_lot`, `brotli`, `daggy`, `strum`, `x509-parser` where they compile unchanged,
  no `pingora-rustls` dependency and no `rustls` feature (the crate is no longer vendored; a lint table keeps
  `cfg(feature = "rustls")` code warning-free), and without the `h1_pipelining` bench (its source is not published).
  `sfv` stays at `0.10` because later releases change the parser and integer APIs used by compression.

- Tests: the unit tests no longer depend on the Internet, fixed ports, a stored key pair or host sysctls. `connectors/test_servers.rs`
  provides loopback servers that own a kernel-assigned port (HTTP redirect, TLS with HTTP/1.1 or HTTP/2, a refused address, a
  blackhole whose accept queue is full), `test_tls.rs` generates a throwaway self-signed certificate per test, and the listener tests
  bind port 0. The TLS tests (and the `openssl` dev-dependency that generates the certificate) cover the OpenSSL backend only.
  Streams coalesce writes, so a test server must flush. `scripts/test-vendored.sh` runs every vendored suite against these sources
  in a network namespace with only loopback.

The normalized crates.io `Cargo.toml` is retained. Its optional `Cargo.toml.orig` reference is
upstream packaging commentary; that non-build manifest is intentionally omitted here.

When upgrading Pingora, import the new published crate as its own commit, three-way merge each file from the
previous pristine release (`git merge-file ours base theirs`), and resolve by delta: the header slots, request and
response facts, noted request write, inline request loop, zero-copy request body and retry buffer,
`try_read_response_task`, connector hook, admission guard and handshake-timeout hook, rendered response write, inline
Date and Connection lines, header map headroom, single-clock timeouts, owned-preread, borrowed-request-write,
parsed-header ownership, response-task batch, lazy read buffer and coalescing writer, pooled gzip state, header-name
cache, body buffer pool, idle pool, listener `TCP_NODELAY`, TLS record coalescing and `Peek::poll_read_ready`.
Every upstream H1 correctness fix whose code our forks replace (`idle.rs`, `proxy_h1_inline.rs` in `pingora-proxy`,
the inline relay) must be re-derived by hand. Then run `scripts/test-vendored.sh`, the OxiRoute TLS and HTTP wire
tests and strict clippy.
