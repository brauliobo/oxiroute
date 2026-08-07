# Compatibility matrix

This matrix describes the OxiRoute 0.4.1 pre-alpha release line. It records narrow tested
capabilities, not complete parity with the native products or protocols named below.

## Status meanings

- `stable`: part of the current narrow release contract with implementation and the required
  evidence for that behavior. It does not mean complete compatibility with the native product.
- `partial`: a narrow path exists but the documented compatibility, failure, observability, or
  production gate is incomplete.
- `foundation`: code and tests exist, but the path is not an active daemon capability.
- `planned M1` through `planned M4`: committed future work assigned to a roadmap milestone.
- `research`: no committed implementation milestone; a product or design decision is required.
- `not-planned`: deliberately excluded from the current product plan.
- `out-of-scope`: conflicts with the user-space proxy boundary or belongs to another product.

This file is updated in the same commit that changes a capability. Build support alone is
not protocol support. Coverage manifests may use the narrower gate word `integrated` for a
component whose canonical, runtime, failure, and test gates are all present; the public matrix
uses `stable` or `partial` so component integration is not confused with product-wide parity.

## Proxy and transport

| Capability | Status | Notes |
| --- | --- | --- |
| HTTP/1 reverse proxy | partial | Deterministic nginx-style routing, health-aware tagged pools, balancing, active checks, bounded safe connect retries, independent listener/route/pool timeouts, route-local body limits, bounded header/cookie policy, bounded fixed-length response buffering, deadline-clamped I/O, H1 early-response draining, semaphore-bounded bcrypt/APR1 Basic access, descriptor-pinned conditional/range static serving with optional ETags, gzip-only weighted content negotiation, bounded asynchronous redacted JSONL access logs, WebSocket upgrades, downstream TLS, and verified upstream TLS are implemented; broader conformance remains. |
| HTTP/2 downstream | partial | TLS profiles provide explicit `h2` ALPN with bounded 64 KiB decoded header lists and 100 concurrent streams. Independent clients wire-test unary and streaming DATA/trailers, request-body streaming, flow-control backpressure, request/body and listener connection bounds, deadlines, cancellation/reset, malformed upstream flow, and required client authentication. H2-only listeners reject incompatible ALPN and close no-ALPN streams before HTTP parsing; h2c and stream takeover remain unsupported and fail closed. HTTP/1 and WebSocket behavior remains on Pingora's existing paths. |
| HTTP/2 upstream | partial | TLS pools support `1.1/1.1`, `1.1/2`, and H2-only `2/2` policies. H2-only success, streaming DATA/trailers, reset/deadline cancellation, bounded request forwarding, malformed content-length flow, and no-compatible-ALPN failure before HTTP headers are wire-tested with verified custom CA and SNI. `1.1/2` may use HTTP/1.1 only when explicitly configured; `2/2` never downgrades. |
| HTTP/3 downstream/upstream | partial | The active `http3` reverse and `forward_http3` UDP listeners use separate bounded Quinn/H3 runtimes with TLS 1.3, `h3` ALPN, explicit SNI and trust roots, disabled/optional/required downstream client authentication with immutable bounded CA bundles and exact DNS/IP SAN checks, reverse routing/static/fixed/proxy responses where configured, forward authority-only classic CONNECT, response trailers, generation-owned graceful GOAWAY drain, disabled migration/0-RTT, deadline/cancellation handling, and no H1/H2 fallback under an H3 policy. Client-auth failures close QUIC before H3 request bytes are processed; generation-owned drain is implemented and process-tested, including reload/shutdown and no post-GOAWAY request; active-traffic drain evidence and broader conformance remain gates. Cache, compression, upgrades, and broader forward request forms remain explicitly unsupported. |
| WebSocket reverse proxy | stable | The standard nginx `Upgrade $http_upgrade` and `Connection upgrade` policy is accepted without taking upgrade ownership from Pingora; HTTP/1.1 passes independent bidirectional framed interoperability coverage. |
| gRPC reverse proxy | partial | The TLS/H2 path wire-tests unary and multi-DATA streaming requests/responses, successful and trailers-only status metadata, bounded request bodies, flow-control backpressure, deadlines, cancellation/reset, malformed framing, required client authentication, and no downgrade under an H2-only upstream policy; broader gRPC conformance remains. |
| HTTP/1 explicit forward proxy | partial | A daemon-integrated HTTP/1 listener handles absolute-form HTTP and bounded CONNECT tunnels with optional socket-bound downstream TLS, negotiated `http/1.1` enforcement, Basic/Bearer authentication, ordered access rules, canonical domain/CIDR and bounded UTC time policy, final-answer DNS/SSRF pinning across address retries, header privacy, connection/body/header/time limits, opt-in bounded GET/HEAD memory or persistent caching with collapsed fills/revalidation, authenticated purge, structured metadata access events, per-listener cache outcomes, shutdown cancellation, and real wire coverage. Broader HTTP conformance remains. |
| HTTP/1 CONNECT-UDP | partial | An opt-in RFC 9298 HTTP/1.1 Upgrade on `forward_http1` relays bounded Capsule Protocol DATAGRAM payloads to approved UDP destinations. `connect_udp.enabled` and its `allowed_ports` are enforced, with malformed framing, policy, shutdown, and real wire coverage; H2/H3 CONNECT-UDP and broader MASQUE compatibility remain unsupported. |
| HTTP/2 forward proxy/CONNECT | partial | The daemon integrates authority-only classic CONNECT over TLS/H2 with the shared policy, exact approved-address connection, bounded DATA relay, half-close, flow-control, timeout, reset, and cancellation behavior. Non-CONNECT requests and arbitrary H2 forwarding forms are rejected or unsupported. |
| HTTP/3 forward proxy | partial | `forward_http3` is daemon-integrated through a separate UDP listener with TLS-level disabled/optional/required downstream client authentication, exact client DNS/IP SAN enforcement, authority-only classic CONNECT tunnel support, shared authorization/destination policy, bounded QUIC resources, and fail-closed no-fallback coverage. H3 absolute-form forwarding has no positive daemon wire evidence and is not advertised; broader conformance remains. |
| HTTP/3 reverse proxy | partial | `http3` is daemon-integrated through a separate UDP listener with TLS-level disabled/optional/required downstream client authentication, exact client DNS/IP SAN enforcement, validated HTTP routing, fixed/redirect/static/proxy actions, explicit `3/3` QUIC/H3 upstream pools with SNI/custom CA support, bounded request/response resources, safe response framing and trailers, safe retries, implemented/tested graceful GOAWAY generation drain, disabled migration/0-RTT, and no H1/H2 fallback; cache, compression, upgrades, and broader conformance remain unsupported. |
| Opaque TCP relay | partial | Bounded bidirectional relay, independent half-close, configured connect/idle/lifetime timeouts, socket/DNS/Unix upstreams, health-aware round-robin or relay-scoped least-connections pools, active TCP/HTTP checks for non-Unix pools, nullable listener connection caps, shutdown cancellation, partial traffic accounting, and loopback tests are implemented; Unix transports require Unix, and reload and graceful process drain remain. |
| TLS pass-through | partial | Opaque bytes can traverse the implemented TCP relay without termination; no SNI inspection, TLS-specific policy, or dedicated pass-through conformance suite exists. |
| UDP relay | partial | UDP listeners use generation-owned bounded pseudo-sessions keyed by client address, per-client reply routing, family-safe DNS selection, idle/lifetime expiry, queue/session/table limits, cancellation, listener/process accounting, bounded PROXY v2 first-datagram acceptance/propagation, and shared passive endpoint-health attribution for genuine upstream connect, datagram I/O, and protocol failures; targeted loopback wire, health, and exhaustion coverage is present, while active UDP health checks and packaged active-traffic replacement remain unsupported. |
| PROXY protocol | partial | Explicit bounded v1/v2 TCP stream and v2 UDP datagram acceptance/propagation with malformed, unsupported, timeout, and transport-mismatch rejection; transparent interception, source spoofing, and broader wire conformance remain unsupported. |
| ICMP/arbitrary IP protocols | out-of-scope | Requires packet-level/kernel integration, not sockets. |
| Transparent interception/source spoofing | research | Only a separately approved privileged helper could own this; it is never ordinary proxy behavior. |
| DNAT/SNAT/MASQUERADE/firewall | out-of-scope | Remains kernel nftables/iptables functionality. |

