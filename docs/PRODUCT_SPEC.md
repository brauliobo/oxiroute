# Product specification

## Status and terminology

This is the normative product specification. `MUST`, `MUST NOT`, `SHOULD`, and `MAY`
have their usual requirements meanings. A roadmap item is not a current capability; the
README and `COMPATIBILITY.md` are the sources of truth for current capability status.

Capability labels in the current release contract are deliberately separate:

- `stable`: part of the current supported contract with implementation and the required
  repository evidence for that narrow behavior. This does not imply complete upstream parity or
  a 1.0 release guarantee.
- `partial`: an integrated path exists, but compatibility breadth, failure evidence, or production
  gates are incomplete.
- `foundation`: a component or protocol foundation is tested, but it is not an active daemon
  capability.
- `planned`: committed future implementation work with no complete current runtime path.
- `research`: an evaluated possibility that still needs a product or design decision.
- `not-planned`: deliberately excluded from the product plan for the current boundary.
- `out-of-scope`: belongs to the kernel, a separate privileged helper, or another product boundary.

## Vision

OxiRoute will provide one memory-safe daemon and control plane for reverse proxying,
explicit forward proxying, load balancing, TCP/UDP relays, configuration migration,
certificate automation, and operational monitoring.

It should make common network publishing and port-forwarding tasks simpler than composing
several proxies and hand-maintained packet rules. It does not claim to replace kernel
firewalling, NAT, routing, or conntrack.

## Goals

1. Build the data plane in Rust on Pingora where Pingora owns the required protocol layer.
2. Support HTTP/1, HTTP/2, and HTTP/3 in explicitly reported downstream and upstream modes.
3. Reach functional coverage across maintained Squid feature families, tracked by a public compatibility matrix.
4. Import useful nginx, HAProxy, Apache virtual-host, and Squid configurations with blocking diagnostics for semantic gaps.
5. Provide KDL 2.0 as the canonical human-editable configuration, while retaining restricted Lua
   as a supported compatibility adapter.
6. Proxy opaque TCP streams and UDP datagrams so application protocols do not require dedicated modules.
7. Provide load balancing, health checking, limits, and observability consistently across supported transports.
8. Provide a Vue 3 control plane using build-time Pug templates that reflects disk and active runtime state.
9. Issue, import, activate, monitor, and automatically renew TLS certificates through a Certbot-like ACME subsystem.
10. Preserve existing traffic during valid configuration and certificate generation changes.
11. Remain usable without root for unprivileged listeners and ordinary proxy modes.
12. Provide RTMP live publish/play, relay, recording, VOD, HLS/DASH output, callbacks, controls, and nginx-rtmp configuration compatibility.

## Non-goals

- Reimplement packet filtering, DNAT, SNAT, MASQUERADE, policy routing, or connection tracking in user space.
- Silently approximate unsupported native configuration behavior.
- Execute unrestricted Lua configuration or user-provided Pug/JavaScript templates.
- Copy upstream implementations instead of building against documented behavior and preserving required license notices.
- Implement cryptographic primitives instead of using reviewed Rust or system libraries.
- Promise protocol support based only on an enabled build flag; active listeners and peers must report their negotiated capabilities.

## Users

- An operator replacing a small nginx, HAProxy, Apache, or Squid deployment.
- A developer publishing several local HTTP and TCP services through one daemon.
- A network operator who wants observable user-space relays without hand-writing common forwarding rules.
- A platform team managing certificates and routing through an API and UI.

## Functional requirements

### Configuration

- The daemon MUST load one canonical typed snapshot before opening listeners.
- The Lua evaluator MUST be text-only, resource-bounded, and expose no filesystem, process, network, package, dynamic-load, or debug facilities.
- Unknown fields, duplicate identities, conflicting binds, unresolved references, and unsupported semantics MUST fail validation.
- Reload MUST parse, validate, prepare, and then activate a complete generation.
- A failed candidate MUST leave the prior active generation and certificate set intact.
- Disk and active revisions MUST be independently observable.

### Reverse HTTP proxy

- Listeners MUST support host and path routing, request limits, timeouts, upstream pools, retries with safe method/body rules, WebSocket, and gRPC as their milestones land.
- HTTP versions MUST be modeled independently for downstream and upstream connections.
- Hop-by-hop headers MUST be normalized according to the negotiated protocol.
- TLS termination, upstream TLS verification, SNI, ALPN, and client-certificate policy MUST be explicit.
- Route precedence MUST be deterministic and visible through diagnostics and the UI.

### Explicit forward proxy

- HTTP/1 absolute-form requests and CONNECT MUST use a dedicated parser/tunnel path.
- Opt-in HTTP/1 RFC 9298 `CONNECT-UDP` MUST use the HTTP/1.1 Upgrade and Capsule Protocol path,
  with explicit destination-port policy; it is a `forward_http1` capability only.
