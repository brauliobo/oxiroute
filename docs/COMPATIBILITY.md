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
| HTTP/1 reverse proxy | partial | Static one-upstream Pingora path exists; TLS, routing, limits, and conformance remain M1. |
| HTTP/2 downstream | planned M1 | Requires TLS/ALPN listener configuration and conformance tests. |
| HTTP/2 upstream | planned M1 | Requires explicit peer version policy and negotiation tests. |
| HTTP/3 downstream/upstream | planned M4 | Pingora has no current H3 stack; requires a tested QUIC/H3 integration. |
| WebSocket reverse proxy | implemented | Pingora HTTP/1.1 upgrade path passes independent bidirectional framed interoperability coverage. |
| gRPC reverse proxy | planned M1 | Requires working HTTP/2 modes and trailer tests. |
| HTTP/1 explicit forward proxy | planned M3 | Includes absolute-form requests and a dedicated CONNECT tunnel. |
| HTTP/2 forward proxy/CONNECT | planned M3 | Only after stream takeover and policy conformance. |
| HTTP/3 forward proxy | planned M4 | Requires explicit H3 proxy/tunnel standards support. |
| Opaque TCP relay | partial | Static Pingora connector path exists; timeout, half-close, reload, and integration gates remain M1. |
| TLS pass-through | planned M1 | Uses opaque TCP; bounded SNI inspection is separate. |
| UDP relay | planned M2 | Requires bounded pseudo-session and reply-routing design. |
| PROXY protocol | planned M2 | Explicit propagation only. |
| ICMP/arbitrary IP protocols | out of scope | Requires packet-level/kernel integration, not sockets. |
| Transparent interception/source spoofing | research | Optional separate privileged helper only; never ordinary proxy behavior. |
| DNAT/SNAT/MASQUERADE/firewall | out of scope | Remains kernel nftables/iptables functionality. |

## Load balancing and operations

| Capability | Status |
| --- | --- |
| One static upstream | implemented |
| Round robin and active health checks | planned M1 |
| Weighted round robin and least connections | planned M2 |
| Retry budgets and passive ejection | planned M1 |
| Config file watcher and generation reload | planned M1 |
| Structured access logs and Prometheus metrics | planned M1 |
| Vue 3 and build-time Pug UI | partial: responsive RTMP broadcast desk and static serving implemented; broader config/certificate/import views pending |
| Management API | partial: loopback RTMP snapshot/detail and recorder-control routes implemented; config writes/auth/events pending |
| Revisioned API and SSE events | planned M1 |

## RTMP

| Capability | Status |
| --- | --- |
| All 117 nginx-rtmp directive keys/value grammars | partial: tokenizer, structural parser, registry, and contextual value validation implemented; include resolution/inheritance lowering pending |
| RTMP simple/complex handshake | partial: simple `S0+S1+S2` response implemented; complex handshake pending |
| Chunk formats 0-3 and extended timestamps | planned RTMP slice 1 |
| AMF0 connect/createStream/publish/play | planned RTMP slice 1 |
| Active stream snapshot catalog | partial: immutable publisher/subscriber/media/recorder snapshots implemented; live session attachment pending |
| Active stream management API | implemented for snapshot/detail JSON; production remains empty until live sessions attach |
| Active stream and recording UI | implemented against the management API; recording controls remain disabled while the backend capability is absent |
| Live publisher/subscriber fanout | planned RTMP slice 1 |
| ACL allow/deny | planned RTMP slice 2 |
| Push/pull relay and reconnect | planned RTMP slice 2 |
| FLV recording and recorder controls | partial: manual recorder transition model implemented; FLV backend and RTMP command dispatch pending |
| FLV/MP4 local/HTTP VOD | planned RTMP slice 2 |
| HTTP notify callbacks | planned RTMP slice 2 |
| RTMP statistics/control API equivalents | planned RTMP slice 2 |
| HLS H.264/AAC transmuxing and AES keys | planned RTMP slice 3 |
| MPEG-DASH fragmented MP4 output | planned RTMP slice 3 |
| Isolated exec/transcode process integration | planned RTMP slice 3 |
| Multi-worker auto-push equivalent | planned RTMP slice 3 |

The directive registry reports each key as enforced, parsed-not-enforced, source-no-op,
source-bug, deprecated, or platform-limited. “All keys parsed” is not advertised as full
nginx-rtmp semantic compatibility.

## Certificates

| Capability | Status |
| --- | --- |
| Imported PEM certificate/key | planned M1 |
| Self-signed development certificate | planned M1 |
| ACME account/order lifecycle | planned M1 |
| ACME HTTP-01 | planned M1 |
| Automatic renewal and zero-downtime activation | planned M1 |
| Existing Certbot lineage import/watch | planned M1 |
| ACME DNS-01 and wildcard names | planned M2 |
| ACME TLS-ALPN-01 | research |
| External/HSM key provider | research |

## Native configuration

| Source | Status | First subset |
| --- | --- | --- |
| OxiRoute Lua | partial | Strict static HTTP/TCP listener schema exists. |
| nginx | planned M2 | Static HTTP virtual hosts/upstreams plus stream TCP/UDP. |
| HAProxy | planned M2 | Static HTTP/TCP frontends, backends, simple ACL switching. |
| Apache httpd | planned M2 | Static virtual hosts, TLS paths, and HTTP ProxyPass/balancers. |
| Squid | planned M3 | Explicit proxy listener, supported ACL/access subset, direct upstream. |

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
| Memory cache | planned M4 | Production bounds, freshness, vary, locking, purge. |
| Persistent disk cache | planned M4 | Index, recovery, eviction, storage coordination. |
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