OxiRoute carries a narrow pinned `pingora-core` 0.8.1 patch that exposes a per-peer OpenSSL
configuration hook and pre-handshake application admission. Upstream TLS applies a TLS 1.2 minimum,
security level 2, strict partial-chain verification, and modern AEAD cipher policy before handshake;
in-process TLS 1.0/1.1 controls verify refusal without origin HTTP bytes. The patch and upgrade
procedure are documented in `vendor/pingora-core/README.oxiroute.md`.

## Load balancing and operations

| Capability | Status |
| --- | --- |
| Single-endpoint pools | stable |
| Static round robin | stable |
| Static least connections | stable with deterministic tie rotation and request/relay-scoped active leases |
| Bounded upstream admission | stable with pool-level FIFO capacity admission for L4 and nonreusable HTTP, including timeout and cancellation handoff; reusable HTTP retains connection/stream reuse-first admission |
| Active TCP/HTTP health checks | stable with configured startup eligibility, normal/fast/down completion-based schedules, consecutive transition thresholds, HTTP version/optional Host/exact status policy, and a shared 32-probe limit |
| Weighted round robin | stable for canonical weighted selection with one bounded weight per server, deterministic health/capacity-aware runtime selection, topology/metrics exposure, frontend weight editing, and configuration/runtime/failure tests; HAProxy static `weight 1..100`/`default-server weight` and nginx HTTP upstream `weight=1..100` lower into weighted pools, while dynamic, duplicate, out-of-range, runtime, and incompatible native forms remain blocked |
| Bounded connect-failure retries | stable for pre-send connection failures on non-upgrade requests, safe refused-stream replays, distinct canonical endpoint identities, and at most three additional attempts |
| Response retries and passive ejection | stable for bounded configurable refused-stream, response-status, empty-response, response-timeout, and malformed-response retries plus per-pool passive observation, threshold, mark-down/up, backoff, recovery, metrics, and events; canonical/configuration and frontend controls plus the HAProxy bounded retry-on subset are covered |
| Config file watcher and generation reload | partial: parent-directory notification, debouncing, periodic hash reconciliation, complete candidate preparation, reusable listener reservations, atomic in-process activation, bounded drain, rollback, native-reference re-resolution, and resolved dependency-path watching are implemented; broader active-traffic drain evidence remains |
| Supervised master/worker listener adoption | partial: authenticated typed TCP, Unix, UDP, and QUIC/H3 descriptor adoption, worker status, initial UDP relay/H3 serving, drain, rollback, and crash handling are implemented and tested; the default public `serve` path remains direct, and production supervised replacement/drain evidence for active UDP/H3 traffic remains |
| Runtime monitoring snapshot | partial: Linux x86_64/aarch64 process/host load, explicit non-Linux unsupported sampler state, listener traffic, pool/endpoint health, RTMP activity, certificate expiry, component degradation, and active-generation age are implemented; cumulative `u64` wire fields are exact decimal strings |
| Prometheus exposition | stable for process/host sampler state and values, active-generation age, process, listener, pool, server, queue, health, retry, certificate, RTMP, and generation families with bounded fixed outcome labels |
| Structured access logs | partial: HTTP and RTMP file logs emit bounded fixed redacted JSONL events without URI/query/auth/cookie values, raw payloads, or client addresses; RTMP uses a separate lifecycle schema and nonblocking drop counters, while retention/rotation breadth remains |
| Vue 3 and build-time Pug UI | partial: responsive monitoring/topology observatory, RTMP broadcast desk, operations, certificate, bounded event-history, native import report/provenance, durable audit, and bearer-unlocked canonical configuration workspaces are implemented with validation, Lua/candidate review, conflict handling, SSE refresh/reconnect, passive-health/retry controls, weighted-round-robin weight editing, and pending-activation save reporting; TLS-ALPN challenge selection and guidance are exposed, but selection does not create or deploy the required listener; native source editing and broader API/UI workflows remain outside the frontend, and listener deployment plus live CA-staging evidence remain gates |
| Management API | partial: loopback monitoring/topology, bounded redacted native import reports, RTMP snapshot/detail/statistics/session controls, configured exact-ID recorder controls, authenticated config GET/validate/revision-checked durable PUT, listener/pool/server operations, generation actions, full bounded ACME lifecycle actions, process actions, durable redacted audit history/status, cursor events/SSE, readiness/status/metrics, and authenticated stats administration are implemented; native source editing and broader API/UI workflows remain |
| Revisioned config API | stable for authenticated canonical reads, complete preflight, deterministic preview, conflict-safe durable writes, and explicit `saved_pending_activation`/`unchanged_active` outcomes |
| Config watcher, live generation activation, and event delivery | partial: watcher-driven in-process generation activation, bounded cursor polling, and bearer-authenticated SSE with cursor replay, heartbeats, resync, and shutdown frames are implemented; the operational event ring is non-durable, while separate durable audit history/status are implemented, and broader active-traffic drain evidence remains |

