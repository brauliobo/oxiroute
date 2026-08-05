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
- Expand audited native configuration coverage across nginx/nginx-RTMP, HAProxy, Squid, Varnish, and
  Apache subsets while retaining blocking diagnostics for unsupported semantics.
- Continue the bounded supervision master/worker and Arch launcher path with worker status and
  managed configuration support; the default public entry point remains direct `oxiroute serve`.

## Compatibility Boundaries

- **HTTP/3: partial.** The active reverse and forward listeners cover bounded routing/forwarding and
  classic CONNECT forms. Static files, cache, compression, upgrades, broad HTTP conformance, and
  arbitrary forward HTTP/2/HTTP/3 forms remain unsupported.
- **HTTP caching: partial.** Only the implemented reverse and eligible HTTP/1 forward GET/HEAD paths
  are cacheable. Broader HTTP cache conformance and Squid refresh/cache semantics are not provided.
- **Squid: partial.** The importer lowers an audited direct HTTP/1 and CONNECT subset plus ordered
  static parent peers and global direct-fallback rules. Sibling/dynamic/credentialed peer forms,
  peer hierarchy, helper protocols, ICAP/eCAP, transparent interception, TLS bump, legacy datagram
  protocols, and native cache-manager behavior remain unsupported.
- **RTMP: partial.** Live publish/play, recording/VOD, HLS, DASH, relays, and selected controls are
  bounded slices. Complete nginx-RTMP directive parity, transcoding, unsupported codecs, broader
  callback/control parity, and multi-worker auto-push remain absent.
- **Supervision: partial/foundation.** The master, worker, launcher, authentication, status, drain,
  and replacement foundations are tested, but the public default remains direct runtime operation;
  UDP and HTTP/3 remain on their direct generation runtimes and broader production migration evidence
  is still required.
- **Managed ACME: partial.** HTTP-01 and bounded DNS-01/wildcard lifecycle paths are implemented,
  including static exact-name provider registration and redacted state. Provider deployment,
  durable cleanup journaling, live staging evidence, and TLS-ALPN-01 remain gaps.
- **Native import: partial.** Importers preserve provenance and fail closed for unsupported or lossy
  forms. No complete nginx, HAProxy, Squid, Varnish, Apache, or nginx-RTMP compatibility is claimed.

See [COMPATIBILITY.md](COMPATIBILITY.md), [ROADMAP.md](ROADMAP.md), and the protocol specifications
for exact supported forms, limits, and remaining work.
