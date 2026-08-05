# Compatibility matrix

This matrix describes the OxiRoute 0.4.0 pre-alpha release line. It records narrow tested
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
| HTTP/2 downstream | partial | TLS profiles provide explicit `h2` ALPN and a rustls client wire test negotiates H2 and proxies a real stream. H2-only listeners reject incompatible ALPN and close no-ALPN streams before HTTP parsing. There is no h2c or broad H2 conformance suite. Downstream client authentication is separately supported by the TLS profile policy. |
| HTTP/2 upstream | partial | TLS pools support `1.1/1.1`, `1.1/2`, and H2-only `2/2` policies. H2-only success and no-compatible-ALPN failure before HTTP headers are wire-tested with verified custom CA and SNI; the HTTP/1.1-only policy is also wire-tested. |
| HTTP/3 downstream/upstream | partial | The active `http3` reverse and `forward_http3` UDP listeners use separate bounded Quinn/H3 runtimes with TLS 1.3, `h3` ALPN, explicit SNI and trust roots, static/fixed/proxy responses where configured, response trailers, graceful GOAWAY drain, disabled migration/0-RTT, deadline/cancellation handling, and no H1/H2 fallback under an H3 policy; cache, compression, upgrades, and broader conformance remain explicitly unsupported. |
| WebSocket reverse proxy | stable | The standard nginx `Upgrade $http_upgrade` and `Connection upgrade` policy is accepted without taking upgrade ownership from Pingora; HTTP/1.1 passes independent bidirectional framed interoperability coverage. |
| gRPC reverse proxy | partial | The TLS/H2 path wire-tests gRPC response DATA, successful trailers, and trailers-only error metadata end to end; streaming breadth, deadlines, cancellation, and broader conformance remain. |
| HTTP/1 explicit forward proxy | partial | A daemon-integrated HTTP/1 listener handles absolute-form HTTP and bounded CONNECT tunnels with Basic/Bearer authentication, ordered access rules, canonical domain/CIDR and bounded UTC time policy, final-answer DNS/SSRF pinning across address retries, header privacy, connection/body/header/time limits, opt-in bounded GET/HEAD memory or persistent caching with collapsed fills/revalidation, authenticated purge, structured metadata access events, per-listener cache outcomes, shutdown cancellation, and real wire coverage. Broader HTTP conformance remains. |
| HTTP/2 forward proxy/CONNECT | partial | The daemon integrates classic CONNECT over TLS/H2 with the shared policy, exact approved-address connection, bounded DATA relay, half-close, flow-control, timeout, reset, and cancellation behavior; arbitrary H2 forward request forms remain unsupported. |
| HTTP/3 forward proxy | partial | `forward_http3` is daemon-integrated through a separate UDP listener with absolute-form forwarding, HTTPS-only QUIC/H3 origin selection, classic CONNECT tunnel support, shared authorization/destination policy, bounded QUIC resources, and fail-closed no-fallback coverage; broader conformance remains. |
| HTTP/3 reverse proxy | partial | `http3` is daemon-integrated through a separate UDP listener with validated HTTP routing, fixed/redirect/static/proxy actions, explicit `3/3` QUIC/H3 upstream pools with SNI/custom CA support, bounded request/response resources, safe response framing and trailers, safe retries, graceful GOAWAY generation drain, disabled migration/0-RTT, and no H1/H2 fallback; cache, compression, upgrades, and broader conformance remain unsupported. |
| Opaque TCP relay | partial | Bounded bidirectional relay, independent half-close, configured connect/idle/lifetime timeouts, socket/DNS/Unix upstreams, health-aware round-robin or relay-scoped least-connections pools, active TCP/HTTP checks for non-Unix pools, nullable listener connection caps, shutdown cancellation, partial traffic accounting, and loopback tests are implemented; Unix transports require Unix, and reload and graceful process drain remain. |
| TLS pass-through | partial | Opaque bytes can traverse the implemented TCP relay without termination; no SNI inspection, TLS-specific policy, or dedicated pass-through conformance suite exists. |
| UDP relay | partial | UDP listeners use generation-owned bounded pseudo-sessions keyed by client address, per-client reply routing, family-safe DNS selection, idle/lifetime expiry, queue/session/table limits, cancellation, listener/process accounting, and bounded PROXY v2 first-datagram acceptance/propagation; passive UDP health semantics and full wire/exhaustion coverage remain. |
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
| Weighted round robin | planned M2 |
| Bounded connect-failure retries | stable for pre-send connection failures on non-upgrade requests, safe refused-stream replays, distinct canonical endpoint identities, and at most three additional attempts |
| Response retries and passive ejection | partial: bodyless refused-stream retries are replay-safe and bounded; broader response retry policy remains |
| Config file watcher and generation reload | partial: parent-directory notification, debouncing, periodic hash reconciliation, complete candidate preparation, reusable listener reservations, atomic in-process activation, bounded drain, rollback, and native-reference re-resolution are implemented; broader active-traffic drain and dependency-watch evidence remain |
| Runtime monitoring snapshot | partial: Linux process/host load, listener traffic, pool/endpoint health, RTMP activity, and redacted Certbot identity/watcher status are implemented; cumulative `u64` wire fields are exact decimal strings, while latency/errors/history/cross-platform sampling remain pending |
| Prometheus exposition | stable for the current process, listener, pool, server, queue, health, retry, certificate, RTMP, and generation families with redacted labels |
| Structured access logs | partial: HTTP file logs emit a fixed redacted JSONL event without URI/query/auth/cookie values; forward-proxy metadata events now emit the same privacy boundary, while file sinks, RTMP, richer result taxonomy, and retention/rotation breadth remain |
| Vue 3 and build-time Pug UI | partial: responsive monitoring/topology observatory, RTMP broadcast desk, and bearer-unlocked canonical configuration workspace with complete current-field editing, validation, Lua/candidate review, conflict handling, and pending-activation save reporting are implemented; certificate/import/event views pending |
| Management API | partial: loopback monitoring/topology, RTMP snapshot/detail, configured exact-ID recorder controls, authenticated config GET/validate/revision-checked durable PUT, listener/pool/server operations, generation actions, TLS reconciliation, process actions, bounded events, readiness/status/metrics, and authenticated stats administration are implemented; broader API/UI workflows remain |
| Revisioned config API | stable for authenticated canonical reads, complete preflight, deterministic preview, conflict-safe durable writes, and explicit `saved_pending_activation`/`unchanged_active` outcomes |
| Config watcher, live generation activation, and bounded event polling | partial: watcher-driven in-process generation activation and bounded structured event polling are implemented; SSE delivery, event durability, and broader drain evidence remain planned |

