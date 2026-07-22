# Upstream analysis

The reference repositories were cloned shallowly into `/home/braulio/Projects` on
2026-07-22. They are research inputs, not vendored source or runtime dependencies.

| Project | Checkout | Relevant finding |
| --- | --- | --- |
| Pingora | `e6e677f` | Strong HTTP/1 and HTTP/2 reverse-proxy, service, TLS, load-balancing, lifecycle, and observability primitives; no HTTP/3 or complete explicit forward proxy. |
| Squid | `6f4c814` | Broad forward-proxy product with CONNECT, ACLs, auth helpers, persistent caching, adaptation, interception, peer protocols, and management features. Parity is a multi-year program. |
| nginx | `eaac3d7` | Contextual directive registry, module-specific inheritance, HTTP virtual hosts, and stream TCP/UDP. Routing precedence cannot be reduced to source order. |
| HAProxy | `ca686e3` | Ordered section and ACL model with mature HTTP/TCP load balancing and reload. It is not a generic UDP load balancer. |
| Apache httpd | `62147ea` | Module-owned directives, virtual-host and directory merges, HTTP proxy modules, and ordered `ProxyPass`; no stock generic TCP/UDP proxy. |
| iptables | `84faa6b` | Configures kernel packet filtering and NAT. A socket proxy cannot reproduce FORWARD-chain filtering, DNAT/SNAT, conntrack, TPROXY, or source preservation. |
| Lua | `84938a7` | Suitable as an embedded data DSL only when the host controls libraries, environment, chunk mode, resources, and result types. |
| Vue | `b5f8518` | Vue 3 SFC tooling supports Pug through a build-time template preprocessor. Filesystem synchronization belongs in the backend, not Vue watchers. |
| Pug | `c323ed3` | Must be precompiled. Runtime compilation or user-supplied templates execute JavaScript and can resolve includes. Use a supported 3.x package rather than the checkout's stale manifest version. |

## Architectural conclusions

### Pingora is a foundation, not the finished proxy

Pingora supplies process lifecycle, listeners, HTTP/1 and HTTP/2, upstream pools,
reverse-proxy filters, load-balancing primitives, graceful shutdown, and cache interfaces.
Its `allow_connect_method_proxying` option permits CONNECT in the normal HTTP flow but
does not create a byte tunnel. A production explicit proxy needs a dedicated parser and
stream-takeover path that preserves bytes read beyond the CONNECT headers.

Pingora currently has no HTTP/3 implementation. HTTP/3 therefore requires a separately
tested QUIC/H3 frontend or an upstream Pingora capability; it must not be represented as
an option that silently falls back to HTTP/2.

Pingora's included memory cache is not a production persistent Squid-style store.
Persistent cache indexing, eviction, recovery, collapsed forwarding, purge, and disk
coordination are a separate subsystem.

### Drop-in import is semantic translation

nginx, HAProxy, Apache httpd, and Squid do not share one grammar or routing model.
Directive meanings also depend on compiled or loaded modules. Each importer needs to:

1. Parse the native syntax and includes with source locations.
2. Resolve product-specific contexts, inheritance, references, and order.
3. Convert only exactly representable behavior into a canonical typed model.
4. Emit blocking diagnostics for unsupported or ambiguous behavior.
5. Preserve the source provenance of every imported object.

The first importer subset should cover static listeners, exact hosts, exact/prefix paths,
static upstreams, equal-weight round robin, basic TLS references, nginx stream TCP/UDP,
and HAProxy TCP. Scripting, rewrites, arbitrary ACL expressions, module extensions, and
dynamic runtime state should initially fail with actionable diagnostics.

### User-space forwarding is not firewalling

OxiRoute can accept TCP or UDP traffic addressed to its own listeners and relay it using
new upstream sockets. It cannot transparently intercept transit packets, preserve client
source addresses, perform DNAT/SNAT/MASQUERADE, provide stateful packet filtering, or
replace policy routing and conntrack without explicit kernel integration.

UDP also needs its own bounded pseudo-session table, reply mapping, idle expiry, and
backpressure policy. It is not implemented by reusing the TCP relay loop.

### Configuration and UI synchronization

The canonical configuration should remain one file for the first release. A UI save must
validate a complete candidate, write and sync a same-directory temporary file, atomically
rename it, sync the directory, prepare a runtime generation, and only then activate it.
The API should expose separate disk and active revisions because a filesystem rename and
live socket activation cannot be one transaction.

The Vue UI should use Pug only in precompiled `<template lang="pug">` SFCs. A minimal
control plane needs `GET /api/config`, revision-checked `PUT /api/config`, and an SSE event
stream. External file replacement must update a clean editor or mark a dirty draft stale;
it must never silently overwrite either side.

## Licensing boundary

Pingora and Apache httpd use Apache-2.0; nginx uses a permissive BSD-style license; Lua,
Vue, and Pug use MIT-style licenses. Squid, HAProxy, and iptables are GPL projects.
OxiRoute can independently implement compatible behavior, but GPL implementation code
must not be copied into this Apache-2.0 project.