## RTMP

| Capability | Status |
| --- | --- |
| All 117 nginx-rtmp directive keys/value grammars | partial: tokenizer, registry, and contextual value validation cover all keys; the registry report distinguishes enforced, disable-only, parsed-only, source-no-op, source-bug, deprecated, and platform-limited forms; deterministic includes, inheritance, occurrence accounting, provenance, and finalization exist only for a strict listener/application/recording subset |
| RTMP simple/complex handshake | stable on the live listener through the pinned `rml_rtmp` state machine; the standalone simple-response primitive remains covered independently |
| Chunk formats 0-3 and extended timestamps | partial: `rml_rtmp` transport is active with a 1 MiB inbound chunk limit, a bounded service-configured assembled-message limit with `max_message` lowering from RTMP/server scope, configured acknowledgement window, and configured outbound chunk size announced on wire; bounded rejection/configuration tests pass, while exhaustive fragmentation/interleaving tests remain |
| AMF0 connect/createStream/publish/play | partial: configured live publish/play, bounded VOD play, stop, delete, and bounded multi-message-stream publisher/subscriber roles are active; broader native command parity remains |
| Active stream snapshot catalog | stable for live publisher/subscriber identity per message-stream role, codec/timestamp/byte observations, duplicate rejection, and disconnect cleanup |
| Active stream management API | stable for snapshot/detail JSON backed by live publisher sessions, with bearer protection on recognized API routes |
| Active stream and recording UI | partial: live recorder snapshots, exact-ID manual recorder controls, RTMP client statistics/drop controls, certificate lifecycle status/actions, bounded event history, and native import report/provenance views are implemented; native source editing and broader media controls remain |
| Live publisher/subscriber fanout | stable with application-scoped canonical subscriber/message/byte bounds, bounded service/per-stream/per-viewer resources, cached metadata/AAC/AVC headers, keyframe gating, slow-viewer resynchronization, and restart reset |
| ACL allow/deny | partial: bounded application publish/play network and stream-token policies are active; broader native directive parity remains |
| Push/pull relay and reconnect | partial: typed push and pull destinations, pinned startup resolution, bounded periodic DNS refresh, direct-loop rejection, full `rml_rtmp` client lifecycle, metadata/header/keyframe bootstrap, independent message/byte bounds, publisher-incarnation cancellation, bounded retry/recovery, shutdown, credentials, and redacted state/counters are implemented; broader native lowering remains absent |
| FLV recording, VOD, and recorder controls | partial: canonical continuous/manual policies, bounded masks and limits, named local/HTTP VOD sources, RTMP playback workers, authenticated single-range VOD responses, cached manual-start bootstrap, publisher media dispatch, exact-ID bearer-protected controls, catalog completion, nonblocking bounded media queues, redacted observability, safe relative naming, descriptor-pinned storage, atomic publication, process-scoped quotas, bounded pending-task reaping, explicit IANA timezone rendering with historical DST, wall-clock segment-open naming, monotonic keyframe-aligned rotation, and FLV payload under arbitrary suffixes are integrated for legacy AVC/AAC; enhanced AVC/HEVC/AV1 recording, cross-process quota coordination, and broader RTMP control remain absent |
| FLV/MP4 local/HTTP VOD | partial: bounded named local/HTTP objects, FLV RTMP playback workers, safe roots/origins, and authenticated single-range management responses |
| HTTP notify callbacks | partial: bounded HTTP/HTTPS authorization, teardown, and update callbacks with GET/POST methods, resolved-address policy checks, strict update handling, and redacted outcomes; richer nginx callback fields and redirect forwarding remain absent |
| RTMP statistics/control API equivalents | partial: authenticated global/live/client statistics include bounded per-message-stream role snapshots, and revision-checked publisher/subscriber/session drop controls are integrated with bounded result shapes; native XML/form `rtmp_stat` and `rtmp_control` parity, redirects, and broader control coverage remain absent |
| HLS H.264/AAC transmuxing and AES keys | partial: bounded MPEG-TS HLS output, variants, atomic storage, cleanup, and AES-128 key rotation are integrated; transcoding and broader origin/auth policy remain absent |
| MPEG-DASH fragmented MP4 output | partial: bounded authenticated AVC/AAC fragmented MP4 segments and MPD `SegmentList` output are active; unsupported codec forms and native nginx-DASH lowering remain explicitly blocked |
| Isolated exec/transcode process integration | partial: typed allowlisted profiles, direct argv/env construction, bounded queues/output/timeouts, publisher correlation, respawn, shutdown, and native publisher/publish-done lowering; broader directive parity and privileged namespace setup remain absent |
| Multi-worker auto-push equivalent | partial: same-daemon Unix-worker auto-push uses authenticated framed media, publisher-incarnation fencing, bounded peer discovery, queue/bootstrap recovery, and no relay/record/exec side effects on peer copies; non-Unix workers, arbitrary RTMP peers, and broader nginx topology parity remain absent |

