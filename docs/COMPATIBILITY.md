# Compatibility matrix

## Status meanings

- `implemented`: code and initial tests exist in this repository.
- `partial`: a narrow path exists but the documented compatibility gate is incomplete.
- `planned M1` through `planned M4`: assigned to a roadmap milestone.
- `research`: no committed implementation milestone yet.
- `out of scope`: conflicts with the user-space proxy boundary.

This file is updated in the same commit that changes a capability. Build support alone is
not protocol support.

## Proxy and transport

| Capability | Status | Notes |
| --- | --- | --- |
| HTTP/1 reverse proxy | partial | Deterministic nginx-style routing, health-aware tagged pools, balancing, active checks, bounded safe connect retries, independent listener/route/pool timeouts, route-local body limits, bounded header/cookie policy, semaphore-bounded bcrypt Basic access, descriptor-pinned conditional/range static serving, streaming gzip, bounded asynchronous redacted JSONL access logs, WebSocket upgrades, downstream TLS, and verified upstream TLS are implemented; buffering-on, total request deadlines, graceful drain, and broader conformance remain. |
| HTTP/2 downstream | partial | TLS profiles provide explicit `h2` ALPN and a rustls client wire test negotiates H2 and proxies a real stream. H2-only listeners reject incompatible ALPN and close no-ALPN streams before HTTP parsing. There is no h2c, client authentication, or broad H2 conformance suite. |
| HTTP/2 upstream | partial | TLS pools support `1.1/1.1`, `1.1/2`, and H2-only `2/2` policies. H2-only success and no-compatible-ALPN failure before HTTP headers are wire-tested with verified custom CA and SNI; the HTTP/1.1-only policy is also wire-tested. |
| HTTP/3 downstream/upstream | planned M4 | Pingora has no current H3 stack; requires a tested QUIC/H3 integration. |
| WebSocket reverse proxy | implemented | The standard nginx `Upgrade $http_upgrade` and `Connection upgrade` policy is accepted without taking upgrade ownership from Pingora; HTTP/1.1 passes independent bidirectional framed interoperability coverage. |
| gRPC reverse proxy | partial | The TLS/H2 path wire-tests gRPC response DATA, successful trailers, and trailers-only error metadata end to end; streaming breadth, deadlines, cancellation, and broader conformance remain. |
| HTTP/1 explicit forward proxy | partial | A standalone wire-tested absolute-form/CONNECT foundation exists; canonical policy and daemon integration remain M3. |
| HTTP/2 forward proxy/CONNECT | partial | A standalone wire-tested stream foundation exists; canonical policy and daemon integration remain M3. |
| HTTP/3 forward proxy | partial | A standalone QUIC/H3 wire foundation exists; it is not a Pingora listener or daemon runtime and integrated support remains M4. |
| Opaque TCP relay | partial | Bounded bidirectional relay, independent half-close, configured connect/idle/lifetime timeouts, socket/DNS/Unix upstreams, health-aware round-robin or relay-scoped least-connections pools, active TCP/HTTP checks for non-Unix pools, nullable listener connection caps, shutdown cancellation, partial traffic accounting, and loopback tests are implemented; Unix transports require Unix, and reload and graceful process drain remain. |
| TLS pass-through | partial | Opaque bytes can traverse the implemented TCP relay without termination; no SNI inspection, TLS-specific policy, or dedicated pass-through conformance suite exists. |
| UDP relay | planned M2 | Requires bounded pseudo-session and reply-routing design. |
| PROXY protocol | planned M2 | Explicit propagation only. |
| ICMP/arbitrary IP protocols | out of scope | Requires packet-level/kernel integration, not sockets. |
| Transparent interception/source spoofing | research | Optional separate privileged helper only; never ordinary proxy behavior. |
| DNAT/SNAT/MASQUERADE/firewall | out of scope | Remains kernel nftables/iptables functionality. |

OxiRoute carries a narrow pinned `pingora-core` 0.8.1 patch that exposes a per-peer OpenSSL
configuration hook and pre-handshake application admission. Upstream TLS applies a TLS 1.2 minimum,
security level 2, strict partial-chain verification, and modern AEAD cipher policy before handshake;
in-process TLS 1.0/1.1 controls verify refusal without origin HTTP bytes. The patch and upgrade
procedure are documented in `vendor/pingora-core/README.oxiroute.md`.

