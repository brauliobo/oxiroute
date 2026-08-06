# Roadmap

## Product boundary

OxiRoute should become a proxy control plane, not a universal packet processor. Its own
typed model is the source of truth. Native configuration formats are import adapters, and
the UI edits the same model through a revisioned API.

The priorities are safe reloads, correct protocol behavior, diagnostics, and observable
limits. Feature count comes after those invariants.

The package currently declares `0.4.0`. This roadmap describes the current pre-alpha working
release line, not a 1.0 commitment.

Roadmap status is not current support. `stable` means part of the current narrow release
contract; `partial` means an integrated slice still has compatibility or production gates;
`foundation` means tested code that is not active in the daemon; `planned` means committed future
work; `research` needs a product or design decision; `not-planned` is a deliberate exclusion; and
`out-of-scope` belongs to another product boundary.

## Milestone 0: executable skeleton

Status: foundation. The skeleton is implemented and tested, but it is not a release boundary.

- Rust 1.87 workspace using Edition 2024 language semantics and resolver v3.
- KDL 2.0 as the default configuration format, plus restricted, text-only Lua compatibility
  configuration with no standard libraries.
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
- Tagged socket/DNS/Unix upstreams with round-robin, weighted round-robin, and least-connections
  selection, plus active TCP/HTTP health checks for non-Unix pools (implemented; Unix transports
  require Unix).
- Request/connection limits, structured access logs, and Prometheus metrics (limits are
  implemented; HTTP structured JSONL access logs and Prometheus exposition are implemented for
  the current narrow surface, while protocol breadth and richer log contracts remain partial).
- Prepare-then-activate configuration reload; invalid candidates leave the prior generation active
  (complete candidate preflight, durable revision-checked save, parent-directory watching,
  periodic reconciliation, and in-process generation activation are implemented; broader
  active-traffic drain evidence remains a gate).
- Parent-directory file watcher with content-hash revisions (implemented for the canonical root,
  including periodic re-resolution of native references; direct watches for every resolved native
  dependency and richer dependency registration remain planned).
- ACME HTTP-01/DNS-01/TLS-ALPN-01 certificate issuance, wildcard renewal scheduling, and zero-downtime
  activation (bounded managed authenticators are implemented; live staging evidence and broader
  providers remain).
- Imported PEM and generated self-signed certificates for development and bootstrap use (strict
  imported-PEM startup preparation, self-signed generation, in-memory ACME bootstrap, Certbot
  reconciliation, and atomic callback-publication are implemented; direct-file API activation and
  reload remain).
- Loopback-only management API by default (recognized `/api/v1` routes use the bearer token;
  exact `GET /ready` and `GET /metrics` are the public probes; monitoring, topology, RTMP,
  generation, TLS, process, listener, pool, server, config, bounded event polling, and SSE routes
  are implemented or partial as documented).
- Minimal Vue 3 SPA using build-time Pug SFC templates (implemented).
- UI for listeners, upstreams, validation diagnostics, active revision, basic metrics, managed
  certificate configuration, operations, bounded events, and source provenance (the current
  runtime observatory and canonical-field workspace are implemented; passive-health/retry controls
  are exposed, while weighted-round-robin weight editing, native import editing, TLS-ALPN challenge
  selection, and durable audit browsing are not frontend surfaces; the event view remains the
  non-durable operational ring).

The original Milestone 1 boundary explicitly excluded UDP, forward proxying, caching, HTTP/3,
native config importers, transparent interception, firewall management, and remote multi-user
administration. The current `0.4.0` working release has partial HTTP/1 and HTTP/3 proxying, UDP
relay, bounded HTTP caching, and bounded nginx, HAProxy, Apache, Squid, and Varnish import paths;
that progress does not promote those subsets to complete compatibility or make foundations active
capabilities. Transparent interception, firewall management, and remote multi-user administration
remain outside the current release contract.

Release gates:

- Full-duplex and half-close TCP integration tests.
- HTTP request/body, WebSocket, timeout, retry, and graceful-drain tests.
- Active health-check transition, scheduling, concurrency-bound, and unavailable-pool tests.
- Independent TLS/H1/H2, exact/wildcard/default SNI identity selection, upstream
  verification/version-policy, gRPC trailer, and per-identity certificate-generation publication
  wire tests (implemented for the current narrow slice).
- Invalid reload and listener-bind failure tests.
- API revision-conflict and explicit external-file-change detection tests (implemented; the
  canonical watcher, bounded event polling, SSE reconnect, and frontend resynchronization exist).
- ACME staging-directory issuance, renewal, failed-challenge, and rollback tests.
- No root requirement and no outbound destination inferred from request input.
- Active-traffic reload and drain across long-lived HTTP, H2, H3, TCP, UDP, RTMP, and SSE
  connections, including no-new-work behavior after GOAWAY/quiesce and old-generation retention.
