# Vendored pingora-core patch

This directory contains the published `pingora-core 0.8.1` source from crates.io
(registry checksum `6a7ffe2f5acf9f94fd255cfd1438866bc9124f8f0c7d42562bd3f853df2094b7`)
under its upstream Apache-2.0 license.

OxiRoute's delta is intentionally limited to an OpenSSL-derived, per-peer TLS configure hook,
pre-handshake application admission and per-application handshake timeout, a non-truncating certificate subject conversion, correct
HTTP/1 HEAD informational-response framing, owned HTTP/1 response preread payloads, borrowed HTTP/1
upstream request writes, parser-owned HTTP/1 header names, an inline HTTP response task batch, an
HTTP/2 test lifecycle correction, a lazily allocated read buffer, pooled gzip deflate state, a per-thread
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
- `utils/chunk_pool.rs` keeps up to eight 64 KiB buffers per thread. `protocols/http/v1/body.rs` takes its body
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
- `Cargo.toml` declares `prometheus 0.14`, `x509-parser 0.18`, `lru 0.19`, `daggy 0.9`,
  `strum`/`strum_macros 0.28`, `brotli 9`, and `parking_lot 0.13`, which compile unchanged. `sfv`
  stays at `0.10` because later releases change the parser and integer APIs used by compression.

The normalized crates.io `Cargo.toml` is retained. Its optional `Cargo.toml.orig` reference is
upstream packaging commentary; that non-build manifest is intentionally omitted here.

When upgrading Pingora, replace this directory from the newly locked published crate, reapply and
review the connector hook, admission guard and handshake-timeout hook, subject conversion, HTTP/1 HEAD informational-response,
owned-preread, borrowed-request-write, parsed-header ownership, response-task batch, lazy read buffer, pooled
gzip state, header-name cache, body buffer pool and hand-over, direct large body writes, coalescing write buffer,
HTTP/2 test lifecycle, and connection-pool changes, then rerun the vendored
connector pool, BodyReader, H1 client, and H2 server suites, OxiRoute TLS and HTTP wire tests, and strict clippy checks.
