# Vendored pingora-core patch

This directory contains the published `pingora-core 0.8.1` source from crates.io
(registry checksum `6a7ffe2f5acf9f94fd255cfd1438866bc9124f8f0c7d42562bd3f853df2094b7`)
under its upstream Apache-2.0 license.

OxiRoute's delta is intentionally limited to an OpenSSL-derived, per-peer TLS
configure hook, pre-handshake application admission, a non-truncating certificate subject
conversion, correct HTTP/1 HEAD informational-response framing, and owned HTTP/1 response preread
payloads, plus borrowed HTTP/1 upstream request writes:

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

The normalized crates.io `Cargo.toml` is retained. Its optional `Cargo.toml.orig` reference is
upstream packaging commentary; that non-build manifest is intentionally omitted here.

When upgrading Pingora, replace this directory from the newly locked published
crate, reapply and review the connector hook, admission guard, subject conversion, and HTTP/1
HEAD informational-response, owned-preread, and borrowed-request-write changes, then rerun the
vendored BodyReader and H1 client suites, OxiRoute TLS and HTTP wire tests, and strict clippy checks.
