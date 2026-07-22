# Product specification

## Status and terminology

This is the normative product specification. `MUST`, `MUST NOT`, `SHOULD`, and `MAY`
have their usual requirements meanings. A roadmap item is not a current capability; the
README and `COMPATIBILITY.md` are the sources of truth for implemented behavior.

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
5. Provide a restricted Lua data format as the canonical human-editable configuration.
6. Proxy opaque TCP streams and UDP datagrams so application protocols do not require dedicated modules.
7. Provide load balancing, health checking, limits, and observability consistently across supported transports.
8. Provide a Vue 3 control plane using build-time Pug templates that reflects disk and active runtime state.
9. Issue, import, activate, monitor, and automatically renew TLS certificates through a Certbot-like ACME subsystem.
10. Preserve existing traffic during valid configuration and certificate generation changes.
11. Remain usable without root for unprivileged listeners and ordinary proxy modes.

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
- CONNECT parsing MUST preserve bytes received after the header terminator.
- HTTP/2 and HTTP/3 proxy modes MUST be implemented only against their stream and tunnel standards; they MUST NOT be HTTP/1 tunnels hidden behind version labels.
- Access MUST be default-deny until an operator deliberately enables clients and destinations.
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
- Parent/sibling peers and peer selection.
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

- The first release MUST bind management to loopback by default.
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

Version 0.1 is the scope in Roadmap Milestone 1. Broader goals above remain specifications,
not blockers for the first useful release. This distinction is required to prevent a
multi-year parity effort from delaying a safe basic proxy.
