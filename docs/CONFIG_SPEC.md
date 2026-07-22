# Configuration specification

## Principles

The canonical file is Lua syntax used as a constrained data language. It returns exactly
one table. It is not a plugin system and cannot call operating-system or network APIs.

Rust owns the schema. The loader evaluates a fresh state, converts the returned value into
strict Rust types, validates cross-references, and destroys the state. Runtime components
receive an immutable compiled snapshot.

## Current schema

The executable skeleton currently accepts:

```lua
return {
  version = 1,
  listeners = {
    {
      name = "web",
      bind = "127.0.0.1:8080",
      protocol = "http", -- http | tcp
      upstream = "127.0.0.1:3000",
    },
  },
}
```

Current constraints:

- `version` MUST be `1`.
- Listener names and bind addresses MUST be unique.
- Names MUST contain non-whitespace text.
- Bind and upstream addresses MUST be IP socket literals with nonzero ports.
- Unknown fields and unknown protocol values are errors.
- Source is limited to 1 MiB, extra Lua memory to 4 MiB, and execution to one million instructions.
- No Lua standard libraries are loaded and binary chunks are rejected.

This schema is pre-release and may change without compatibility code until a public release
persists it.

## Target canonical model

The model will grow only as a milestone needs each field:

```text
Config
  version
  global
  listeners[]
  http_services[]
  l4_services[]
  upstream_pools[]
  policies[]
  tls_profiles[]
  certificates[]
  imports[]
  management
  observability
```

Required identities are stable names, not array indexes. References are resolved during
validation. Every compiled object records canonical source location and optional imported
source provenance.

### Listener

- Name and one or more bind addresses.
- Transport: `tcp` or `udp`.
- Application mode: `http`, `forward_proxy`, or `raw`.
- Optional TLS profile and default service.
- Socket limits and optional PROXY protocol policy.

### HTTP service and route

- Exact, wildcard, or regex host matchers where supported.
- Exact, prefix, or regex path matchers.
- Methods and policy references.
- Explicit precedence and source order.
- Proxy, redirect, static response, or reject action.
- Explicit URI transformation rather than implicit trailing-slash behavior.

### Upstream pool

- Protocol and endpoint list.
- Weight, algorithm, timeouts, health policy, TLS, SNI, and verification.
- HTTP minimum and maximum versions.
- Retry and circuit-breaker limits.

### L4 service

- TCP or UDP transport.
- Pool reference, affinity policy, timeouts, limits, and optional bounded inspection.

### Certificate and TLS profile

Certificates are reusable lifecycle objects. TLS profiles reference them and define
listener behavior.

```lua
certificates = {
  {
    name = "public-example",
    source = "acme", -- acme | files | self_signed
    domains = { "example.com", "www.example.com" },
    acme = {
      directory = "https://acme-v02.api.letsencrypt.org/directory",
      email = "ops@example.com",
      challenge = "http-01",
    },
  },
}

tls_profiles = {
  {
    name = "public-tls",
    certificate = "public-example",
    min_version = "1.2",
    alpn = { "h2", "http/1.1" },
  },
}
```

Secrets SHOULD be references to protected files, environment-independent secret stores,
or plugin credentials. Canonical output MUST NOT inline private keys or DNS API tokens.

## Deterministic rendering

The backend, not the browser, renders typed JSON into canonical Lua.

- Rendering MUST be deterministic for the same typed model.
- A successful UI save normalizes formatting and field order.
- Arbitrary comments and executable Lua syntax are not guaranteed to round-trip.
- The API MUST state that normalization will occur before accepting a save.

## Revisions and reload

- `diskRevision` is the SHA-256 hash of the complete canonical file bytes.
- `activeRevision` identifies the compiled runtime generation.
- API writes MUST include the expected disk revision.
- The backend MUST re-read and compare immediately before writing.
- A mismatch returns a conflict and does not write.
- Writes use a unique same-directory temporary file, complete write, permission setting,
  file sync, atomic rename, and parent-directory sync.
- The watcher observes the parent directory because atomic replacement changes the inode.
- Invalid external edits update disk diagnostics while the prior active revision remains live.

## Diagnostics

Diagnostics contain stable code, severity, stage, source range, include/import stack,
related ranges, explanation, and suggested resolution. Initial code families:

- `E_SYNTAX`, `E_UNKNOWN_FIELD`, `E_INVALID_VALUE`, `E_DUPLICATE_IDENTITY`
- `E_DUPLICATE_BIND`, `E_UNRESOLVED_REFERENCE`, `E_INCLUDE_CYCLE`
- `E_UNSUPPORTED_FEATURE`, `E_SEMANTICS_NOT_REPRESENTABLE`
- `E_CERTIFICATE_INVALID`, `E_SECRET_UNAVAILABLE`, `E_RUNTIME_PREPARE`
- `W_NATIVE_VALIDATION_REQUIRED`, `W_NONPORTABLE`, `W_ORDER_DEPENDENT`

Warnings never convert an unsafe or unrepresentable service into an active one.
