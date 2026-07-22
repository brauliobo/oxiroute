# Configuration specification

## Principles

The canonical file is Lua syntax used as a constrained data language. It returns exactly
one table. It is not a plugin system and cannot call operating-system or network APIs.

Rust owns the schema. The loader evaluates a fresh state, converts the returned value into
strict Rust types, validates cross-references, and destroys the state. Runtime components
receive an immutable compiled snapshot.

## Current schema

The daemon currently accepts the following canonical object families:

```lua
return {
  version = 1,
  management = {
    bind = "127.0.0.1:9080",
    ui_dir = "./ui/dist",
  },
  listeners = {
    {
      name = "web",
      bind = "127.0.0.1:8080",
      protocol = "http", -- http | tcp | rtmp
      service = "web",
      max_connections = 10000,
    },
    {
      name = "postgres",
      bind = "127.0.0.1:15432",
      protocol = "tcp",
      service = "postgres",
    },
    {
      name = "live",
      bind = "127.0.0.1:1935",
      protocol = "rtmp",
    },
  },
  upstream_pools = {
    {
      name = "web",
      endpoints = { "127.0.0.1:3000", "127.0.0.1:3001" },
      algorithm = "round_robin",
      health_check = {
        type = "http",
        host = "api.example.com",
        path = "/healthz",
        interval_ms = 5000,
        timeout_ms = 1000,
        healthy_threshold = 1,
        unhealthy_threshold = 3,
      },
    },
    {
      name = "postgres",
      endpoints = { "127.0.0.1:5432" },
    },
  },
  http_services = {
    {
      name = "web",
      upstream_io_timeout_ms = 30000,
      max_request_body_bytes = 10485760,
      max_retries = 1,
      routes = {
        {
          host = "api.example.com",
          path_prefix = "/v1",
          methods = { "GET", "POST" },
          upstream_pool = "web",
        },
        { path_prefix = "/", upstream_pool = "web" },
      },
    },
  },
  l4_services = {
    {
      name = "postgres",
      upstream_pool = "postgres",
      connect_timeout_ms = 10000,
      idle_timeout_ms = 300000,
      lifetime_timeout_ms = 3600000,
    },
  },
}
```

Current constraints:

- `version` MUST be `1`.
- `management` is optional and MUST use a loopback IP with a nonzero port until remote authentication exists.
- `management.ui_dir` optionally points to a prebuilt Vue distribution loaded into memory at daemon startup.
- Names MUST be unique within their listener, pool, HTTP-service, or L4-service namespace.
- Listener bind addresses MUST be unique.
- Names MUST contain non-whitespace text without surrounding whitespace or control characters.
- Bind addresses MUST be IP socket literals with nonzero ports. Exact duplicates and wildcard binds
  that overlap another listener or management bind are rejected.
- HTTP and TCP listeners MUST reference an existing same-kind service. RTMP listeners terminate
  locally and MUST NOT reference a service.
- `max_connections` defaults to `10000`, MUST be nonzero, and MUST be no greater than
  `9007199254740991` so the monitoring API and UI preserve it exactly. Excess accepted connections
  are closed before entering the protocol handler.
- Pools MUST contain between 1 and 256 unique IP socket endpoints with nonzero ports, and one
  configuration MUST contain at most 1024 pool endpoints in total. The only current algorithm is
  `round_robin`, which is also the default. A pool cannot target the loopback management endpoint
  because that would bypass its exposure boundary.
- HTTP services MUST contain at least one route. Route pool references MUST resolve.
- A missing route `host` matches any authority. A host may be an exact DNS name/IP literal or a
  single-label wildcard such as `*.example.com`; names are normalized to lowercase.
- `path_prefix` defaults to `/`, matches only complete path segments, and has trailing slashes
  normalized away except for `/`. A missing or empty `methods` list matches every method;
  configured methods MUST be uppercase HTTP tokens.
- Route precedence is exact host, wildcard host, then host catch-all; within a host class the
  longest path prefix wins, and source order resolves any remaining tie. No match returns `404`.
- Routes with identical normalized host, path, and method matchers are rejected. Source order only
  resolves ties between distinct overlapping matchers.
- Requests with duplicate/conflicting authorities, userinfo authorities, dot path segments,
  repeated separators, backslashes, malformed escapes, encoded unreserved characters, or encoded
  path separators are rejected with `400` before upstream selection. Route prefixes use the same
  path policy so configured routes remain reachable. Accepted percent-triplet hex digits are
  canonicalized to uppercase for matching.
