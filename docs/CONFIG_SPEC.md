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
  certificates = {
    {
      name = "public-example",
      dns_names = { "api.example.com" },
      source = {
        type = "files",
        certificate_chain_path = "/etc/oxiroute/api-fullchain.pem",
        private_key_path = "/etc/oxiroute/api-key.pem",
      },
    },
  },
  tls_profiles = {
    {
      name = "public-tls",
      certificates = { "public-example" },
      default_certificate = "public-example",
      min_version = "1.2",
      alpn = { "h2", "http/1.1" },
    },
  },
  listeners = {
    {
      name = "web",
      bind = { type = "socket", address = "127.0.0.1:8443" },
      protocol = "http", -- http | tcp | rtmp
      service = "web",
      tls_profile = "public-tls",
      max_connections = 10000,
    },
    {
      name = "postgres",
      bind = { type = "unix", path = "/run/oxiroute/postgres.sock" },
      protocol = "tcp",
      service = "postgres",
      max_connections = null,
    },
    {
      name = "live",
      bind = { type = "socket", address = "127.0.0.1:1935" },
      protocol = "rtmp",
      service = "live",
    },
  },
  upstream_pools = {
    {
      name = "web",
      endpoints = {
        { type = "socket", address = "127.0.0.1:3000" },
        { type = "dns", host = "backend.example.com", port = 3001 },
      },
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
      endpoints = {
        { type = "unix", path = "/run/postgresql/.s.PGSQL.5432" },
      },
    },
    {
      name = "secure-api",
      endpoints = {
        { type = "dns", host = "origin.example.com", port = 443 },
      },
      tls = {
        server_name = "origin.example.com",
        ca_certificate_path = "/etc/oxiroute/origin-ca.pem",
      },
      http_versions = { min = "1.1", max = "2" },
    },
  },
  http_services = {
    {
      name = "web",
      upstream_io_timeout_ms = 30000,
      max_request_body_bytes = 10485760,
      routes = {
        {
          host = { kind = "normalized_host", value = "api.example.com" },
          path = { kind = "segment_prefix", value = "/v1" },
          methods = { "GET", "POST" },
          action = {
            type = "proxy",
            upstream_pool = "web",
            policy = {},
          },
        },
        {
          path = { kind = "segment_prefix", value = "/secure" },
          methods = {},
          action = {
            type = "proxy",
            upstream_pool = "secure-api",
            policy = {},
          },
        },
        {
          path = { kind = "segment_prefix", value = "/" },
          methods = {},
          action = {
            type = "proxy",
            upstream_pool = "web",
            policy = {},
          },
        },
      },
    },
  },
  rtmp_services = {
    {
      name = "live",
      applications = {
        {
          name = "live",
          live = true,
          idle_streams = true,
          recorders = {
            {
              name = "archive",
              start = "continuous",
              root_directory = "/var/lib/oxiroute/recordings",
              suffix_template = "-%Y-%m-%dT%H-%M-%S.flv",
              append_unix_seconds = false,
              rotation_interval_ms = 3600000,
              max_queue_messages = 256,
              max_queue_bytes = 8388608,
              shutdown_timeout_ms = 5000,
              max_storage_bytes = 10737418240,
              max_storage_files = 10000,
              max_active_recorders = 8,
            },
          },
        },
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
- `management` is optional and MUST use a loopback IP with a nonzero port. The current
  configuration routes use bearer authentication, but the schema does not expose a remote
  management mode.
- `management.ui_dir` optionally points to a prebuilt Vue distribution loaded into memory at daemon startup.
- Names MUST be unique within their certificate, TLS-profile, listener, pool, HTTP-service,
  RTMP-service, or L4-service namespace.
- Listener binds MUST be unique after normalization.
- Names MUST contain non-whitespace text without surrounding whitespace or control characters.
- A listener `bind` is exactly one tagged object: `{ type = "socket", address = "IP:port" }` or
  `{ type = "unix", path = "/absolute/socket/path" }`. Socket ports MUST be nonzero. Exact socket
  duplicates, wildcard socket binds that overlap another listener or management bind, and duplicate
  normalized Unix paths are rejected.
- Unix paths MUST be valid UTF-8 absolute paths of at most 107 bytes. Repeated `/` separators are
  collapsed; a root-only path, trailing `/`, NUL, and `.` or `..` segments are rejected. Unix
  listeners and upstreams can start only on Unix platforms. A Unix listener cannot use TLS.
- HTTP, RTMP, and TCP listeners MUST reference an existing same-kind service. An RTMP service MUST
  contain between 1 and 256 unique applications, and one configuration accepts at most 64 RTMP
  services.
- `tls_profile` is optional and accepted only on HTTP listeners. Its named TLS profile and that
  profile's named certificate MUST exist. TCP and RTMP listeners reject `tls_profile` rather than
  implicitly changing protocol behavior.
- `max_connections` omitted or set to `null` means unbounded admission. A configured limit MUST be
  positive and no greater than `9007199254740991` so the monitoring API and UI preserve it exactly.
  Excess accepted connections are closed immediately after TCP accept, before TLS handshakes or
  protocol handling, and one admission remains charged for the complete transport connection
  lifetime. Deterministic canonical rendering writes the unbounded value as explicit `null`.
- Pools MUST contain between 1 and 256 unique tagged endpoints, and one configuration MUST contain
  at most 1024 pool endpoints in total. An endpoint is exactly one of
  `{ type = "socket", address = "IP:port" }`,
  `{ type = "dns", host = "origin.example.com", port = 443 }`, or
  `{ type = "unix", path = "/absolute/socket/path" }`. Socket and DNS ports MUST be nonzero.
- DNS endpoint hosts are normalized to ASCII lowercase and MUST be DNS names of at most 253 bytes
  with nonempty labels of at most 63 bytes. IP literals, wildcards, trailing dots, non-ASCII bytes,
  and labels with non-alphanumeric edge characters are rejected. Validation does not resolve or
  expand a DNS endpoint. Its normalized host and port remain the stable selection, retry,
  monitoring, and topology identity; lookup occurs while establishing each connection or probe.
- Endpoints MUST be unique after socket, DNS-name, and Unix-path normalization. A socket endpoint
  cannot directly target the loopback management endpoint because that would bypass its exposure
  boundary. Pools containing any Unix endpoint cannot enable upstream TLS or active health checks.
- `algorithm` accepts `round_robin` (the default) or `least_connections`. Both skip unavailable and
  request-excluded endpoints. Least-connections selects the eligible endpoint with the fewest
  active leases and rotates deterministic ties from the pool cursor. A lease is acquired before
  connection preparation and released when the HTTP request or L4 relay attempt finishes, including
  failure paths.
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
  not a total request deadline. Omitted `max_request_body_bytes` defaults to `10485760`; explicit
  `null` means unbounded streaming, and a configured limit MUST be positive. Oversized declared
  bodies return `413` before contacting an origin. A streamed overflow aborts forwarding and
  returns `413` when an origin response has not already committed. Canonical rendering preserves
  the unbounded policy as explicit `null`.
- `max_retries` is the number of additional connection attempts after the first, defaults to `0`,
  and MUST be at most `2`. Retries are permitted only for bodyless `GET` and `HEAD` requests that
  are not protocol upgrades, only after a transient connection-establishment failure, and only
  when a distinct canonical endpoint identity remains. DNS address expansion does not create extra
  retry identities. Established-connection errors, response statuses,
  body-bearing requests, unsafe methods, and upgrades are never retried. Each attempt has its own
  `upstream_io_timeout_ms` connect deadline; there is no total request deadline.
- L4 services reference a pool. Connect and idle timeouts default to `10000` and `300000`
  milliseconds; an optional lifetime timeout has no default. Configured timeout values MUST be
  nonzero. An L4 service MUST NOT reference a TLS-enabled upstream pool; opaque TLS pass-through
  uses an ordinary plaintext-configured TCP pool and does not terminate or originate TLS.
- Unknown fields and unknown protocol values are errors.
- Source is limited to 1 MiB, extra Lua memory to 4 MiB, and execution to one million instructions.
- No Lua standard libraries are loaded and binary chunks are rejected.

### Downstream certificates and TLS profiles

`certificates` and `tls_profiles` default to empty collections. A certificate source is either a
direct file pair or an operator-owned Certbot live/archive lineage:

```lua
certificates = {
  {
    name = "public-example",
    dns_names = { "example.com", "*.example.com" },
    source = {
      type = "files",
      certificate_chain_path = "/etc/oxiroute/public-fullchain.pem",
      private_key_path = "/etc/oxiroute/public-key.pem",
    },
  },
  {
    name = "certbot-example",
    dns_names = { "certbot.example.com" },
    source = {
      type = "certbot",
      live_directory_path = "/etc/letsencrypt/live/certbot.example.com",
      archive_directory_path = "/etc/letsencrypt/archive/certbot.example.com",
    },
  },
}

tls_profiles = {
  {
    name = "public-tls",
    certificates = { "public-example" },
    default_certificate = "public-example",
    min_version = "1.2",
    alpn = { "h2", "http/1.1" },
  },
}

listeners = {
  {
    name = "https",
    bind = { type = "socket", address = "0.0.0.0:443" },
    protocol = "http",
    service = "web",
    tls_profile = "public-tls",
  },
}
```

The current certificate and profile rules are:

- One configuration accepts at most 256 certificates and 256 TLS profiles. Every certificate has
  1 through 100 declared `dns_names`; names are lowercased, and duplicates after normalization are
  rejected. They MUST be ASCII DNS names of at most 253 bytes with labels of at most 63 bytes. IP
  literals, trailing dots, and wildcards other than one leading `*.` label are rejected.
- Direct-file certificate and key paths MUST be distinct, valid UTF-8 absolute paths of at most
  4096 bytes. Certbot live and archive directory paths follow the same lexical rules and MUST be
  distinct. NUL, repeated `/`, a trailing `/`, and `.` or `..` segments are rejected lexically.
- Runtime preparation requires regular, nonempty files and reads each file twice with identical
  content. The certificate chain is limited to 1 MiB and 16 certificates total; it MUST contain the
  leaf first and at least one ordered issuer. The private key is limited to 256 KiB and, on Unix,
  MUST have exactly mode `0400`, `0600`, `0440`, or `0640`.
- The chain file may contain only `CERTIFICATE` PEM blocks. The key file MUST contain exactly one
  unencrypted `PRIVATE KEY`, `RSA PRIVATE KEY`, or `EC PRIVATE KEY` block. Parsed keys MUST be RSA
  with at least 2048 bits or EC with at least 256 bits. When the leaf contains `KeyUsage`, it MUST
  permit `digitalSignature`. The leaf/key match, current validity of every chain entry, adjacent
  issuer/signature order, CA-capable issuers, SSL-server purpose, and OpenSSL acceptor loading are
  checked before startup. This validates a self-consistent server chain but does not establish
  public trust.
- Certbot preparation requires `cert.pem`, `chain.pem`, `fullchain.pem`, and `privkey.pem` live
  symlinks to one positive numbered archive revision. `certN.pem` contains exactly one certificate,
  `chainN.pem` contains one or more certificate blocks, and `fullchainN.pem` is byte-for-byte
  `certN.pem + chainN.pem`. Archive entries are read through a pinned directory descriptor with
  no-follow semantics. A private-key archive symlink may reuse another numbered `privkeyN.pem` only
  within the same archive. Mixed revisions, escapes, unstable reads, and insecure key modes fail
  startup without publishing material.
- The leaf MUST contain DNS SANs; the common name is not a fallback. After lowercase normalization,
  the complete DNS SAN set MUST exactly equal the declared `dns_names` set.
- Every TLS profile MUST reference a nonempty list of unique certificates and name one listed
  `default_certificate`. Two certificates in the same profile MUST NOT claim the same normalized
  DNS SAN. During a handshake, an exact SNI match wins over a one-label wildcard match; unknown,
  non-DNS, and absent SNI select the explicit default certificate.
- `min_version` accepts only `"1.2"` (the default) or `"1.3"`. `alpn` defaults to
  `{ "http/1.1" }`; the only accepted policies are `{ "http/1.1" }`, `{ "h2" }`, and
  `{ "h2", "http/1.1" }` in that order.
- Downstream TLS session caching and tickets are disabled. Every new handshake selects one identity
  and takes one immutable certificate-generation snapshot through the TLS callback, so an atomic
  generation publication cannot mix key and chain material and existing connections retain their
  selected generation. Publication is independent per identity and requires the same certificate
  identity and exact declared DNS-name set.
  Direct-file identities remain startup snapshots. Configured Certbot identities have a
  process-lifetime watcher that validates and atomically publishes complete replacement
  generations. No canonical-config watcher, direct-file watcher, self-signed generator, managed
  ACME job, or certificate-management API currently publishes replacements.

Downstream HTTP/2 is available only on a TLS listener whose ALPN policy includes `h2`. Plaintext
HTTP listeners are HTTP/1.1-only; h2c is not supported. gRPC has no separate configuration object:
it is proxied over a compatible downstream and upstream H2 path. An H2-only listener rejects an
incompatible ALPN offer during negotiation. A client that omits ALPN can complete TLS, but OxiRoute
closes the stream before HTTP parsing instead of allowing Pingora's HTTP/1.1 fallback.

### Upstream TLS and HTTP versions

An HTTP upstream pool enables verified TLS by adding `tls`. `ca_certificate_path` is optional; when
omitted, Pingora uses its default trust roots.

```lua
upstream_pools = {
  {
    name = "secure-origin",
    endpoints = {
      { type = "dns", host = "origin.example.com", port = 443 },
    },
    tls = {
      server_name = "origin.example.com",
      ca_certificate_path = "/etc/oxiroute/origin-ca.pem",
    },
    http_versions = { min = "1.1", max = "2" },
  },
}
```

- `server_name` is required, lowercased, sent as SNI, and used for strict certificate hostname
  verification. It MUST be an ASCII DNS name of at most 253 bytes; IP literals, wildcards,
  trailing dots, empty labels, overlong labels, and labels with invalid edge characters are
  rejected.
- The custom CA path follows the same 4096-byte lexical and stable regular-file rules as other TLS
  paths. Its PEM is limited to 1 MiB and 128 unique `CERTIFICATE` blocks. Every anchor MUST be
  currently valid and CA-capable; malformed, duplicate, expired, future, and non-CA entries fail
  runtime preparation. Self-signed roots and intermediate CA certificates are accepted as explicit
  trust anchors under strict partial-chain verification. Trust, SNI, hostname, CA content, and
  HTTP-version policy are included in upstream connection-reuse isolation.
- `http_versions` defaults to `{ min = "1.1", max = "1.1" }`. The accepted ranges are `1.1/1.1`,
  `1.1/2`, and `2/2`. Flexible `1.1/2` permits ALPN fallback to HTTP/1.1; `2/2` requires H2 and
  rejects a downgrade before HTTP headers. Any range with `max = "2"` requires `tls`; upstream
  h2c is not supported.
- Upstream TLS always verifies both the certificate chain and hostname. A pinned, documented
  Pingora connector hook applies security level 2, a TLS 1.2 minimum, ECDHE+AEAD TLS 1.2 ciphers,
  and standard TLS 1.3 AEAD suites before handshake. The negotiated digest is checked again before
  request headers as defense in depth.
- `health_check` and `tls` are mutually exclusive on one pool because current active checks are
  plaintext TCP or HTTP/1.1. TLS-enabled pools are valid only for HTTP services and are rejected by
  L4 services. Any pool containing a Unix endpoint rejects both `health_check` and `tls`.

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
and exact `Host` header; only status `200` succeeds. `timeout_ms` bounds the complete probe. DNS
endpoints are resolved for each probe and each returned address may be attempted; Unix endpoints
cannot be health checked.

Health-enabled endpoints start `unknown` and are not selectable. The healthy threshold must be met
by consecutive successes before an unknown or unhealthy endpoint becomes `healthy`; the unhealthy
threshold must be met by consecutive failures before an unknown or healthy endpoint becomes
`unhealthy`.
Success resets the failure streak and failure detail, while failure resets the success streak.
Round robin and least-connections selection skip unknown and unhealthy endpoints, and a matched
HTTP route whose pool has no selectable endpoint returns `503`.

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
- Endpoint weight, additional algorithms beyond round robin and least connections, passive health
  policy, TLS, SNI, and verification.
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
    certificates = { "public-example" },
    default_certificate = "public-example",
    min_version = "1.2",
    alpn = { "h2", "http/1.1" },
  },
}
```

Secrets SHOULD be references to protected files, environment-independent secret stores,
or plugin credentials. Canonical output MUST NOT inline private keys or DNS API tokens.

### RTMP service and recorder

The current RTMP model supports live applications and canonical recorder policies. Relay, VOD,
segment, callback, access, and logging policies remain future fields. The strict nginx-RTMP
importer retains source tokens, effective inheritance, terminal decisions, and provenance for its
supported subset as defined in `RTMP_SPEC.md`.

Live mode requires an explicit listener-to-service reference and application policy. A recorder is
valid only on an application with `live = true`:

```lua
listeners = {
  {
    name = "live",
    bind = { type = "socket", address = "127.0.0.1:1935" },
    protocol = "rtmp",
    service = "live",
  },
}

rtmp_services = {
  {
    name = "live",
    applications = {
      {
        name = "live",
        live = true,
        idle_streams = true,
        recorders = {
          {
            name = "archive",
            start = "manual",
            root_directory = "/var/lib/oxiroute/recordings",
          },
        },
      },
    },
  },
}
```

Application names match exactly. Stream query arguments are protocol data and do not change the
normalized stream identity; recording filenames discard query arguments. `idle_streams = true`
permits a viewer to wait before a publisher. One application accepts at most 8 uniquely named
recorders, one configuration accepts at most 256 recorders and 64 normalized recording roots, and
recorders require `live = true`.

Recorder fields are:

| Field | Required | Default | Constraint |
| --- | --- | --- | --- |
| `name` | yes | none | Unique nonblank canonical name within the application. |
| `start` | no | `"continuous"` | `"continuous"` or `"manual"`. |
| `root_directory` | yes | none | Normalized absolute UTF-8 directory path, at most 4096 bytes. |
| `suffix_template` | no | `".flv"` | At most 128 bytes; only UTC `%Y`, `%m`, `%d`, `%H`, `%M`, `%S`, and `%%`; no NUL or path separator. |
| `append_unix_seconds` | no | `false` | Appends `-<open Unix seconds>` before the suffix. |
| `rotation_interval_ms` | no | `null` | `null` or `1` through `2147483647`. |
| `max_queue_messages` | no | `256` | `1` through `65536`. |
| `max_queue_bytes` | no | `8388608` | `1` through `1073741824`, and no greater than `max_storage_bytes`. |
| `shutdown_timeout_ms` | no | `5000` | `1` through `60000`. |
| `max_storage_bytes` | no | `10737418240` | `1` through `1099511627776`. |
| `max_storage_files` | no | `10000` | `1` through `1000000`. |
| `max_active_recorders` | no | `8` | `1` through `256`. |

Repeated `/` separators in `root_directory` are collapsed. `/`, a trailing `/`, NUL, `.` or `..`
segments, relative paths, and non-UTF-8 paths are rejected. Recorders sharing one normalized root
MUST configure identical storage-byte, storage-file, and active-recorder limits.

Runtime planning opens the existing root one component at a time without following symlinks and
performs a read-only ownership/quota preflight. The root MUST be owned by the daemon user and MUST
NOT be writable by group or other users. Candidate config validation neither creates the root nor
creates a lock, probe, partial, or recording file. Actual RTMP service activation opens and pins the
root, validates or creates the mode-`0600` single-link ownership lock, may clean exact abandoned
partials only under exclusive ownership, and can still fail if the root changed after preflight.
Errors identify the service/application/recorder but redact the root path.

Existing regular files count against root quotas. Stores for the same directory identity share
byte, file, and active-recorder counters within one daemon process. The ownership protocol protects
partial cleanup across processes, but quota counters are not distributed: multiple daemon
processes can collectively exceed the configured limits and require deployment-level isolation.

## Deterministic rendering

The backend, not the browser, renders typed JSON into canonical Lua.

- Rendering MUST be deterministic for the same typed model.
- A successful UI save normalizes formatting and field order.
- Arbitrary comments and executable Lua syntax are not guaranteed to round-trip.
- The API MUST state that normalization will occur before accepting a save.

## Revisions and activation

- `diskRevision` is the SHA-256 hash of the complete canonical file bytes.
- `activeRevision` identifies the compiled runtime generation.
- The implemented API requires one raw 64-hex `If-Config-Revision` header on writes.
- The backend MUST re-read and compare immediately before writing.
- A mismatch returns a conflict and does not write.
- Validation and writes prepare the complete candidate runtime, management UI assets, and Certbot
  watcher prerequisites before any disk mutation.
- Writes use a unique same-directory temporary file, complete write, permission setting,
  file sync, atomic rename, and parent-directory sync.
- A changed save returns `saved_restart_required`; an idempotent save of the active generation
  returns `unchanged_active`. Neither path activates a new generation in the running daemon.
- There is no canonical-config watcher. External changes are observed only on an API read or a
  later revision-checked write; invalid external edits make the persisted configuration
  unavailable while the startup generation continues serving traffic.

## Diagnostics

Diagnostics contain stable code, severity, stage, source range, include/import stack,
related ranges, explanation, and suggested resolution. Initial code families:

- `E_SYNTAX`, `E_UNKNOWN_FIELD`, `E_INVALID_VALUE`, `E_DUPLICATE_IDENTITY`
- `E_DUPLICATE_BIND`, `E_UNRESOLVED_REFERENCE`, `E_INCLUDE_CYCLE`
- `E_UNSUPPORTED_FEATURE`, `E_SEMANTICS_NOT_REPRESENTABLE`
- `E_CERTIFICATE_INVALID`, `E_SECRET_UNAVAILABLE`, `E_RUNTIME_PREPARE`
- `W_NATIVE_VALIDATION_REQUIRED`, `W_NONPORTABLE`, `W_ORDER_DEPENDENT`

Warnings never convert an unsafe or unrepresentable service into an active one.
