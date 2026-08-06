# OxiRoute 0.4.0

OxiRoute 0.4.0 is a pre-alpha release that expands the tested narrow runtime and control-plane
surface while keeping compatibility claims explicit. It does not represent full parity with HTTP/3,
Squid, RTMP, or any native proxy product.

## Highlights

- Add active, separate reverse `http3` and forward `forward_http3` listener paths through bounded
  Quinn/H3 services with TLS 1.3, `h3` ALPN, SNI/trust-root policy, request/response limits,
  deadlines, cancellation, response trailers, generation drain, and disabled migration/0-RTT.
- Add explicit bounded PROXY protocol v1/v2 handling for TCP and PROXY v2 handling for UDP, including
  fail-closed malformed and transport-mismatch behavior and propagated client addresses.
- Integrate bounded memory and persistent caching for eligible reverse and HTTP/1 forward GET/HEAD
  requests with freshness/revalidation, collapsed fills, authenticated purge, and cache outcomes.
- Extend RTMP with bounded HLS H.264/AAC output, AES-128 key rotation, authenticated fragmented-MP4
  DASH output, VOD/recording controls, live statistics, and allowlisted isolated exec profiles.
- Extend the management API and Vue dashboard for current RTMP, certificate, configuration, event,
  and operational controls, with deterministic component and browser coverage.
- Add weighted-round-robin server weight editing with bounded validation and add an authenticated
  durable audit workspace with filters, cursor pagination, and persistence status; the operational
  event ring remains non-durable.
- Add authenticated bounded native import report inventory/detail and a redacted Provenance workspace
  with finalized read-only KDL previews; native source files remain read-only and are never rewritten.
- Add bounded redacted RTMP JSONL access logging with lifecycle metrics and periodic relay DNS refresh
  with policy/family/direct-loop checks and refresh outcomes.
- Add targeted UDP relay passive failure attribution and bounded exhaustion coverage, plus bounded H3
  resource-exhaustion coverage, while retaining the existing production-evidence gates.
- Add bounded authenticated event SSE with cursor replay/resync, heartbeats, shutdown signaling, and
  frontend event-history/reconnect handling; the operational event ring remains in memory and
  non-durable, while redacted durable audit history is stored and queried separately.
- Expand audited native configuration coverage across nginx/nginx-RTMP, HAProxy, Squid, Varnish, and
  Apache subsets while retaining blocking diagnostics for unsupported semantics.
- Add the bounded supervision master/worker and Arch launcher path for authenticated typed TCP, Unix,
  UDP, and QUIC/H3 listener adoption, worker status, drain, rollback, crash recovery, and managed
  configuration support; initial UDP/H3 serving and generic replacement/error paths are tested, but
  the default public entry point remains direct `oxiroute serve` until packaged active-traffic
  replacement evidence is complete.

## Compatibility Boundaries

- **HTTP/3: partial.** The active reverse and forward listeners cover bounded routing/forwarding,
  configured fixed/redirect/static actions, classic CONNECT forms, bounded resource exhaustion, and
  implemented/tested generation-owned GOAWAY drain. Cache, compression, upgrades, broad HTTP
  conformance, active-traffic drain evidence, and arbitrary forward HTTP/2/HTTP/3 forms remain
  unsupported or gated.
- **HTTP caching: partial.** Only the implemented reverse and eligible HTTP/1 forward GET/HEAD paths
  are cacheable. Broader HTTP cache conformance and Squid refresh/cache semantics are not provided.
- **Squid: partial.** The importer lowers an audited direct HTTP/1 and CONNECT subset plus ordered
  static parent peers and global direct-fallback rules. Sibling/dynamic/credentialed peer forms,
  peer hierarchy, helper protocols, ICAP/eCAP, transparent interception, TLS bump, legacy datagram
  protocols, and native cache-manager behavior remain unsupported.
- **RTMP: partial.** Live publish/play, recording/VOD, HLS, DASH, relays, statistics/session
  controls, bounded redacted access logs, periodic relay DNS refresh, allowlisted isolated exec
  profiles, and same-daemon Unix-worker auto-push are bounded slices. Complete nginx-RTMP directive
  parity, transcoding, unsupported codecs, broader callback/control parity, non-Unix worker topology,
  and wider multi-worker evidence remain absent.
- **Supervision: partial.** The master, worker, launcher, authentication, typed TCP/Unix/UDP/QUIC
  descriptor adoption, status, drain, generic replacement/rollback, and crash-recovery paths are
  tested; initial supervised UDP/H3 serving is covered, while active UDP/H3 replacement and broader
  production migration evidence remain required. The public default remains direct runtime operation.
- **Managed ACME: partial.** HTTP-01, bounded DNS-01/wildcard, and TLS-ALPN-01 lifecycle paths are
  implemented, including static exact-name provider registration, in-memory challenge certificates,
  redacted state, renewal, Renewal Information scheduling, durable DNS cleanup recovery, revocation,
  rollover, and job controls. Provider deployment, live staging evidence, and the frontend's TLS-ALPN
  challenge selector remain gaps.
- **Control plane/UI: partial.** The backend exposes certificate lifecycle, RTMP statistics and
  controls, bounded polling/SSE events, durable redacted audit history/status, native import reports,
  operations, and provenance; the Vue frontend exposes durable audit browsing, weighted-round-robin
  weight editing, native import report browsing, and the current configuration/control contracts.
  Native-file editing and TLS-ALPN challenge selection remain backend/canonical-file capabilities
  only, not frontend controls.
- **Native import: partial.** Importers preserve provenance and fail closed for unsupported or lossy
  forms. No complete nginx, HAProxy, Squid, Varnish, Apache, or nginx-RTMP compatibility is claimed.

## Release Gates Still Open

- Active traffic: long-lived HTTP, H2, H3, TCP, UDP, RTMP, and SSE reload/drain tests must prove
  no-new-work admission, cancellation/deadlines, GOAWAY behavior, and old-generation retention.
- ACME staging: CA-staging issuance and renewal for HTTP-01, DNS-01, and TLS-ALPN-01 must cover
  listener deployment, failed challenges, cleanup, rollback, and real certificate activation.
- Interoperability: independent H3/TLS clients and origins, FFmpeg/OBS RTMP publish/play, and
  representative Apache/HAProxy/Squid/Varnish migration evidence must extend beyond synthetic tests.
- Fuzz and crash: every checked-in parser harness needs bounded execution and crash-corpus triage;
  media, exec, reload, and supervision fault-injection matrices remain open.
- Production supervision: packaged Linux UDP/H3 replacement, rollback, descriptor ownership, drain,
  restart, and crash recovery under active traffic must be demonstrated before supervised operation
  is treated as the default deployment path.

See [COMPATIBILITY.md](COMPATIBILITY.md), [ROADMAP.md](ROADMAP.md), and the protocol specifications
for exact supported forms, limits, and remaining work.
