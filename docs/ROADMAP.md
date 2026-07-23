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
- Canonical listener, HTTP/L4 service, route, and static pool subset.
- HTTP body and upstream I/O limits plus per-listener connection caps.
- Bounded connect-failure retries for replay-safe HTTP requests.
- Red-green acceptance tests for configuration and runtime planning.

This milestone proves the dependency and configuration seams. It is not a release.

## Milestone 1: small useful release

Target one Linux daemon and one canonical config file.
Status: partial. The annotations below identify landed slices; Milestone 1 is not complete.

- HTTP/1 reverse proxy with TLS termination and upstream TLS (strict file-backed downstream TLS
  and verified upstream TLS/SNI/custom CA are implemented and wire-tested).
- HTTP/2 downstream and upstream where Pingora supports it (TLS/ALPN downstream H2, explicit
  upstream version policies, and gRPC DATA/trailers are implemented and wire-tested; broader
  conformance remains).
- Raw TCP relay with correct half-close, timeout, cancellation, and backpressure tests.
- Tagged socket/DNS/Unix upstreams with round-robin and least-connections selection, plus active
  TCP/HTTP health checks for non-Unix pools (implemented; Unix transports require Unix).
- Request/connection limits, structured access logs, and Prometheus metrics (limits are
  implemented; structured access logs and Prometheus exposition remain).
- Prepare-then-activate configuration reload; invalid candidates leave the prior generation active
  (complete candidate preflight and durable revision-checked save are implemented, but changed
  saves require process restart and are not activated live).
- Parent-directory file watcher with content-hash revisions (not implemented for the canonical
  config; the Certbot lineage watcher is separate).
- ACME HTTP-01 certificate issuance, renewal scheduling, and zero-downtime activation (managed ACME
  remains absent; external Certbot lineage rotation is implemented).
- Imported PEM and generated self-signed certificates for development and bootstrap use (strict
  imported-PEM startup preparation and the atomic callback-publication seam are implemented;
  file reload/API activation and self-signed generation remain).
- Loopback-only management API by default (monitoring/topology/RTMP plus bearer-authenticated config
  read, validation, preview, and durable write are implemented; SSE is absent).
- Minimal Vue 3 SPA using build-time Pug SFC templates (implemented).
- UI for listeners, upstreams, validation diagnostics, active revision, and basic metrics (the
  complete current canonical-field workspace and runtime observatory are implemented; certificate,
  import, and event workflows remain).

Explicitly exclude UDP, forward proxying, caching, HTTP/3, native config importers,
transparent interception, firewall management, and remote multi-user administration.

Release gates:

- Full-duplex and half-close TCP integration tests.
- HTTP request/body, WebSocket, timeout, retry, and graceful-drain tests.
- Active health-check transition, scheduling, concurrency-bound, and unavailable-pool tests.
- Independent TLS/H1/H2, exact/wildcard/default SNI identity selection, upstream
  verification/version-policy, gRPC trailer, and per-identity certificate-generation publication
  wire tests (implemented for the current narrow slice).
- Invalid reload and listener-bind failure tests.
- API revision-conflict and explicit external-file-change detection tests (implemented; no watcher
  or SSE reconnect path exists).
- ACME staging-directory issuance, renewal, failed-challenge, and rollback tests.
- No root requirement and no outbound destination inferred from request input.

## Milestone 2: import and layer-4 breadth

Audited migration cases and implementation progress are tracked in
[`HOST_CONFIG_COVERAGE.md`](HOST_CONFIG_COVERAGE.md). A case is complete only after canonical,
runtime, failure-path, test, and native-lowering coverage all land.

- Canonical listener, HTTP service, route, upstream pool, TLS profile, certificate, and L4 service
  model (implemented for the current strict subset; importer provenance and broader policy remain).
- nginx importer for a static HTTP and stream subset (bounded source/include and HTTP semantic
  reports exist, but HTTP remains draft-only and stream lowering is absent).
- HAProxy importer for static HTTP/TCP frontends and backends (ordered roots, semantic resolution,
  and strict static TCP finalization for socket/Unix binds, socket/DNS/Unix servers, and
  `roundrobin`/`leastconn` exist; HTTP and audited host candidates remain blocked).
- Apache virtual-host importer for static HTTP proxy rules.
- Native source locations, include graphs, decision ledgers, provenance, and stable diagnostic
  codes (partial for nginx and HAProxy; capability profiles and other products remain).
- UDP relay with bounded pseudo-sessions, per-client reply mapping, and expiry.
- PROXY protocol v1/v2 for explicit client-address propagation.
- Least-connections policy (implemented); weighted round-robin remains.
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

1. Register and validate all 117 active nginx-rtmp directives with lossless raw values and
   runtime-support status (registry/context validation implemented; deterministic includes,
   inheritance, occurrence accounting, provenance, and canonical finalization implemented for a
   strict listener/application/recording subset; broad lowering remains).
2. Implement handshake, chunk transport, AMF0 connect/createStream, live publish/play,
   metadata/codec headers, keyframe gating, and bounded fanout (narrow live listener path
   implemented; broader conformance remains).
3. Add access, callbacks, push/pull relay, FLV recording, VOD, statistics, control, and logging
   (canonical continuous/manual legacy AVC/AAC FLV recording, session dispatch, storage, bounded
   workers/reaping, observability, and exact-ID controls are integrated; enhanced codec recording,
   access, callbacks, relay, VOD, statistics parity, authenticated remote control, and logging
   remain).
4. Add HLS, MPEG-DASH, isolated exec workers, limits, and multi-worker equivalents.

Each remaining slice will begin with protocol/configuration failures and differential fixtures
against the cloned nginx-rtmp module. Directive parsing does not count as runtime feature parity.
