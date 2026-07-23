# OxiRoute forward proxy foundation

This standalone crate defines explicit forward-proxy parsing, authorization, destination policy,
request sanitization, and tunnel primitives. It is intentionally outside the root Cargo workspace
for now; all commands use its manifest directly.

```sh
cargo test --manifest-path crates/oxiroute-forward-proxy/Cargo.toml
cargo clippy --manifest-path crates/oxiroute-forward-proxy/Cargo.toml --all-targets -- -D warnings
```

## Protocol coverage

| Ingress | Forward request | Classic CONNECT | Wire evidence |
| --- | --- | --- | --- |
| HTTP/1.1 | Strict absolute-form URI; normalized origin-form output | Strict authority-form, including bracketed IPv6; upgraded bytes supported | Hyper HTTP/1 over TCP; CONNECT payload is sent with the headers and echoed after a real upgrade |
| HTTP/2 | Absolute URI reconstructed from `:scheme`, `:authority`, and `:path` | Authority-form CONNECT | `h2` client/server over TCP; CONNECT payload is echoed in DATA frames |
| HTTP/3 | Absolute URI reconstructed from QPACK pseudo-fields | Authority-only RFC 9114 classic CONNECT with DATA-frame tunnel relay | Quinn QUIC with TLS, negotiated `h3` ALPN, real QPACK HEADERS, bidirectional DATA, FIN, reset, rejection, and limit tests |

The selected Rust-1.87-compatible H3 stack is `h3 0.0.8`, `h3-quinn 0.0.10`, and Quinn 0.11. The
standalone manifest patches `h3` to verified upstream commit `e07e6941`, which is the focused
standard-CONNECT encoder fix later merged upstream: classic CONNECT omits `:scheme` and `:path` and
emits only `:method` and `:authority`. The patched crate declares Rust 1.74. Extended CONNECT is
explicitly rejected and is not used as a TCP CONNECT substitute.

## Security boundaries

- Parsing accepts only HTTP(S) absolute-form forwarding targets and explicit `host:port` CONNECT
  targets. User information, invalid DNS labels, unbracketed IPv6, zero ports, and ports above 65535
  fail closed.
- DNS runs through the injected `Resolver`. Policy evaluates the complete answer once, and
  `ApprovedDestination` carries the exact socket addresses the connector must use. A runtime must
  not resolve the hostname again after approval.
- `ForbiddenDestinationPolicy` rejects localhost names, mixed public/private answers, and common
  non-public, special-use, mapped, NAT64, multicast, documentation, benchmark, and reserved IP
  ranges. Deployments can provide a stricter `DestinationPolicy`, including port or tenant rules.
- `ProxyAuthenticator` receives a borrowed credential wrapper that has no `Debug` or `Display`.
  Decisions retain only `Principal`; proxy authorization headers are removed before forwarding.
  Secret storage and credential comparison belong to the injected implementation.
- Sanitization removes `Connection`-nominated fields and standard hop-by-hop/proxy fields, then
  replaces `Host` with the normalized destination authority.
- `OverreadIo` replays bytes consumed beyond H1 headers before touching the socket. This is required
  when CONNECT payload arrives in the same read as its request.
- `BoundedTunnel` enforces finite per-direction bytes, idle time, lifetime, and buffer allocation.
  `relay` handles byte streams and `relay_h3` applies the same coordinator and accounting to H3
  DATA frames, including FIN half-close and reset cancellation.
  The runtime remains responsible for connection concurrency, aggregate bandwidth, audit logging,
  shutdown, and mapping typed failures to safe HTTP responses.

## Pingora integration

The existing runtime registers reverse HTTP traffic through `pingora::proxy::http_proxy` and
`HttpReverseProxy`. Forward proxying must be a separate service/listener path, not another route in
that reverse-proxy implementation:

1. Add a distinct forward-proxy configuration/service kind and listener admission limits.
2. Implement a dedicated `HttpServerApp` that passes framed requests to
   `ForwardProxy::decide_request`. Preserve an exact raw H1 target through `IncomingRequest` only
   when the HTTP stack cannot retain it. H3 malformed classic CONNECT shapes must reset the stream
   with `H3_MESSAGE_ERROR` when `DecisionError::InvalidHttp3` is returned.
3. Inject production authentication, DNS, and destination policy implementations. Connect only an
   address from `ApprovedDestination.socket_addresses`, retaining that selected address for audit.
4. For forwarding, create an upstream session from the approved address, use the normalized
   origin-form target and sanitized headers, and select upstream TLS from `ForwardScheme`.
5. For H1 CONNECT, verify that Pingora can return the post-2xx underlying stream and its parser
   over-read bytes without treating CONNECT as the reverse proxy's 101 Upgrade path. The current
   public `ServerSession` API exposes raw H1 headers and body methods but does not expose a clear
   successful-CONNECT stream takeover; this likely needs a focused Pingora-core API extension.
6. For H2 CONNECT, adapt Pingora's per-stream body read/write operations to the shared bounded
   tunnel accounting. Never take ownership of the shared H2 connection.
7. Register Quinn/H3 as a separate UDP listener because Pingora's current HTTP app path covers H1
   and H2 only. After a successful tunnel decision and upstream connection, send the 2xx response
   and pass the H3 request stream plus the connected upstream stream to `BoundedTunnel::relay_h3`.
   Map ordinary decision failures through `DecisionError::rejection` before opening an upstream.

Reverse proxy pools, retries, route matching, upstream health selection, and configured `HttpPeer`
targets must not be reused for arbitrary forward destinations. Shared listener metrics and TLS
profile machinery can be integrated at the service boundary without merging the two trust models.

## Dependencies

Runtime foundation dependencies are `async-trait`, `bytes`, `h3`, `http`, `thiserror`, and `tokio`.
Wire-test dependencies are `hyper`/`hyper-util` for H1, `h2`, and
`quinn`/`rustls`/`h3`/`h3-quinn` for H3; `rcgen` creates ephemeral test certificates only. The
standalone `Cargo.lock` pins the Rust-1.87-compatible resolution, including `time 0.3.45`.
