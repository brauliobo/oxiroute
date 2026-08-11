# OxiRoute forward proxy foundation

This workspace crate defines explicit forward-proxy parsing, authorization, destination policy,
request sanitization, and bounded tunnel primitives. The daemon integrates HTTP/1.1 and HTTP/2
classic CONNECT plus a separate bounded `forward_http3` UDP listener.

```sh
cargo test -p oxiroute-forward-proxy --locked
cargo clippy -p oxiroute-forward-proxy --all-targets --all-features --locked -- -D warnings
```

## Protocol coverage

| Ingress | Forward request | Classic CONNECT | Wire evidence |
| --- | --- | --- | --- |
| HTTP/1.1 | Strict absolute-form URI; normalized origin-form output | Strict authority-form, including bracketed IPv6; upgraded bytes supported | Hyper HTTP/1 over TCP; CONNECT payload is sent with the headers and echoed after a real upgrade |
| HTTP/2 | Absolute URI reconstructed from `:scheme`, `:authority`, and `:path` | Authority-form CONNECT | `h2` client/server wire test and daemon TLS/ALPN test; CONNECT payload is relayed in DATA frames |
| HTTP/3 | Absolute URI reconstructed from QPACK pseudo-fields | Authority-only RFC 9114 classic CONNECT with DATA-frame tunnel relay | Quinn QUIC with TLS, negotiated `h3` ALPN, real QPACK HEADERS, bidirectional DATA, FIN, reset, rejection, and limit tests |

The table describes crate-level parser and tunnel primitives. The checked-in daemon's
`forward_http3` listener supports authority-only classic CONNECT plus bounded HTTP/HTTPS absolute-form
forwarding and eligible GET/HEAD memory or persistent caching. CONNECT-UDP remains HTTP/1-only.

The selected Rust-1.97-compatible H3 stack is `h3 0.0.8`, `h3-quinn 0.0.10`, and Quinn 0.11. The
workspace uses Rust `1.97.1` as its active MSRV. The
workspace manifest patches `h3` to verified upstream commit `e07e6941`, which is the focused
standard-CONNECT encoder fix later merged upstream: classic CONNECT omits `:scheme` and `:path` and
emits only `:method` and `:authority`. The patched crate declares Rust 1.74. Extended CONNECT is
explicitly rejected and is not used as a TCP CONNECT substitute.

## Security boundaries

- Parsing accepts only HTTP(S) absolute-form forwarding targets and explicit `host:port` CONNECT
  targets. User information, invalid DNS labels, unbracketed IPv6, zero ports, and ports above 65535
  fail closed.
- DNS runs through the injected `Resolver`. Policy evaluates every complete answer, and
  `ApprovedDestination` is an unforgeable, read-only capability carrying the destination and exact
  socket addresses the connector must use. A runtime reads them through `destination()` and
  `socket_addresses()` and must not connect to an address outside that approved set. The daemon's
  optional connect revalidation evaluates the final answer through the same policy before replacing
  the approved set.
- `ForbiddenDestinationPolicy` rejects localhost names, mixed public/private answers, and common
  non-public, special-use, mapped, NAT64, multicast, documentation, benchmark, and reserved IP
  ranges. Deployments can provide a stricter `DestinationPolicy`, including port or tenant rules.
- `DestinationRules` implements the canonical public-by-default contract: empty allow lists admit
  public destinations, denies override, and nonempty domain/CIDR lists independently constrain the
  requested DNS name and every address in the complete resolution result. Bounded UTC time windows
  can further constrain the destination; deny windows override allow windows.
- `ProxyAuthenticator` receives a borrowed credential wrapper that has no `Debug` or `Display`.
  Decisions retain only `Principal`; proxy authorization headers are removed before forwarding.
  Secret storage and credential comparison belong to the injected implementation.
- Canonical `mutual_tls` authentication is currently rejected during validation. The required
  interface is a listener TLS client-CA verifier that publishes a verified peer-certificate
  identity to the HTTP session; the current daemon forward listener does not expose that identity
  boundary, so it never treats an unverified certificate or header as proxy authentication.
- Sanitization removes `Connection`-nominated fields and standard hop-by-hop/proxy fields, then
  replaces `Host` with the normalized destination authority.
