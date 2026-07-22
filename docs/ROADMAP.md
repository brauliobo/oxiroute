# Roadmap

## Product boundary

OxiRoute should become a proxy control plane, not a universal packet processor. Its own
typed model is the source of truth. Native configuration formats are import adapters, and
the UI edits the same model through a revisioned API.

The priorities are safe reloads, correct protocol behavior, diagnostics, and observable
limits. Feature count comes after those invariants.

## Milestone 0: executable skeleton

Status: implemented.

- Rust 1.87 workspace using Edition 2024 language semantics and resolver v3.
- Restricted, text-only Lua configuration with no standard libraries.
- Source, memory, and instruction limits.
- Strict config decoding and validation.
- Static HTTP reverse proxy through Pingora.
- Static opaque TCP relay through Pingora.
- Red-green acceptance tests for configuration and runtime planning.

This milestone proves the dependency and configuration seams. It is not a release.

## Milestone 1: small useful release

Target one Linux daemon and one canonical config file.

- HTTP/1 reverse proxy with TLS termination and upstream TLS.
- HTTP/2 downstream and upstream where Pingora supports it.
- Raw TCP relay with correct half-close, timeout, cancellation, and backpressure tests.
- Static round-robin upstream pools and active TCP/HTTP health checks.
- Request/connection limits, structured access logs, and Prometheus metrics.
- Prepare-then-activate configuration reload; invalid candidates leave the prior generation active.
- Parent-directory file watcher with content-hash revisions.
- ACME HTTP-01 certificate issuance, renewal scheduling, and zero-downtime activation.
- Imported PEM and generated self-signed certificates for development and bootstrap use.
- Loopback-only management API by default.
- Minimal Vue 3 SPA using build-time Pug SFC templates.
- UI for listeners, upstreams, validation diagnostics, active revision, and basic metrics.

Explicitly exclude UDP, forward proxying, caching, HTTP/3, native config importers,
transparent interception, firewall management, and remote multi-user administration.

Release gates:

- Full-duplex and half-close TCP integration tests.
- HTTP request/body, WebSocket, timeout, retry, and graceful-drain tests.
- Invalid reload and listener-bind failure tests.
- API revision-conflict and external-file-change tests.
- ACME staging-directory issuance, renewal, failed-challenge, and rollback tests.
- No root requirement and no outbound destination inferred from request input.

## Milestone 2: import and layer-4 breadth

- Canonical listener, HTTP service, route, upstream pool, TLS profile, and L4 service model.
- nginx importer for a static HTTP and stream subset.
- HAProxy importer for static HTTP/TCP frontends and backends.
- Apache virtual-host importer for static HTTP proxy rules.
- Native source locations, include graphs, capability profiles, and stable diagnostic codes.
- UDP relay with bounded pseudo-sessions, per-client reply mapping, and expiry.
- PROXY protocol v1/v2 for explicit client-address propagation.
- Least-connections and weighted round-robin policies.
- ACME DNS-01 through isolated provider plugins and wildcard certificate support.

Imports are not successful when behavior is ignored. Unsupported routing, TLS, ACL, or
listener semantics must block the affected service.

## Milestone 3: explicit forward proxy

- Dedicated HTTP/1 absolute-form parser and CONNECT tunnel with over-read preservation.
- Default-deny ACL engine for identity, source, destination, method, port, and time.
- DNS and resolved-IP egress policy to prevent open-proxy and SSRF behavior.
- Static or mTLS proxy authentication first.
- Squid importer for the independently implemented supported subset.
- HTTP/2 CONNECT only after stream takeover semantics have dedicated conformance tests.

Defer TLS interception, transparent proxying, ICAP/eCAP, NTLM/Negotiate helpers, cache
peer protocols, and broad Squid helper compatibility.

## Milestone 4: cache and HTTP/3

- Production cache storage designed as a separate component with recovery and eviction tests.
- Cache freshness, revalidation, locking, range, purge, and observability behavior.
- QUIC/H3 frontend selected through a proof of compatibility with Pingora's service model.
- HTTP/3 conformance, migration, timeout, 0-RTT policy, and UDP resource-exhaustion tests.

HTTP/3 must be advertised only when the active listener is actually QUIC-capable. It must
never degrade silently to another HTTP version.

## Future kernel integration

If transparent traffic handling becomes necessary, build a separate privileged Linux
helper with a narrow API and explicit nftables/policy-routing ownership. Keep it optional
and out of the proxy process. Do not generate or reconcile arbitrary firewall rules from
the main daemon.

## RTMP workstream

RTMP proceeds in independently releasable slices rather than waiting for HTTP/Squid parity:

1. Register and validate all 117 active nginx-rtmp directives with lossless raw values and runtime-support status.
2. Implement handshake, chunk transport, AMF0 connect/createStream, live publish/play, metadata/codec headers, keyframe gating, and bounded fanout.
3. Add access, callbacks, push/pull relay, FLV recording, VOD, statistics, control, and logging.
4. Add HLS, MPEG-DASH, isolated exec workers, limits, and multi-worker equivalents.

Every slice begins with protocol/configuration failures and differential fixtures against
the cloned nginx-rtmp module. Directive parsing does not count as runtime feature parity.