## Load balancing and operations

| Capability | Status |
| --- | --- |
| Single-endpoint pools | implemented |
| Static round robin | implemented |
| Static least connections | implemented with deterministic tie rotation and request/relay-scoped active leases |
| Active TCP/HTTP health checks | implemented with configured startup eligibility, normal/fast/down completion-based schedules, consecutive transition thresholds, HTTP version/optional Host/exact status policy, and a shared 32-probe limit |
| Weighted round robin | planned M2 |
| Bounded connect-failure retries | implemented for bodyless `GET`/`HEAD`, distinct canonical endpoint identities, and at most two additional attempts |
| Response retries and passive ejection | planned M1 |
| Config file watcher and generation reload | implemented with parent-directory notification, debouncing, periodic hash reconciliation, complete candidate preparation, reusable listener reservations, atomic in-process activation, bounded graceful drain, and rollback |
| Runtime monitoring snapshot | partial: Linux process/host load, listener traffic, pool/endpoint health, RTMP activity, and redacted Certbot identity/watcher status are implemented; cumulative `u64` wire fields are exact decimal strings, while latency/errors/history/cross-platform sampling remain pending |
| Structured access logs and Prometheus metrics | partial: HTTP file logs emit a fixed redacted JSONL event without URI/query/auth/cookie values; Prometheus exposes process, listener, pool, server, queue, health, retry, certificate, RTMP, and generation families with redacted labels |
| Vue 3 and build-time Pug UI | partial: responsive monitoring/topology observatory, RTMP broadcast desk, and bearer-unlocked canonical configuration workspace with complete current-field editing, validation, Lua/candidate review, conflict handling, and pending-activation save reporting are implemented; certificate/import/event views pending |
| Management API | partial: loopback monitoring/topology, RTMP snapshot/detail, configured exact-ID recorder controls, authenticated config GET/validate/revision-checked durable PUT, readiness/status/metrics, and authenticated stats administration are implemented; recorder controls remain loopback-only but unauthenticated |
| Revisioned config API | implemented for authenticated canonical reads, complete preflight, deterministic preview, conflict-safe durable writes, and explicit `saved_pending_activation`/`unchanged_active` outcomes |
| Config watcher, live generation activation, and SSE events | partial: watcher-driven in-process generation activation and structured operational events are implemented; SSE delivery remains planned |

## RTMP

| Capability | Status |
| --- | --- |
| All 117 nginx-rtmp directive keys/value grammars | partial: tokenizer, registry, and contextual value validation cover all keys; deterministic includes, inheritance, occurrence accounting, provenance, and finalization exist only for a strict listener/application/recording subset |
| RTMP simple/complex handshake | implemented on the live listener through the pinned `rml_rtmp` state machine; the standalone simple-response primitive remains covered independently |
| Chunk formats 0-3 and extended timestamps | partial: `rml_rtmp` transport is active with a 1 MiB inbound chunk limit and configured outbound chunk size announced on wire; configurable assembled-message limits and exhaustive fragmentation/interleaving tests remain |
| AMF0 connect/createStream/publish/play | partial: configured live publish/play, stop, and delete are active; VOD and multi-message-stream roles remain absent |
| Active stream snapshot catalog | implemented for live publisher/subscriber identity, codec/timestamp/byte observations, duplicate rejection, and disconnect cleanup |
| Active stream management API | implemented for snapshot/detail JSON backed by live publisher sessions |
| Active stream and recording UI | implemented against live recorder snapshots; exact-ID controls are available for configured manual recorders and disabled when the active stream has no controllable recorder |
| Live publisher/subscriber fanout | implemented with application-scoped canonical subscriber/message/byte bounds, bounded service/per-stream/per-viewer resources, cached metadata/AAC/AVC headers, keyframe gating, slow-viewer resynchronization, and restart reset |
| ACL allow/deny | planned RTMP slice 2 |
| Push/pull relay and reconnect | partial: typed push destinations, pinned startup resolution, direct-loop rejection, full `rml_rtmp` client lifecycle, metadata/header/keyframe bootstrap, independent message/byte bounds, publisher-incarnation cancellation, bounded retry/recovery, shutdown, and redacted state/counters are implemented; pull, credentials, dynamic DNS refresh, and native lowering remain absent |
| FLV recording and recorder controls | partial: canonical continuous/manual policies, cached manual-start bootstrap, publisher media dispatch, exact-ID controls, catalog completion, nonblocking bounded media queues, redacted observability, safe relative naming, descriptor-pinned storage, atomic publication, process-scoped quotas, bounded pending-task reaping, explicit IANA timezone rendering with historical DST, wall-clock segment-open naming, monotonic keyframe-aligned rotation, and FLV payload under arbitrary suffixes are integrated for legacy AVC/AAC; enhanced AVC/HEVC/AV1 recording, cross-process quota coordination, and authenticated remote control remain absent |
| FLV/MP4 local/HTTP VOD | planned RTMP slice 2 |
| HTTP notify callbacks | planned RTMP slice 2 |
| RTMP statistics/control API equivalents | planned RTMP slice 2 |
| HLS H.264/AAC transmuxing and AES keys | planned RTMP slice 3 |
| MPEG-DASH fragmented MP4 output | planned RTMP slice 3 |
| Isolated exec/transcode process integration | planned RTMP slice 3 |
| Multi-worker auto-push equivalent | planned RTMP slice 3 |

