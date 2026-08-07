# OxiRoute 0.4.1

OxiRoute 0.4.1 adds opt-in RFC 9298 `CONNECT-UDP` support to the HTTP/1.1 forward proxy while
keeping existing CONNECT behavior and defaults unchanged. The documented and tested scope is
intentionally H1-only; H2 and H3 forward listeners expose authority-only classic CONNECT, and
arbitrary forward request forms remain bounded or unsupported.

## Highlights

- Add bounded Capsule Protocol DATAGRAM relay to connected UDP destinations.
- Add separate typed configuration and policy controls for allowed CONNECT-UDP ports.
- Expose the feature through validation, topology, native-import defaults, and the Vue dashboard.

## Configuration

Enable the feature on a forward service with an H1 version and an explicit port allowlist:

```lua
enabled_versions = { "h1" },
connect_udp = { enabled = true, allowed_ports = { 443 } },
```

The policy is honored by `forward_http1` only. H2/H3 CONNECT-UDP is not enabled by this setting.

## Evidence Boundary

Configuration validation/rendering and real HTTP/1 wire tests cover the upgrade, Capsule DATAGRAM
relay, malformed framing, and port policy. This release does not claim positive H3 absolute-form
forwarding, external CA-staging/production ACME evidence, or long-running fuzz and active-traffic
evidence.