- CA-staging HTTP-01, DNS-01, and TLS-ALPN-01 issuance and renewal with listener deployment,
  cleanup, failure, rollback, and real certificate activation evidence.
- Independent interoperability for H3/TLS and FFmpeg/OBS RTMP paths, plus representative Apache,
  HAProxy, Squid, and Varnish migration cases beyond synthetic fixtures.
- Bounded runs of every checked-in fuzz target, crash-corpus triage, and fault injection for media,
  exec, reload, and supervision workers.
- Packaged production supervision with active UDP/H3 replacement, rollback, drain, restart, and
  crash recovery. The direct `serve` path remains the default until this gate closes.

## Milestone 2: import and layer-4 breadth

Status: partial. Strict nginx, HAProxy, Apache, Squid, Varnish, and nginx-RTMP subsets are implemented,
including complete-root nginx HTTP+RTMP composition through canonical native references; UDP and
PROXY runtime breadth, broader importer semantics, and complete migration evidence remain.

Audited migration cases and implementation progress are tracked in
[`HOST_CONFIG_COVERAGE.md`](HOST_CONFIG_COVERAGE.md). A case is complete only after canonical,
runtime, failure-path, test, and native-lowering coverage all land.

- Canonical listener, HTTP service, route, upstream pool, TLS profile, certificate, and L4 service
  model (implemented for the current strict subset; importer provenance and broader policy remain).
- nginx importer for a static HTTP and nginx-RTMP subset (the explicit HTTP-fragment API
  conditionally finalizes a strict proxy/fixed/redirect subset; `import_root` composes complete
  nginx HTTP and RTMP roots, and KDL/HOCON/UCI native references can resolve the finalized result
  into a watched runtime generation; stream lowering, broader semantics, and audited candidates
  remain partial or blocked).
- HAProxy importer for static HTTP/TCP frontends and backends (ordered roots, semantic resolution,
  strict socket/DNS/Unix transport lowering, HTTP balancing/routing/retry/health policy, and the
  audited live hostrouter candidate now finalize; broader ACLs, redispatch interval forms,
  unsupported stats forms, and server policy remain blocked, while log/process policy is a
  deployment warning).
- Apache virtual-host importer for static HTTP/HTTPS proxy rules, inherited defaults, exact
  authorities, equal-weight balancers, deterministic optional includes, and blocking diagnostics
  (integrated for the strict subset; broader httpd semantics remain blocked).
- Native source locations, include graphs, decision ledgers, provenance, capability profiles, and
  stable diagnostic codes (partial for the current nginx, HAProxy, Apache, Squid, and Varnish
  subsets; broader product semantics remain).
- UDP relay with bounded pseudo-sessions, per-client reply mapping, and expiry.
- Bounded PROXY protocol v1/v2 for explicit client-address propagation is implemented; broader wire
  conformance remains.
- Least-connections and weighted round-robin policies are implemented and tested; the weighted
  canonical/runtime path is current, while native importer lowering and frontend weight editing
  remain outside the release surface.
- ACME DNS-01/TLS-ALPN-01 through bounded authenticators and wildcard certificate support (the
  provider seam, challenge orchestration, and cleanup paths are implemented; concrete provider and
  CA-staging deployments remain).

Imports are not successful when behavior is ignored. Unsupported routing, TLS, ACL, or
listener semantics must block the affected service.

## Milestone 3: explicit forward proxy

Status: partial. The HTTP/1, HTTP/2 classic CONNECT, forward HTTP/3, and reverse HTTP/3 daemon paths
are integrated; arbitrary HTTP/2 forwarding remains outside the daemon contract.

- Dedicated HTTP/1 absolute-form parser and bounded CONNECT tunnel with over-read preservation
  (integrated for the HTTP/1 daemon listener; broader conformance remains).
- Ordered first-match ACL engine for identity, source, destination, method, and port (integrated for
  the audited Squid subset); canonical destination domains and bounded UTC time windows are also
  available, while imported time/domain ACL lowering and helper-backed predicates remain.
- DNS and resolved-IP egress policy to prevent open-proxy and SSRF behavior (integrated with bounded
  custom/system resolution, optional connect revalidation, and exact final-answer approved-address
  connection across address-family retries).
- Static or mTLS proxy authentication first (Bearer and bounded bcrypt/APR1 htpasswd Basic are
  integrated; canonical mTLS fails closed until the listener TLS client-CA/session identity seam is
  available).
- Squid importer for the independently implemented supported subset (bounded source/parser/typed
  semantics, canonical direct-forward lowering, ordered static parent peers, global direct-fallback
  rules, CLI/native references, and daemon runtime are integrated; cache/refresh semantics remain
  explicitly non-equivalent).
- HTTP/2 classic CONNECT with dedicated stream takeover, flow-control, half-close, timeout, reset,
  and cancellation conformance coverage (integrated; arbitrary H2 forward requests remain blocked).