- CONNECT parsing MUST preserve bytes received after the header terminator.
- HTTP/2 and HTTP/3 proxy modes MUST be implemented only against their stream and tunnel standards;
  they MUST NOT be HTTP/1 tunnels hidden behind version labels. The current forward-proxy slice
  exposes authority-only classic CONNECT on H2/H3 and bounded H3 absolute-form forwarding; arbitrary
  H2 request forms, CONNECT-UDP over H2/H3, and broader H3 conformance are not current capabilities.
- Non-public and special-use destinations MUST be denied by default. An omitted authentication
  policy and empty allow lists MAY permit public destinations; configured domain/CIDR allow lists
  constrain the complete target and DNS answer, and deny rules always override allows.
- Destination policy MUST be checked against resolved addresses to prevent DNS-based policy bypass and SSRF.
- Proxy credentials and hop-by-hop proxy headers MUST NOT reach origin servers.

### Squid coverage

The compatibility matrix MUST separately track:

- Forward requests and CONNECT.
- ACL types, ordering, and asynchronous lookups.
- Authentication schemes and helper integrations.
- Memory and persistent HTTP caching.
- Freshness, revalidation, collapsed forwarding, range behavior, and purge.
- Delay pools and traffic limits.
- URL rewrite and external ACL helpers.
- ICAP/eCAP adaptation.
- Parent/sibling peers and peer selection, except the bounded static HTTP parent subset and global
  direct-fallback rules explicitly listed in the compatibility registry.
- Transparent interception and TLS bump.
- Logging, management reports, and operational controls.
- ICP, HTCP, WCCP, and other legacy integrations.

A feature is `compatible` only after behavior and failure cases have conformance tests.
Features may be marked `not planned` with rationale, but the project MUST NOT advertise
complete Squid parity while such entries remain.

### Layer-4 and datagram proxying

- TCP mode MUST relay opaque bytes with bounded buffering, backpressure, independent half-close handling, connect/idle/lifetime timeouts, and graceful drain.
- TLS pass-through is ordinary TCP relay behavior; optional SNI inspection MUST be bounded and MUST preserve all peeked bytes.
- UDP mode MUST maintain bounded pseudo-sessions keyed by listener and client identity, route replies correctly, expire idle state, and enforce datagram and table limits.
- TCP and UDP MAY carry any application protocol that does not require kernel transparency or unsupported socket semantics.
- ICMP, arbitrary IP protocols, source spoofing, and transparent transit forwarding are outside the ordinary relay abstraction.
- PROXY protocol MAY explicitly propagate client addresses when both endpoints support it.

### RTMP media service

- RTMP behavior and nginx-rtmp directive compatibility MUST follow `RTMP_SPEC.md`.
- The server MUST distinguish imported/validated directives from runtime-enforced directives.
- Live fanout MUST use bounded per-subscriber queues and deterministic media-drop/resynchronization policy.
- RTMP callbacks, process execution, file recording, relays, and segment output MUST have independent security and resource policies.
- HLS and DASH output are media transmuxing features, not generic HTTP proxy behavior.

### Load balancing

- Initial algorithms: round robin, weighted round robin, and least connections.
- Health checks MUST distinguish transport, HTTP, and protocol-specific probes.
- Passive failures, active health, ejection, recovery, and retry budgets MUST be observable.
- A request or flow MUST use one immutable pool generation.
- UDP affinity policy and expiry MUST be explicit.

### Native configuration imports

- Each product MUST have a separate parser and semantic resolver.
- Imported objects MUST retain product, file, line, include stack, and original directive provenance.
- Unsupported behavior MUST produce stable, actionable diagnostics and block affected services.
- UI edits MUST write canonical OxiRoute config, not destructively rewrite legacy files by default.
- Watched native files MAY trigger re-import into a candidate generation as specified in `IMPORT_SPEC.md`.

### Certificate management

- Certificates MAY be imported, generated for local development, or managed through ACME.
- ACME issuance and renewal MUST follow `ACME_SPEC.md`.
- Private keys MUST never appear in normal API responses, events, metrics, or logs.
- Certificate activation MUST validate key match, chain, names, validity, and listener references before replacing an active version.
- Existing connections MUST continue using the certificate context selected during their handshake.

### Control plane and UI

- The current release MUST bind management to loopback by default.
- API writes MUST use content-hash optimistic concurrency.
- Vue MUST receive typed JSON; it MUST NOT generate Lua fragments in the browser.
- Pug MUST be used only as a build-time Vue SFC template preprocessor.
- Filesystem changes MUST reach clients through backend events.
- A dirty UI draft MUST be marked stale rather than overwritten after an external edit.

### Observability

- Every request, tunnel, flow, reload, import, and certificate job MUST have a stable result category.
- Metrics MUST cover traffic, errors, latency, connections, pool health, reload state, certificate expiry, and ACME jobs.
- Logs MUST be structured and redact authorization fields, cookies where configured, private keys, ACME account material, and challenge credentials.
- Status MUST expose software version, active config revision, disk revision, listener state, and degraded components.

## Non-functional requirements

