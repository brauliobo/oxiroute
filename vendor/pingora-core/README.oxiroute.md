# Vendored pingora-core patch

This directory contains the published `pingora-core 0.8.1` source from crates.io
(registry checksum `6a7ffe2f5acf9f94fd255cfd1438866bc9124f8f0c7d42562bd3f853df2094b7`)
under its upstream Apache-2.0 license.

OxiRoute's delta is intentionally limited to an OpenSSL-derived, per-peer TLS configure hook,
pre-handshake application admission, a non-truncating certificate subject conversion, correct
HTTP/1 HEAD informational-response framing, owned HTTP/1 response preread payloads, borrowed HTTP/1
upstream request writes, an inline HTTP response task batch, an HTTP/2 test lifecycle correction,
and stage-1 connection-pool experiments:

- `protocols/tls/mod.rs` declares the public `TlsConfigureHook` type.
- `upstreams/peer.rs` stores the optional hook in `PeerOptions`, omits it from
  `Debug`, and exposes it through `Peer`.
- `connectors/tls/boringssl_openssl/mod.rs` invokes it after CA, verification,
  hostname, and ALPN setup, but before clearing the OpenSSL error stack and
  starting the handshake. Hook failures become `TLSHandshakeFailure` errors.
- `apps/mod.rs` exposes an opaque connection-admission guard, and
  `services/listening.rs` acquires it after TCP accept and retains it through the handshake and
  application connection lifetime.
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

The normalized crates.io `Cargo.toml` is retained. Its optional `Cargo.toml.orig` reference is
upstream packaging commentary; that non-build manifest is intentionally omitted here.

When upgrading Pingora, replace this directory from the newly locked published crate, reapply and
review the connector hook, admission guard, subject conversion, HTTP/1 HEAD informational-response,
owned-preread, borrowed-request-write, response-task batch, HTTP/2 test lifecycle, and connection-pool
changes, then rerun the vendored connector pool, BodyReader, H1 client, and H2 server suites,
OxiRoute TLS and HTTP wire tests, and strict clippy checks.