- Bounded forward HTTP/3 absolute-form and classic CONNECT through a separate UDP listener, with
  TLS 1.3/`h3` negotiation, shared forward policy, and generation-aware drain (integrated; broader
  conformance remains).
- Opt-in bounded memory/persistent caching for eligible HTTP/1 forward GET/HEAD requests, with
  collapsed fills, origin revalidation, fail-closed privacy admission, authenticated purge, and
  listener cache outcome metrics (integrated; broader HTTP cache conformance remains).

Defer TLS interception, transparent proxying, ICAP/eCAP, NTLM/Negotiate helpers, sibling/dynamic
peer protocols, ICP/HTCP, and broad Squid helper compatibility.

## Milestone 4: cache and HTTP/3

Status: partial; bounded reverse HTTP and eligible HTTP/1 forward cache integrations are active.
Both H3 directions remain partial capabilities pending broader conformance.

- Production cache storage with recovery, eviction, exclusive-root ownership, bounded asynchronous
  request-path I/O, and cache-bound prepared-entry admission is active for reverse HTTP and forward
  HTTP/1.
- Cache freshness, revalidation, conditional validators, collapsed forwarding, bounded surrogate-tag
  purge, streaming/range bypass, and request-level observability are active; H3 cache remains
  explicitly unsupported.
- Reverse QUIC/H3 frontend selected through a proof of compatibility with Pingora's service model
  and reusing immutable upstream service/pool plans.
- HTTP/3 broader conformance and additional UDP resource-exhaustion tests remain; bounded response
  conformance, generation-owned graceful GOAWAY drain, cancellation, malformed-input, and reload
  coverage are implemented and tested, but passing active-traffic drain evidence remains a gate.
  Migration is disabled and 0-RTT is disabled by policy.

HTTP/3 must be advertised only when the active listener is actually QUIC-capable. It must
never degrade silently to another HTTP version.

## Supervision workstream

Status: partial. The master, worker, launcher, authenticated typed descriptor protocol, status
reporting, drain, rollback, and crash handling are implemented for TCP, Unix, UDP, and QUIC/H3
listener adoption. Process tests serve supervised UDP and H3 traffic, but the default public `serve`
path remains direct and packaged production replacement evidence for active UDP/H3 traffic is not
complete.

Remaining supervision gates are packaged Linux default deployment and active-traffic UDP/H3
replacement, rollback, drain, restart, descriptor-ownership, and crash-recovery evidence. Generic
TCP/Unix replacement, listener-start failure, and worker crash handling have focused tests;
supervision must not be treated as broad inherited-file-descriptor compatibility.

## Future kernel integration

Status: out-of-scope for the ordinary daemon; research only for a separately approved privileged
helper.

If transparent traffic handling becomes necessary, build a separate privileged Linux
helper with a narrow API and explicit nftables/policy-routing ownership. Keep it optional
and out of the proxy process. Do not generate or reconcile arbitrary firewall rules from
the main daemon.

## RTMP workstream

Status: partial. The current contract is bounded live publish/play, fanout, static push/pull relay,
same-daemon auto-push, callbacks, local/HTTP VOD, legacy AVC/AAC FLV recording, RTMP
statistics/session controls, HLS, DASH, and allowlisted isolated exec profiles. Canonical named
recorders, exact-ID manual controls, and service-configured inbound assembled-message protection
are implemented; nginx-RTMP directive breadth and parity remain incomplete.

RTMP proceeds in independently releasable slices rather than waiting for HTTP/Squid parity:

1. Register and validate all 117 active nginx-rtmp directives with lossless raw values and
   runtime-support status (registry/context validation implemented; deterministic includes,
   inheritance, occurrence accounting, provenance, and canonical finalization implemented for a
   strict listener/application/recording subset, including supported named recorder blocks;
   broad lowering remains).
2. Implement handshake, chunk transport, AMF0 connect/createStream, live publish/play,
   metadata/codec headers, keyframe gating, and bounded fanout (narrow live listener path and
    bounded service-configured assembled-message and acknowledgement-window ceilings are implemented;
    broader conformance and exhaustive chunk coverage remain).
 3. Add access, callbacks, push/pull relay, FLV recording, VOD, statistics, control, and logging
    (canonical continuous/manual legacy AVC/AAC FLV recording, canonical named recorders, session
    dispatch, storage, bounded workers/reaping, observability, exact-ID bearer-protected controls,
    bounded access/callback policy, local/HTTP VOD, global/live/client statistics, and
    revision-checked session drop controls are integrated; enhanced codec recording, broader
    callback/native parity, and RTMP access-log parity remain).
 4. Add HLS, MPEG-DASH, isolated exec workers, limits, and multi-worker equivalents (bounded HLS,
    DASH, exec, and same-daemon auto-push slices are implemented; broader native parity, crash and
    resource-exhaustion evidence, and multi-worker deployment evidence remain).

Each remaining slice will begin with protocol/configuration failures and differential fixtures
against the cloned nginx-rtmp module. Directive parsing does not count as runtime feature parity.