## RTMP

| Capability | Status |
| --- | --- |
| All 117 nginx-rtmp directive keys/value grammars | partial: tokenizer, registry, and contextual value validation cover all keys; the registry report distinguishes enforced, disable-only, parsed-only, source-no-op, source-bug, deprecated, and platform-limited forms; deterministic includes, inheritance, occurrence accounting, provenance, and finalization exist only for a strict listener/application/recording subset |
| RTMP simple/complex handshake | stable on the live listener through the pinned `rml_rtmp` state machine; the standalone simple-response primitive remains covered independently |
| Chunk formats 0-3 and extended timestamps | partial: `rml_rtmp` transport is active with a 1 MiB inbound chunk limit, a fixed 8 MiB assembled inbound-message limit, and configured outbound chunk size announced on wire; per-configuration `max_message` and exhaustive fragmentation/interleaving tests remain |
| AMF0 connect/createStream/publish/play | partial: configured live publish/play, bounded VOD play, stop, and delete are active; multi-message-stream roles remain absent |
| Active stream snapshot catalog | stable for live publisher/subscriber identity, codec/timestamp/byte observations, duplicate rejection, and disconnect cleanup |
| Active stream management API | stable for snapshot/detail JSON backed by live publisher sessions, with bearer protection on recognized API routes |
| Active stream and recording UI | partial: live recorder snapshots and exact-ID controls for configured manual recorders are implemented; certificate/import/event views remain |
| Live publisher/subscriber fanout | stable with application-scoped canonical subscriber/message/byte bounds, bounded service/per-stream/per-viewer resources, cached metadata/AAC/AVC headers, keyframe gating, slow-viewer resynchronization, and restart reset |
| ACL allow/deny | partial: bounded application publish/play network and stream-token policies are active; broader native directive parity remains |
| Push/pull relay and reconnect | partial: typed push and pull destinations, pinned startup resolution, direct-loop rejection, full `rml_rtmp` client lifecycle, metadata/header/keyframe bootstrap, independent message/byte bounds, publisher-incarnation cancellation, bounded retry/recovery, shutdown, credentials, and redacted state/counters are implemented; dynamic DNS refresh and broader native lowering remain absent |
| FLV recording, VOD, and recorder controls | partial: canonical continuous/manual policies, bounded masks and limits, named local/HTTP VOD sources, RTMP playback workers, authenticated single-range VOD responses, cached manual-start bootstrap, publisher media dispatch, exact-ID bearer-protected controls, catalog completion, nonblocking bounded media queues, redacted observability, safe relative naming, descriptor-pinned storage, atomic publication, process-scoped quotas, bounded pending-task reaping, explicit IANA timezone rendering with historical DST, wall-clock segment-open naming, monotonic keyframe-aligned rotation, and FLV payload under arbitrary suffixes are integrated for legacy AVC/AAC; enhanced AVC/HEVC/AV1 recording, cross-process quota coordination, and broader RTMP control remain absent |
| FLV/MP4 local/HTTP VOD | partial: bounded named local/HTTP objects, FLV RTMP playback workers, safe roots/origins, and authenticated single-range management responses |
| HTTP notify callbacks | partial: bounded HTTP/HTTPS authorization, teardown, and update callbacks with GET/POST methods, resolved-address policy checks, strict update handling, and redacted outcomes; richer nginx callback fields and redirect forwarding remain absent |
| RTMP statistics/control API equivalents | planned RTMP slice 2 |
| HLS H.264/AAC transmuxing and AES keys | partial: bounded MPEG-TS HLS output, variants, atomic storage, cleanup, and AES-128 key rotation are integrated; transcoding and broader origin/auth policy remain absent |
| MPEG-DASH fragmented MP4 output | partial: bounded authenticated AVC/AAC fragmented MP4 segments and MPD `SegmentList` output are active; unsupported codec forms and native nginx-DASH lowering remain explicitly blocked |
| Isolated exec/transcode process integration | partial: typed allowlisted profiles, direct argv/env construction, bounded queues/output/timeouts, publisher correlation, respawn, shutdown, and native publisher/publish-done lowering; broader directive parity and privileged namespace setup remain absent |
| Multi-worker auto-push equivalent | planned RTMP slice 3 |

