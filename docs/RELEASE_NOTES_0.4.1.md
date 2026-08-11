# OxiRoute 0.4.1

OxiRoute 0.4.1 adds opt-in RFC 9298 `CONNECT-UDP` support to the HTTP/1.1 forward proxy and completes
the shared cache transaction lifecycle for reverse and forward HTTP/3. CONNECT-UDP remains H1-only;
H3 now has bounded absolute-form forwarding plus eligible GET/HEAD memory or persistent caching.

## Highlights

- Add bounded Capsule Protocol DATAGRAM relay to connected UDP destinations.
- Add separate typed configuration and policy controls for allowed CONNECT-UDP ports.
- Add reverse and forward H3 cache lookup, collapsed fills, revalidation, stale-if-error, bounded
  admission, authenticated purge, and listener cache outcomes.
- Add independent reverse and forward H3 wire evidence proving second-request cache reuse without
  second-origin contact.
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
relay, malformed framing, and port policy. Independent QUIC/H3 tests cover bounded absolute-form
forwarding and reverse/forward cache reuse. This release does not claim H2/H3 CONNECT-UDP, H3
compression/upgrades, external CA-staging/production ACME evidence, or long-running fuzz and
active-traffic evidence.