- `upstream_io_timeout_ms` defaults to `30000` and applies independently to upstream connect,
  read-inactivity, and write-inactivity operations; progress resets the I/O deadline, so this is
  not a total request deadline. `max_request_body_bytes` defaults to `10485760`; both values MUST
  be nonzero. Oversized declared bodies return `413` before contacting an origin. A streamed
  overflow aborts forwarding and returns `413` when an origin response has not already committed.
- `max_retries` is the number of additional connection attempts after the first, defaults to `0`,
  and MUST be at most `2`. Retries are permitted only for bodyless `GET` and `HEAD` requests that
  are not protocol upgrades, only after a transient connection-establishment failure, and only
  when a distinct configured endpoint remains. Established-connection errors, response statuses,
  body-bearing requests, unsafe methods, and upgrades are never retried. Each attempt has its own
  `upstream_io_timeout_ms` connect deadline; there is no total request deadline.
- L4 services reference a pool. Connect and idle timeouts default to `10000` and `300000`
  milliseconds; an optional lifetime timeout has no default. Configured timeout values MUST be
  nonzero.
- Unknown fields and unknown protocol values are errors.
- Source is limited to 1 MiB, extra Lua memory to 4 MiB, and execution to one million instructions.
- No Lua standard libraries are loaded and binary chunks are rejected.

### Active pool health checks

`health_check` is optional on every upstream pool. Omitting it leaves the pool's endpoints in the
selectable `unchecked` state. When present, it has this strict schema:

| Field | Required | Default | Constraint |
| --- | --- | --- | --- |
| `type` | yes | none | `tcp` or `http` |
| `interval_ms` | no | `10000` | `1000` through `86400000` inclusive |
| `timeout_ms` | no | `1000` | `1` through `30000` inclusive and less than `interval_ms` |
| `healthy_threshold` | no | `1` | `1` through `100` inclusive |
| `unhealthy_threshold` | no | `3` | `1` through `100` inclusive |
| `host` | HTTP only | none | Required HTTP authority, at most 255 bytes, without userinfo; any port MUST be numeric |
| `path` | HTTP only | none | Required query-free absolute path, at most 2048 bytes, accepted by the request-path ambiguity policy |

TCP checks accept neither `host` nor `path` and succeed when a connection to the endpoint is
established. HTTP checks send a plaintext HTTP/1.1 `GET` to the endpoint using the configured path
and exact `Host` header; only status `200` succeeds. `timeout_ms` bounds the complete probe.

Health-enabled endpoints start `unknown` and are not selectable. The healthy threshold must be met
by consecutive successes before an unknown or unhealthy endpoint becomes `healthy`; the unhealthy
threshold must be met by consecutive failures before an unknown or healthy endpoint becomes
`unhealthy`.
Success resets the failure streak and failure detail, while failure resets the success streak.
Round robin skips unknown and unhealthy endpoints, and a matched HTTP route whose pool has no
selectable endpoint returns `503`.

Each endpoint runs its first probe immediately, then waits its pool's `interval_ms` after that probe
completes. A slow endpoint does not shift another endpoint's schedule, even within the same pool.
All pools share a limit of 32 concurrent probes, and an endpoint never has overlapping probes.

This schema is pre-release and may change without compatibility code until a public release
persists it.

## Future canonical model

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
  rtmp_services[]
  imports[]
  management
  observability
```

Required identities are stable names, not array indexes. References are resolved during
validation. Every compiled object records canonical source location and optional imported
source provenance.

The current schema above is the implemented subset. It will grow only as a milestone needs each
additional field; the items below are targets, not accepted fields unless listed in the current
schema.

### Listener

- Name and one or more bind addresses.
- Transport: `tcp` or `udp`.
- Application mode: `http`, `forward_proxy`, or `raw`.
- Optional TLS profile and default service.
- Socket limits and optional PROXY protocol policy.

### HTTP service and route

- Exact, wildcard, or future regex host matchers where supported.
- Prefix and future exact or regex path matchers.
- Methods and policy references.
- Explicit precedence and source order.
- Proxy, redirect, static response, or reject action.
- Explicit URI transformation rather than implicit trailing-slash behavior.

### Upstream pool

- Protocol and endpoint list.
- Endpoint weight, additional algorithms, passive health policy, TLS, SNI, and verification.
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

### RTMP service

RTMP services contain listeners, applications, live/relay/record/VOD/segment policies,
callbacks, controls, and limits. The canonical model uses typed fields; the nginx-rtmp
importer additionally retains every raw directive token and its effective scope as defined
in `RTMP_SPEC.md`.

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