The live listener pins `rml_rtmp` 0.8.0 behind OxiRoute's session adapter. The adapter enforces
the inbound chunk-size ceiling, a fixed 8 MiB assembled-message allocation ceiling, and bounded
fanout/drain/write policies. The nginx `max_message` directive remains parsed and classified but
does not configure that runtime ceiling.

The directive registry retains a backward-compatible key-level classification and adds explicit
form-level reporting for the narrow lowered subset. Its report distinguishes enforced,
disable-only, parsed-only, source-no-op, source-bug, deprecated, and platform-limited forms.
“All keys parsed” is not advertised as full nginx-rtmp semantic compatibility.

## Certificates

| Capability | Status |
| --- | --- |
| Imported PEM certificate/key | partial: strict bounded direct-file and descriptor-relative Certbot loading creates immutable generations; every declared DNS/IP identity must exist in the typed SAN set, undeclared SANs are not added to named SNI selection, IPv4-mapped IPv6 is canonicalized to IPv4, one TLS profile can select multiple exact/wildcard DNS SNI identities with an explicit default, and Certbot lineages are watched, but direct-file/config reload is absent. As with nginx, an explicit default certificate is still presented for unmatched SNI. |
| Atomic certificate generation publication | partial: independent identity/SAN-bound compare-and-swap publication, complete-generation handshake snapshots, disabled downstream session resumption, existing-connection retention, deterministic concurrent handshake waves, per-identity SNI rotation, a multithreaded publication/snapshot race, process-lifetime Certbot reconciliation, and managed ACME activation are implemented |
| Self-signed development certificate | partial: explicit development generation and in-memory first-start ACME bootstrap are implemented |
| ACME account/order lifecycle | partial: bounded account, order, authorization, CSR, certificate download, and owner-only state paths are implemented; live staging evidence remains |
| ACME HTTP-01 | partial: exact bounded challenge leases and HTTP routing are implemented; explicit listener/deployment evidence remains |
| Automatic renewal and zero-downtime activation | partial: externally renewed Certbot lineages and OxiRoute-managed ACME certificates are reconciled and published without interrupting existing connections; live staging evidence remains |
| Existing Certbot lineage import/watch | stable with strict common-revision snapshots, archive containment, descriptor-relative no-follow reads, key-reuse handling, bounded event coalescing, periodic rescans, directory-watch rebuilding, mixed/invalid retention, and zero-downtime per-identity publication |
| ACME DNS-01 and wildcard names | partial: bounded wildcard identifiers, DNS-01 TXT derivation, statically linked exact-name provider registration, credential/timeout/cancellation bounds, provider propagation hooks, exact cleanup, atomic validated activation, and redacted status are implemented; provider deployments and durable cleanup journaling remain |
| ACME TLS-ALPN-01 | research |
| External/HSM key provider | research |

## Native configuration