The live listener pins `rml_rtmp` 0.8.0 behind OxiRoute's session adapter. The adapter enforces
the inbound chunk-size ceiling and bounded fanout/drain/write policies, but the dependency does not
expose an assembled-message allocation limit.

The directive registry reports each key as enforced, parsed-not-enforced, source-no-op,
source-bug, deprecated, or platform-limited. “All keys parsed” is not advertised as full
nginx-rtmp semantic compatibility.

## Certificates

| Capability | Status |
| --- | --- |
| Imported PEM certificate/key | partial: strict bounded direct-file and descriptor-relative Certbot loading creates immutable generations; one TLS profile can select multiple exact/wildcard SNI identities with an explicit default, and Certbot lineages are watched, but direct-file/config reload is absent |
| Atomic certificate generation publication | partial: independent identity/SAN-bound compare-and-swap publication, complete-generation handshake snapshots, disabled downstream session resumption, existing-connection retention, deterministic concurrent handshake waves, per-identity SNI rotation, a multithreaded publication/snapshot race, and process-lifetime Certbot reconciliation are implemented; managed ACME activation remains absent |
| Self-signed development certificate | planned M1 |
| ACME account/order lifecycle | planned M1 |
| ACME HTTP-01 | planned M1 |
| Automatic renewal and zero-downtime activation | partial: externally renewed Certbot lineages are reconciled and published without interrupting existing connections; OxiRoute-managed ACME issuance and scheduling remain planned M1 |
| Existing Certbot lineage import/watch | implemented with strict common-revision snapshots, archive containment, descriptor-relative no-follow reads, key-reuse handling, bounded event coalescing, periodic rescans, directory-watch rebuilding, mixed/invalid retention, and zero-downtime per-identity publication |
| ACME DNS-01 and wildcard names | planned M2 |
| ACME TLS-ALPN-01 | research |
| External/HSM key provider | research |

## Native configuration