- Linux x86_64 and aarch64 are tier-one targets.
- Unsafe code is forbidden in project crates unless a future exception has a reviewed design record.
- The daemon SHOULD run as an unprivileged user after any explicitly configured privileged setup.
- Configuration and control-plane operations MUST have bounded input and execution resources.
- No request-controlled destination may bypass policy through redirects, retries, DNS changes, or alternate address families.
- New features require unit, integration, failure-path, and observable-state tests before being marked supported.
- Releases MUST publish the exact protocol, importer, and Squid compatibility matrices.

## Release definition

The package currently declares version `0.4.1`. The current release line is pre-alpha, not a 1.0
stability claim. The compatibility matrix records the supported narrow paths for this working
release line, while the roadmap records work that is not yet available.

The current contract is:

- `stable`: KDL 2.0 default authoring and deterministic rendering, strict typed configuration,
  loopback management exposure, bearer protection for recognized management/API routes, public
  readiness and metrics probes, round-robin, weighted round-robin, least-connections, and first
  selection, bounded configurable safe retry/passive-health policies, bounded event polling, durable
  redacted audit history, and external Certbot lineage reconciliation.
- `partial`: reverse HTTP and TCP/UDP relay with bounded explicit PROXY protocol propagation, HTTP/1
  forward absolute-form/CONNECT/CONNECT-UDP, authority-only classic CONNECT on H2/H3, bounded H3
  absolute-form forwarding, reverse HTTP/3, RTMP live/recording/relay slices,
  bounded authenticated SSE delivery, managed ACME HTTP-01/DNS-01/TLS-ALPN-01 issuance, wildcard
  renewal, bounded reverse and eligible HTTP/1/H3 forward cache, structured access logs, RTMP statistics/session controls,
  HLS/DASH/isolated-exec/auto-push slices, and native nginx/HAProxy/Apache/Squid/Varnish import
  subsets with canonical provenance and a partial Vue control plane. Native import reports are
  browsable as read-only evidence, weighted-round-robin weight editing and durable audit browsing
  are current frontend workflows, while native source editing remains outside the frontend. TLS-ALPN
  challenge selection and listener-deployment guidance are exposed; listener deployment and
  CA-staging evidence remain gates.
- `foundation`: forward-proxy request forms outside the active HTTP/1/H3 absolute-form and
  authority-only H2/H3 classic CONNECT paths; these are not active daemon capabilities.
- `planned`: broader cache conformance, broader managed ACME authenticators, durable replay/history
  for the non-durable operational event ring, and broader protocol/import compatibility.
- `research`: remote administration, broader DNS provider policy, external key providers, and
  transparent interception through a separate privileged helper.
- `not-planned`: unrestricted Lua and runtime user-provided templates.
- `out-of-scope`: firewalling, NAT, packet forwarding, source spoofing, and other kernel-owned
  network functions.

Broader goals above remain specifications, not current capability claims. A feature moves to
`stable` only when its implementation, failure behavior, observability, reload/rotation behavior,
and interoperability evidence meet the applicable release gate.

## Remaining release gates

The current narrow implementation is not a production-parity claim. The following evidence is
required before the affected partial or foundation paths can become a broader supported contract:

- Active traffic: reload and drain with long-lived HTTP, H2, H3, TCP, UDP, RTMP, and SSE activity,
  including no-new-work admission after GOAWAY/quiesce, deadline/cancellation behavior, and old
  generation retention. H3 generation-owned GOAWAY drain is implemented and targeted-tested; this
  gate is for active-traffic evidence across the supported transports.
- ACME staging: CA-staging issuance and renewal for HTTP-01, DNS-01, and TLS-ALPN-01, including
  listener/deployment checks, failed challenge cleanup, rollback, and real certificate activation.
  TLS-ALPN challenge handling and DNS exact-record cleanup/recovery are implemented and tested;
  staging deployment and real-certificate evidence remain open.
- UI/import exposure: native import remains a read-only redacted report/compositional-source
  workflow, while the frontend exposes durable audit browsing and weighted-round-robin fields.
  Native-file editing remains intentionally absent. TLS-ALPN challenge selection and its
  listener-deployment guidance are exposed in the frontend; listener deployment and CA-staging
  evidence remain open gates. Passive health and retry controls are exposed in the frontend.
  These are exposure boundaries, not claims that the corresponding backend behavior is absent.
- Interoperability: independent HTTP/H2/H3/TLS clients and origins, FFmpeg/OBS RTMP publish/play,
  and representative Apache, HAProxy, Squid, and Varnish migration cases beyond synthetic fixtures.
- Fuzz and crash: bounded runs of every checked-in parser harness, crash-corpus triage, and failure
  injection for media workers, exec workers, reload activation, and listener supervision.
- Production supervision: packaged Linux master/worker replacement and rollback with active UDP and
  H3 traffic, descriptor ownership across restart, drain, restart, and crash recovery. Initial
  supervised UDP/H3 serving and generic replacement/error paths are tested. On Linux, eligible
  `serve` configurations use supervision when the fixed launcher is installed, including the Arch
  package; unsupported topologies, unpackaged installs without it, and non-Linux builds run direct.
  Active-traffic production evidence remains a broader-support gate, not a default-mode gate.