| Source | Status | First subset |
| --- | --- | --- |
| OxiRoute Lua | partial | Aggregate/listener admission, Unix modes, downstream and route-local policy, nginx wildcard selectors, bounded headers/auth/cookies/static/gzip/logging, named server capacity and queueing, balancing, DNS, pool timeout/reuse, extended health policy, and RTMP chunk/fanout/push/recording naming policies compile into the runtime. Response buffering requires a positive route body cap and a bounded fixed-length origin response; cache and RTMP file access logging remain fail closed. |
| nginx | partial | `import_root` loads one complete include graph, emits one root terminal decision per occurrence, and merges strict HTTP and nginx-RTMP candidates while retaining deployment concerns. KDL/HOCON/UCI native references resolve that finalized candidate into the canonical runtime and the watcher re-resolves it; standalone CLI report/preview remains offline evidence. Bind lowering preserves accepted exact/wildcard claims on the default server plus its fallback. Value-bearing certificate, htpasswd, bearer, upstream-TLS, and host-timezone overlays must be uniquely supplied and consumed before finalization. Proxy defaults preserve `$proxy_host`, connection-close behavior, bare-second and composite times. Bind-wide identical effective gzip `on`/`off`, level, concrete types, minimum length, HTTP version, eligible statuses, `gzip_proxied off`, and vary policy lower natively; unsupported proxied modes fail closed. Static `etag on|off` lowers with nginx-format validators. Proxy HTTP/1.0 or buffering defaults, active X-Accel controls, non-equivalent TLS session/cipher/DH/H2 policy, formatted logs, and cross-location static reroutes still fail closed. The HTTP fragment API remains available. |
| HAProxy | partial | Ordered roots, deterministic `${NODE_IP}`/`defined(GPU1)` preprocessing, immutable original snapshots, generated-to-original source maps, environment fingerprints, inactive provenance, defaults/reference resolution, decision accounting, diagnostics, provenance, and HTTP/TCP lowering exist. Aggregate/listener/server admission, `roundrobin`/`leastconn`/`first`, reusable HTTP least-connections, DNS/Unix servers, `unix@` modes, cumulative `default-server` health intervals/capacity, exact health timeouts, one literal `http-check send` GET request with canonical URI/version and optional Host, case-insensitive exact Host routes with fixed `503` fallback, one positive exact Host-plus-`path_beg` ACL conjunction, bounded same-server retries with bare final redispatch, timeout scopes including queue deadlines, source-CIDR `forwardfor except`, source server-close policy, and dedicated supported stats pages lower. Reports identify the `haproxy-strict` capability profile and retain ordinary source tables plus preprocessing provenance; native version remains unset without explicit version evidence. Stats authentication/other forms and exact Prometheus services remain non-equivalent activation requirements; a uniquely consumed migration overlay can opt a dedicated `/metrics` section into OxiRoute's different metric families and broader observability routes. Logging/process settings remain explicit deployment warnings. Redispatch interval forms, dynamic/negated/duplicate ACL expressions, maps, arbitrary health-check headers/bodies, and broader native policy remain blocking. |
| Apache httpd | partial | Offline `import apache` reports and previews plus KDL/HOCON/UCI `apache_server` references cover bounded includes, explicit IP listeners, exact virtual hosts, static HTTP/HTTPS ProxyPass destinations, equal-weight balancers, and TLS path identities; rewrites, regex proxying, directory/location merges, response rewriting, dynamic balancer state, unsupported modules, and daemon-native Apache activation remain out of the subset. |
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
| Memory and persistent HTTP caching | partial | Reverse HTTP and eligible HTTP/1 forward requests use bounded memory or descriptor-safe persistent stores; broad Squid admission/storage parity remains absent. |
| Freshness, revalidation, ranges, and collapsed forwarding | partial | Forward and reverse runtime cache timelines, revalidation, collapsed fills, and fail-closed range/conditional bypass are active; broad `refresh_pattern` parity remains absent. |
| Parent and sibling cache peers | partial | Static ordered HTTP parent peers and global `always_direct`/`never_direct` fallback rules are supported for HTTP/1; sibling, dynamic, credentialed, hierarchy, ICP, and peer-option forms remain blocked. |
| External ACL and URL rewrite helpers | unsupported | Helper lifecycle and protocols are not implemented. |
| ICAP and eCAP adaptation | unsupported | Adaptation negotiation and transformation are absent. |
| Transparent interception and TPROXY | unsupported | Kernel/router cooperation is not a socket-proxy capability. |
| TLS bump and certificate mimicry | unsupported | HTTPS-origin verification is not TLS interception. |
| Delay pools and traffic shaping | unsupported | Squid hierarchical bandwidth accounting is absent. |
| FTP proxy and listing behavior | not_planned | Outside the maintained direct HTTP/1 line. |
| ICP, HTCP, WCCP, SNMP, and Cache Manager | not_planned | Legacy datagram, router, and native management protocols are not targets. |
| Process, worker, and deployment controls | not_planned | Externalized or owned by OxiRoute supervision. |
| ESI and content processing | not_planned | Cache-coupled content processing is outside this scope. |
| Obsolete target-version directives | obsolete | Removed or replaced names are blocked. |

The registry deliberately reports `partial` parity and `completeParity: false` while any open
family or directive remains. Cache, refresh, unsupported peer, helper, adaptation, interception, TLS-bump,
legacy, and UDP-related behavior is not advertised by this matrix.