The live listener pins `rml_rtmp` 0.8.0 behind OxiRoute's session adapter. The adapter enforces
the inbound chunk-size ceiling, the service-configured assembled-message allocation ceiling, the
configured acknowledgement window, and bounded fanout/drain/write policies. Canonical services
default to an 8 MiB message ceiling; imported nginx services default to 1 MiB and lower bounded
`max_message` values from RTMP or server scope.

The directive registry retains a backward-compatible key-level classification and adds explicit
form-level reporting for the narrow lowered subset. Its report distinguishes enforced,
disable-only, parsed-only, source-no-op, source-bug, deprecated, and platform-limited forms.
“All keys parsed” is not advertised as full nginx-rtmp semantic compatibility.

## Certificates

| Capability | Status |
| --- | --- |
| Imported PEM certificate/key | partial: strict bounded direct-file and descriptor-relative Certbot loading creates immutable generations; every declared DNS/IP identity must exist in the typed SAN set, undeclared SANs are not added to named SNI selection, IPv4-mapped IPv6 is canonicalized to IPv4, one TLS profile can select multiple exact/wildcard DNS SNI identities with an explicit default, and direct-file PEM pairs and Certbot lineages are watched by separate reconcilers; valid replacement generations are published atomically, invalid direct-file candidates retain the active generation while direct-file watcher health degrades until recovery, canonical configuration reload remains on the separate config-watcher path, and direct-file API editing is absent. As with nginx, an explicit default certificate is still presented for unmatched SNI. |
| Atomic certificate generation publication | partial: independent identity/SAN-bound compare-and-swap publication, complete-generation handshake snapshots, disabled downstream session resumption, existing-connection retention, deterministic concurrent handshake waves, per-identity SNI rotation, a multithreaded publication/snapshot race, process-lifetime direct-file and Certbot reconciliation, and managed ACME activation are implemented |
| Self-signed development certificate | partial: explicit in-memory development generation without a replacement watcher and in-memory first-start ACME bootstrap are implemented |
| ACME account/order lifecycle | partial: bounded account, order, authorization, CSR, certificate download, and owner-only state paths are implemented; live staging evidence remains |
| ACME HTTP-01 | partial: exact bounded challenge leases and HTTP routing are implemented; explicit listener/deployment evidence remains |
| Automatic renewal and zero-downtime activation | partial: externally renewed Certbot lineages and OxiRoute-managed ACME certificates are reconciled and published without interrupting existing connections; live staging evidence remains |
| Existing Certbot lineage import/watch | stable with strict common-revision snapshots, archive containment, descriptor-relative no-follow reads, key-reuse handling, bounded event coalescing, periodic rescans, directory-watch rebuilding, mixed/invalid retention, and zero-downtime per-identity publication |
| ACME DNS-01 and wildcard names | partial: bounded wildcard identifiers, DNS-01 TXT derivation, statically linked exact-name provider registration, credential/timeout/cancellation bounds, provider propagation hooks, exact cleanup, durable cleanup journaling/recovery, atomic validated activation, and redacted status are implemented; provider deployments and live staging evidence remain |
| ACME TLS-ALPN-01 | partial: RFC 8737 challenge certificates, exact SNI/`acme-tls/1` selection, bounded ownership/expiry/cancellation, cleanup, and no-fallback handshake behavior are implemented and tested; live CA staging/deployment evidence remains |
| External/HSM key provider | research |