- `OverreadIo` replays bytes consumed beyond H1 headers before touching the socket. This is required
  when CONNECT payload arrives in the same read as its request.
- `BoundedTunnel` enforces finite per-direction bytes, idle time, lifetime, and buffer allocation.
  `relay` handles byte streams, `relay_h2` applies bounded DATA-frame relay to HTTP/2 streams, and
  `relay_h3` applies the same coordinator and accounting to H3 DATA frames, including FIN
  half-close and reset cancellation.
  Tunnel outcomes have stable labels for EOF, byte limits, timeouts, I/O failure, and cancellation.
  The runtime remains responsible for connection concurrency, aggregate bandwidth, audit logging,
  shutdown, and mapping typed failures to safe HTTP responses.
- With `audit_mode = "metadata"`, the daemon emits redacted JSON forward-access events containing
  only normalized authority, method, protocol, outcome, status/reason, authentication state, and
  client IP. The compiled plan also exposes saturating access-result counters; credentials and raw
  request targets are never retained in either surface.

## Canonical destination time grammar

Forward destination rules use UTC and half-open `start`/`end` minute windows. `end` may be `24:00`;
overnight windows must be split at midnight. A Lua example is:

```lua
destination_policy = {
  allow_domains = { "example.com", "*.example.net" },
  allow_times = {
    { days = { "monday", "friday" }, start = "09:00", ["end"] = "17:00" },
  },
  deny_times = {
    { days = { "monday" }, start = "12:00", ["end"] = "13:00" },
  },
}
```

Both time lists are bounded to 256 normalized ranges. Domains are ASCII exact names or one-label
wildcards, and the resolved-address policy still applies to every address in the final answer.

## Pingora integration

The existing runtime registers reverse HTTP traffic through `pingora::proxy::http_proxy` and
`HttpReverseProxy`. Forward proxying must be a separate service/listener path, not another route in
that reverse-proxy implementation:

1. Add a distinct forward-proxy configuration/service kind and listener admission limits.
2. Implement a dedicated `HttpServerApp` that applies the shared authorization and destination
   policy path. HTTP/2 classic CONNECT uses Pingora's per-stream body operations and never takes
   ownership of the shared H2 connection. H3 malformed classic CONNECT shapes must reset the
   stream with `H3_MESSAGE_ERROR` when `DecisionError::InvalidHttp3` is returned.
3. Inject production authentication, DNS, and destination policy implementations. Connect only an
   address from `ApprovedDestination::socket_addresses()`, retaining that selected address for audit.
4. For forwarding, create an upstream session from the approved address, use the normalized
   origin-form target and sanitized headers, and select upstream TLS from `ForwardScheme`.
5. For H1 CONNECT, preserve the post-2xx underlying stream and parser over-read bytes without
   routing CONNECT through the reverse proxy's 101 Upgrade path.
6. For H2 CONNECT, adapt Pingora's per-stream body read/write operations to the shared bounded
   tunnel accounting. The relay propagates DATA end-stream, upstream half-close, flow control,
   byte limits, idle/lifetime timeouts, and stream reset cancellation.
7. Register Quinn/H3 as a separate UDP listener because Pingora's current HTTP app path covers H1
   and H2 only. The daemon now performs this registration with shared listener reservation,
   generation, metrics, TLS, and drain ownership. After a successful tunnel decision and upstream
   connection, send the 2xx response and pass the H3 request stream plus the connected upstream
   stream to `BoundedTunnel::relay_h3`. Map ordinary decision failures through
   `DecisionError::rejection` before opening an upstream.

Reverse proxy pools, retries, route matching, upstream health selection, and configured `HttpPeer`
targets must not be reused for arbitrary forward destinations. Shared listener metrics and TLS
profile machinery can be integrated at the service boundary without merging the two trust models.

## Dependencies

Runtime foundation dependencies are `async-trait`, `bytes`, `h3`, `http`, `thiserror`, and `tokio`.
Wire-test dependencies are `hyper`/`hyper-util` for H1, `h2`, and
`quinn`/`rustls`/`h3`/`h3-quinn` for H3; `rcgen` creates ephemeral test certificates only. The
workspace lockfile pins the Rust-1.97-compatible resolution, including the current `time` release.
