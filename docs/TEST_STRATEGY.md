# Test strategy

## Method

Every behavior change starts with the smallest failing test at the owning abstraction.
Implementation follows only after the failure demonstrates the missing behavior. Each
logical step is formatted, linted, tested, and committed independently.

The initial repository followed this sequence:

1. Four Lua configuration acceptance tests compiled and failed at the loader stub.
2. The restricted loader and validation made those tests pass.
3. Runtime planning compiled and failed at its stub.
4. Protocol-specific Pingora service planning made that test pass.

## Test layers

### Unit and property tests

- Config type decoding, validation, canonical rendering, revisions, and diagnostics.
- Route precedence, URI transforms, ACL decisions, pool selection, retries, and limits.
- ACME order/renewal state machines using fake HTTP, DNS, clock, random, and storage seams.
- UDP pseudo-session keying/expiry and TCP timeout/half-close state.
- Import parser tokens, include graphs, inheritance, and semantic conversion.

Use property tests for parser round trips, deterministic rendering, revision behavior,
renewal windows, routing precedence, and bounded table/session behavior.

### Integration tests

- Loopback-only upstreams and ephemeral ports; no root or Internet dependency.
- HTTP methods, bodies, trailers, hop-by-hop fields, upgrades, gRPC, and negotiated versions.
- TCP full duplex, half-close, slow readers, backpressure, cancellation, and reload drain.
- TCP connect/idle/lifetime deadlines and partial traffic accounting across failure paths.
- UDP request/reply mapping, expiry, duplicate clients, upstream changes, and table pressure.
- TLS SNI/ALPN, client auth, upstream verification, certificate rotation, and expiry.
- Atomic config writes, parent-directory watches, stale revisions, and failed preparation.
- Management API response shapes, conflicts, redaction, authentication, and event reconnect.
- Monitoring counter lifecycle, Linux process/host parser fixtures, response shape, stale refresh,
  and non-overlapping polling.

### Import conformance

Each upstream format has versioned fixtures under:

```text
fixtures/<product>/{valid,invalid,unsupported,edge}/
```

Fixtures include source files, capability manifest, expected include graph, expected
canonical model, expected diagnostics, and optional native validator output. Route behavior
is tested against requests, not only snapshot text.

### Protocol conformance and interoperability

- HTTP semantics use standards-derived tests and independent clients/servers.
- HTTP/2 and HTTP/3 versions are asserted from negotiation and wire behavior.
- Forward proxy tests include CONNECT payload coalesced with headers, policy after DNS
  resolution, credential stripping, and destination-change attempts.
- TLS tests use independent OpenSSL/rustls clients where applicable.
- PROXY protocol and gRPC/WebSocket behavior use independent implementations.

### Fuzzing

Fuzz targets cover Lua value decoding limits, native config parsers, HTTP/1 forward-proxy
target parsing, CONNECT over-read handling, TLS ClientHello inspection, PROXY protocol, and
UDP pseudo-session input. Fuzzers have allocation and execution bounds.

### UI end-to-end

- Vue component tests cover validation, clean refresh, dirty draft, and redaction.
- Browser tests save config, observe canonical file changes, process external replacement,
  handle conflicts, and follow certificate jobs.
- Desktop and mobile viewport tests cover core workflows and keyboard navigation.

## Failure injection

Storage and runtime interfaces expose deterministic test-only failure points before write,
sync, rename, prepare, activate, and cleanup. Tests prove that observers see either the old
or new complete state and that active traffic never moves to a partially prepared state.

## Release gates

Every merge/release runs:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo +1.84 test --workspace --locked
pnpm --dir ui test
pnpm --dir ui build
```

As components land, gates add frontend typecheck/unit/build, browser tests, importer fixture
tests, local ACME integration, fuzz smoke corpus, dependency/license audit, and supported
platform builds.

A capability cannot move to `supported` in a public matrix while its failure-path,
reload/rotation, observability, and interoperability tests are missing.