| Source | Status | First subset |
| --- | --- | --- |
| OxiRoute Lua | partial | Aggregate/listener admission, Unix modes, downstream and route-local policy, nginx wildcard selectors, bounded headers/auth/cookies/static/gzip/logging, named server capacity and queueing, balancing, DNS, pool timeout/reuse, extended health policy, and RTMP chunk/fanout/push/recording naming policies compile into the runtime. Buffering-on, cache, and RTMP file access logging fail closed. |
| nginx | partial | `import_root` loads one complete include graph, emits one root terminal decision per occurrence, and merges strict HTTP and nginx-RTMP candidates while retaining deployment concerns. Bind lowering uses only first-loaded accepted exact/wildcard claims. Value-bearing certificate, htpasswd, bearer, upstream-TLS, and host-timezone overlays must be uniquely supplied and consumed before finalization. Proxy HTTP/1.0 or buffering defaults, active X-Accel controls, non-equivalent TLS session/cipher/DH/H2 policy, formatted logs, gzip, and cross-location static reroutes fail closed. The HTTP fragment API remains available; the daemon binary provides offline report/preview only. |
| HAProxy | partial | Ordered roots, deterministic `${NODE_IP}`/`defined(GPU1)` preprocessing, immutable original snapshots, generated-to-original source maps, environment fingerprints, inactive provenance, defaults/reference resolution, decision accounting, diagnostics, provenance, and HTTP/TCP lowering exist. Aggregate/listener/server admission, `roundrobin`/`leastconn`/`first`, DNS/Unix servers, cumulative `default-server` health intervals and capacity, healthy-startup checks, bounded same-server retries, independent timeout scopes including queue deadlines, source-CIDR `forwardfor except`, source server-close policy, and a strict backend-scoped one-request-per-connection audit overlay lower. Native stats and exact Prometheus forms remain non-equivalent activation requirements. A uniquely consumed migration overlay can explicitly opt an exact dedicated `/metrics` section into OxiRoute's different metric families and broader stats routes. Logging/process settings remain deployment requirements. Arbitrary ACLs and broader native policy remain blocking. |
| Apache httpd | planned M2 | Static virtual hosts, TLS paths, and HTTP ProxyPass/balancers. |
| Squid | partial | A bounded library foundation loads deterministic includes, rechecks source/path/glob identity, and classifies the sanitized Squid 7.6 inventory into typed semantic facts. It emits no canonical config and has no runtime or daemon integration; the first integrated subset remains M3. |
| Varnish VCL | partial | A bounded library foundation provides ordered includes, parsing, a typed semantic IR, decision accounting, and an invocation model. It has no canonical lowering, runtime, or daemon integration. |

## Squid feature families

Squid configuration contains hundreds of directives whose availability changes by build.
The importer will generate a versioned directive registry from the targeted upstream
release and assign each directive `compatible`, `partial`, `unsupported`, or `obsolete`.
Until that registry and behavior suite exist, no complete parity claim is valid.

| Family | Status | Required work |
| --- | --- | --- |
| Explicit HTTP requests | planned M3 | Parser, forwarding, policy, auth, logging. |
| CONNECT tunnels | planned M3 | Over-read preservation, duplex semantics, destination policy. |
| ACL definitions and access ordering | planned M3 | Typed sync/async predicates and first-match behavior. |
| Basic proxy authentication | planned M3 | Secure static/mTLS identities first. |
| Digest/Negotiate/NTLM and helpers | research | Helper protocol and connection-affinity semantics. |
| External ACL and URL rewrite helpers | research | Isolated helper lifecycle and concurrency. |
| Memory cache | planned M4 | A bounded RFC-aware core exists, including cache-bound prepared-entry ownership, but active route policies fail startup until exact server integration lands. |
| Persistent disk cache | planned M4 | A descriptor-safe persistent core exists and rejects foreign prepared entries before disk admission, but active route policies fail startup; async request-path integration and storage coordination remain. |
| Revalidation/range/collapsed forwarding | planned M4 | HTTP cache conformance. |
| Delay pools | research | Hierarchical bandwidth accounting and compatibility tests. |
| ICAP/eCAP adaptation | research | Separate adaptation protocol/plugin architecture. |
| Parent/sibling cache peers | research | Selection, failure, direct/never-direct policies. |
| ICP/HTCP | research | Legacy datagram peer protocols. |
| WCCP | research | Kernel/router cooperation and operational relevance review. |
| Transparent interception/TPROXY | research | Optional Linux helper and policy routing. |
| TLS bump/certificate mimicry | research | High-risk security and policy feature; not part of initial ACME work. |
| FTP proxy/listing behavior | research | Current Squid target-version capability review required. |
| ESI and content processing | research | Standards/relevance review and cache integration. |
| SNMP and Cache Manager equivalents | research | Prefer Prometheus/API equivalents; exact compatibility assessed separately. |
| Logging formats and rotation | research | Structured native model plus selected format imports. |
| Multi-worker coordination | planned M1 | Pingora lifecycle first; shared cache/state later. |

The matrix expands to directive-level coverage before a Squid-compatible release. Features
marked `research` are goals under evaluation, not promises for the first releases.