## Native configuration

| Source | Status | First subset |
| --- | --- | --- |
| OxiRoute Lua | partial | Aggregate/listener admission, Unix modes, downstream and route-local policy, nginx wildcard selectors, bounded headers/auth/cookies/static/gzip/logging, named server capacity and queueing, balancing, DNS, pool timeout/reuse, extended health policy, and RTMP chunk/fanout/push/recording naming policies compile into the runtime. Response buffering requires a positive route body cap and a bounded fixed-length origin response; cache remains fail closed, while RTMP file access logging uses the bounded fixed JSONL sink. |
| nginx | partial | `import_root` loads one complete include graph, emits one root terminal decision per occurrence, and merges strict HTTP and nginx-RTMP candidates while retaining deployment concerns. KDL/HOCON/UCI native references resolve that finalized candidate into the canonical runtime and the watcher re-resolves it; standalone CLI report/preview remains offline evidence. Bind lowering preserves accepted exact/wildcard claims on the default server plus its fallback. Value-bearing certificate, htpasswd, bearer, upstream-TLS, and host-timezone overlays must be uniquely supplied and consumed before finalization. Proxy defaults preserve `$proxy_host`, connection-close behavior, bare-second and composite times. Bind-wide identical effective gzip `on`/`off`, level, concrete types, minimum length, HTTP version, eligible statuses, `gzip_proxied off`, and vary policy lower natively; bounded static HTTP upstream `weight=1..100` lowers to canonical weighted round robin with implicit default weight `1` and per-weight provenance. Unsupported proxied modes, dynamic/duplicate/out-of-range upstream weights, other upstream parameters, proxy HTTP/1.0 or buffering defaults, active X-Accel controls, non-equivalent TLS session/cipher/DH/H2 policy, formatted logs, and cross-location static reroutes still fail closed. Static `etag on|off` lowers with nginx-format validators. The HTTP fragment API remains available. |
| HAProxy | partial | Ordered roots, deterministic `${NODE_IP}`/`defined(GPU1)` preprocessing, immutable original snapshots, generated-to-original source maps, environment fingerprints, inactive provenance, defaults/reference resolution, decision accounting, diagnostics, provenance, and HTTP/TCP lowering exist. Aggregate/listener/server admission, `roundrobin`/`leastconn`/`first`, reusable HTTP least-connections, DNS/Unix servers, `unix@` modes, cumulative `default-server` health intervals/capacity, bounded static `weight 1..100` with inherited effective weights under `roundrobin`, exact health timeouts, one literal `http-check send` GET request with canonical URI/version and optional Host, case-insensitive exact Host routes with fixed `503` fallback, one positive exact Host-plus-`path_beg` ACL conjunction, bounded same-server retries with bare final redispatch, timeout scopes including queue deadlines, source-CIDR `forwardfor except`, source server-close policy, and dedicated supported stats pages lower. Reports identify the `haproxy-strict` capability profile and retain ordinary source tables plus preprocessing provenance; native version remains unset without explicit version evidence. Stats authentication/other forms and exact Prometheus services remain non-equivalent activation requirements; a uniquely consumed migration overlay can opt a dedicated `/metrics` section into OxiRoute's different metric families and broader observability routes. Logging/process settings remain explicit deployment warnings. Redispatch interval forms, dynamic/duplicate/out-of-range/incompatible server weights, dynamic/negated/duplicate ACL expressions, maps, arbitrary health-check headers/bodies, and broader native policy remain blocking. |
| Apache httpd | partial | Offline `import apache` reports and previews plus KDL/HOCON/UCI `apache_server` references cover byte-sorted includes with silent missing optionals, inherited server defaults, explicit/wildcard IP listener matching, multi-address exact virtual hosts, case-insensitive exact authorities, static HTTP/HTTPS ProxyPass destinations, first-match rules only when canonical longest-prefix routing is equivalent, equal-weight balancers, and TLS path identities; rewrites, regex proxying, directory/location merges, module scripts, authentication/authorization, response rewriting, dynamic balancer state, unsupported modules, and daemon-native Apache activation remain out of the subset. |
| Squid | partial | The bounded importer loads deterministic includes, rechecks source/path/glob identity, classifies typed semantic facts, and lowers the audited direct HTTP/1 subset plus ordered static parent peers and global direct-fallback rules into a runtime-preparable canonical candidate. `oxiroute import squid` and `squid_server` native references are integrated. Cache/refresh behavior is externalized because the runtime is direct and non-caching; native references containing refresh rules require `externalize_cache = true` so activation cannot discard them silently. |
| Varnish VCL | partial | Bounded ordered includes, typed semantic evidence, explicit invocation facts, exact static HTTP/cache lowering, offline report/preview, and KDL/HOCON/UCI native references are integrated. VMODs, dynamic VCL, custom subroutines, invalidation, synthetic responses, and mismatched cache/invocation semantics remain fail-closed. |

## Squid feature families

Squid directive coverage is a versioned registry for checkout `6f4c814` in
[`coverage/squid-directives.json`](../coverage/squid-directives.json). The importer publishes the
same registry in the machine-readable `capabilities` object of `oxiroute import squid --output
report`. `compatible` is reserved for an exact form with both runtime integration and failure
coverage. `partial` means an audited subset exists; `unsupported`, `obsolete`, and `not_planned`
forms are never treated as parity.

| Family | Status | Boundary |
| --- | --- | --- |
| Explicit HTTP forward requests | partial | Only the audited direct HTTP/1 forms are lowered. |
| CONNECT tunnels | compatible | The bounded explicit CONNECT form has wire and failure coverage. |
| Includes and source ordering | partial | Bounded deterministic file and glob includes only. |
| ACL definitions and access ordering | partial | Source, exact port, proxy-auth, built-ins, and ordered HTTP access only. |
| Proxy authentication | partial | Basic htpasswd form only; other schemes and helper settings block. |
| Resolver selection | partial | Explicit finite nameserver lists only. |
| Header privacy and access | partial | `forwarded_for delete` and `via off` only. |
| Logging and audit output | partial | Disabled native access logging only. |
| Memory and persistent HTTP caching | unsupported | The Squid importer is direct and non-caching; OxiRoute's separate reverse/forward cache is not Squid cache admission or storage parity. |
| Freshness, revalidation, ranges, and collapsed forwarding | unsupported | OxiRoute has bounded cache timelines outside the Squid importer, but Squid `refresh_pattern` and related cache semantics are not runtime-owned and remain externalized or blocked. |
| Parent and sibling cache peers | partial | Static ordered HTTP parent peers and global `always_direct`/`never_direct` fallback rules are supported for HTTP/1; sibling, dynamic, credentialed, hierarchy, ICP, and peer-option forms remain blocked. |
| External ACL and URL rewrite helpers | unsupported | Helper lifecycle and protocols are not implemented. |
| ICAP and eCAP adaptation | unsupported | Adaptation negotiation and transformation are absent. |
| Transparent interception and TPROXY | unsupported | Kernel/router cooperation is not a socket-proxy capability. |
| TLS bump and certificate mimicry | unsupported | HTTPS-origin verification is not TLS interception. |
| Delay pools and traffic shaping | unsupported | Squid hierarchical bandwidth accounting is absent. |
| FTP proxy and listing behavior | not_planned | Outside the maintained direct HTTP/1 line. |
| ICP, HTCP, WCCP, SNMP, and Cache Manager | not_planned | Legacy datagram, router, and native management protocols are not targets. |
| Process, worker, and deployment controls | not_planned | Squid worker/deployment settings are externalized; OxiRoute supervision is a separate lifecycle contract, not a Squid process-control protocol. |
| ESI and content processing | not_planned | Cache-coupled content processing is outside this scope. |
| Obsolete target-version directives | obsolete | Removed or replaced names are blocked. |

The registry deliberately reports `partial` parity and `completeParity: false` while any open
family or directive remains. Cache, refresh, unsupported peer, helper, adaptation, interception,
TLS-bump, legacy, process, and UDP-related behavior is not advertised by this matrix. OxiRoute's
separate cache and supervision contracts do not promote those Squid families.
