# Configuration specification

## Principles

The canonical model is a strict Rust-owned typed object. KDL 2.0 is the default file syntax and
deterministic revision format. Restricted Lua, HOCON, and OpenWrt UCI remain supported source and
rendering formats; syntax is selected by the path extension, with an extensionless path defaulting
to KDL. See [`CONFIG_FORMATS.md`](CONFIG_FORMATS.md) for their exact reversible mappings,
declarative templates, native references, bounds, and security limitations.

Every source resolves into the same bounded value tree and strict Rust types before defaults,
cross-reference validation, runtime preparation, and immutable generation compilation. Lua alone
evaluates code: it runs as a constrained data language in a fresh state with no standard libraries,
then the state is destroyed. KDL, HOCON, and UCI are declarative parsers and do not execute source.

Format status is explicit: KDL 2.0 is the current canonical default; restricted Lua is a supported
legacy/compatibility adapter; HOCON and UCI are supported declarative adapters; templates and native
server references are available only in the declarative pipeline. None of these adapters implies
that every modeled field is an active runtime capability: unsupported cache policy forms still fail
closed during runtime preparation rather than being silently ignored.

## Current Schema

The daemon currently accepts the following canonical object families. KDL is the recommended
authoring syntax; [`../oxiroute.example.kdl`](../oxiroute.example.kdl) is a complete deterministic
KDL example. The larger schema illustration below uses the still-supported restricted Lua notation
because its inline object syntax is compact; it describes the same typed model rather than a
Lua-only contract.

```lua
return {
  version = 1,
  max_connections = 4096,
  management = {
    bind = "127.0.0.1:9080",
    ui_dir = "./ui/dist",
  },
  stats = {
    binds = { "127.0.0.1:8404", "[::1]:8404" },
    admin_token_file = "/etc/oxiroute/stats-admin.token",
    pages = {
      {
        bind = "127.0.0.1:8405",
        uri_prefix = "/haproxy",
        refresh_ms = 10000,
        admin = "localhost",
      },
    },
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
  cache_stores = {
    { name = "forward-cache", type = "memory" },
  },
  listeners = {
    {
      name = "web",
      bind = { type = "socket", address = "127.0.0.1:8443" },
      protocol = "http", -- http | http3 | forward_http1 | forward_http2 | forward_http3 | tcp | udp | rtmp
      service = "web",
      tls_profile = "public-tls",
      max_connections = 10000,
    },
    {
      name = "forward",
      bind = { type = "socket", address = "127.0.0.1:3128" },
      protocol = "forward_http1",
      service = "forward",
      max_connections = 10000,
    },
    {
      name = "postgres",
      bind = { type = "unix", path = "/run/oxiroute/postgres.sock", mode = 432 },
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
      servers = {
        { name = "web-1", endpoint = { type = "socket", address = "127.0.0.1:3000" } },
        {
          name = "web-2",
          endpoint = { type = "dns", host = "backend.example.com", port = 3001 },
          dns_resolution = "startup",
        },
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
      servers = {
        {
          name = "postgres-1",
          endpoint = { type = "unix", path = "/run/postgresql/.s.PGSQL.5432" },
        },
      },
    },
    {
      name = "secure-api",
      servers = {
        {
          name = "secure-api-1",
          endpoint = { type = "dns", host = "origin.example.com", port = 443 },
        },
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
      automatic_response_headers = true,
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
  forward_proxy_services = {
    {
      name = "forward",
      enabled_versions = { "h1" },
      allow_absolute_form = true,
      tls_required = false,
      connect = { enabled = true, allowed_ports = { 443 } },
      connect_udp = { enabled = true, allowed_ports = { 443 } },
      peer_policy = {
        peers = {
          { host = "proxy.example.test", port = 3128 },
        },
        direct_fallback = "denied",
        max_retries = 1,
      },
      auth = {
        type = "basic_htpasswd_file",
        htpasswd_file_path = "/etc/oxiroute/proxy.htpasswd",
        realm = "Private proxy",
        username_case_sensitive = true,
      },
      access_policy = {
        rules = {
          { action = "allow", conditions = { { type = "authenticated" } } },
          { action = "deny", conditions = { { type = "all" } } },
        },
        default_action = "deny",
      },
      destination_policy = { deny_private = true },
      header_policy = { forwarded_for = "delete", via = "delete" },
      cache = { store = "forward-cache" },
      resolver = {},
      audit_mode = "off",
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
          vod = {
            max_sessions = 64,
            max_file_bytes = 67108864,
            max_duration_ms = 21600000,
            sources = {
              { type = "local", name = "archive", root_directory = "/var/lib/oxiroute/recordings" },
            },
          },
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
      udp = nil,
    },
  },
}
```

Current constraints:

- `version` MUST be `1`.
- Root `max_connections` is an optional aggregate process admission limit. Omission or explicit
  `null` means unbounded; a configured value MUST be a positive exact JSON integer.
- `management` is optional and MUST use a loopback IP with a nonzero port. The current
  configuration routes use bearer authentication, but the schema does not expose a remote
  management mode.
- `management.ui_dir` optionally points to a prebuilt Vue distribution loaded into memory at daemon startup.
- RTMP VOD sources are named local roots or HTTP/HTTPS origins. VOD paths are relative, no-follow,
  single-range objects with bounded file size, duration, and concurrent sessions. HTTP origins are
  resolved and checked against the RTMP service outbound policy before activation.
- `stats` is optional. `binds` and `pages` together contain one to eight nonoverlapping IPv4/IPv6
  sockets. Every `binds` socket serves public read-only `/metrics` and `/ready`; `/stats` and
  `/api/v1/status` require a loopback peer and the configured Bearer token. Server
  enable/disable uses `POST /stats/admin` with JSON `{ "pool": "...", "server": "...", "action":
  "enable" | "disable" }` plus `If-Generation-Revision`, and succeeds only for a loopback peer with
  exactly one matching Bearer token loaded from a no-follow regular file whose mode is `0400` or
  `0600`. GET and HEAD can never mutate pool state. Metric labels omit listener binds, upstream
  addresses, paths, stream keys, and token material. Each `pages` entry independently serves a
  public HAProxy-compatible HTML table below its exact `uri_prefix`; apart from that prefix it does
  not expose `/metrics`, `/ready`, the legacy authenticated `/stats`, `/api/v1/status`, or any other
  observability route on that socket.
  `refresh_ms` is `1` through `86400000`. `admin = "disabled"` makes the page read-only;
  `admin = "localhost"` shows and accepts Ready/Drain/Maintenance forms only for same-origin
  loopback requests with the active generation revision. Page administration does not use the
  statistics Bearer token and MUST NOT be treated as remote administration.
- Names MUST be unique within their certificate, TLS-profile, listener, pool, HTTP-service,
  forward-proxy-service, RTMP-service, or L4-service namespace.
- Listener binds MUST be unique after normalization.
- Names MUST contain non-whitespace text without surrounding whitespace or control characters.
- A listener `bind` is exactly one tagged object: `{ type = "socket", address = "IP:port" }` or
  `{ type = "unix", path = "/absolute/socket/path", mode = 432 }`. The optional Unix mode contains
  permission bits `001` through `777`, represented as an integer. Socket ports MUST be nonzero. Exact socket
  duplicates, wildcard socket binds that overlap another listener, management, or stats bind, and duplicate
  normalized Unix paths are rejected.
- Unix paths MUST be valid UTF-8 absolute paths of at most 107 bytes. Repeated `/` separators are
  collapsed; a root-only path, trailing `/`, NUL, and `.` or `..` segments are rejected. Unix
  listeners and upstreams can start only on Unix platforms. A Unix listener cannot use TLS. A Unix
  listener directory MUST be owned by the effective service user or have the sticky bit, and no
  path ancestor may be group/world writable unless it has the sticky bit. The runtime retains a
  mode-`0600` `<socket>.oxiroute.lock` ownership marker beside each Unix listener.
- HTTP, HTTP/3, forward-HTTP/1, forward-HTTP/2, forward-HTTP/3, RTMP, and TCP listeners MUST
  reference an existing same-kind service. An RTMP service MUST contain between 1 and 256 unique
  applications, and one configuration accepts at most 64 RTMP services.
- The `http` listener uses TCP and can negotiate HTTP/1.1 or H2 according to its TLS ALPN profile.
  `forward_http2` uses a TLS H2 listener, while `http3` and `forward_http3` use UDP/QUIC listeners;
  those protocol-specific paths do not silently fall back to another HTTP version.
- `tls_profile` is accepted on HTTP, forward-HTTP, and HTTP/3 listeners. Its named TLS profile and
  that profile's named certificate MUST exist. A socket-bound forward-HTTP/1 listener with
  `tls_required = true` MUST reference a profile advertising `http/1.1`; a Unix forward-HTTP/1
  listener remains no-TLS and MUST use `tls_required = false`. TCP and RTMP listeners reject
  `tls_profile` rather than implicitly changing protocol behavior.
- `max_connections` omitted or set to `null` means unbounded admission. A configured limit MUST be
  positive and no greater than `9007199254740991` so the monitoring API and UI preserve it exactly.
  Excess accepted connections are closed immediately after TCP accept, before TLS handshakes or
  protocol handling, and one admission remains charged for the complete transport connection
  lifetime. Deterministic canonical rendering writes the unbounded value as explicit `null`.
- Pools MUST contain between 1 and 256 uniquely named servers, and one configuration MUST contain
  at most 1024 servers in total. Each server owns one tagged `endpoint`, optional positive
  `max_connections`, and `dns_resolution = "startup" | "on_connect"`. Startup resolution applies
  only to DNS endpoints. An endpoint is exactly one of
  `{ type = "socket", address = "IP:port" }`,
  `{ type = "dns", host = "origin.example.com", port = 443 }`, or
  `{ type = "unix", path = "/absolute/socket/path" }`. Socket and DNS ports MUST be nonzero.
- DNS endpoint hosts are normalized to ASCII lowercase and MUST be DNS names of at most 253 bytes
  with nonempty labels of at most 63 bytes. IP literals, wildcards, trailing dots, non-ASCII bytes,
  and labels with non-alphanumeric edge characters are rejected. Validation does not resolve or
  expand a DNS endpoint. Its normalized host and port remain the stable selection, retry,
  monitoring, and topology identity; lookup occurs while establishing each connection or probe.
  A nonempty result is sorted and deduplicated into at most 16 addresses. Health probes, HTTP, and
  L4 traffic try that same deterministic address order until one connects; overflow and empty
  answers fail closed. One configured connect timeout bounds DNS resolution plus all address
  attempts for the logical endpoint.
- Server names and endpoints MUST both be unique after socket, DNS-name, and Unix-path normalization. A socket endpoint
  cannot directly target the loopback management endpoint because that would bypass its exposure
  boundary. Pools containing any Unix endpoint cannot enable upstream TLS or active health checks.
- `algorithm` accepts `round_robin` (the default), bounded `weighted_round_robin` with one weight
  from 1 through 100 per server, `least_connections`, or `first`. All skip unavailable and
  request-excluded servers. Weighted round-robin preserves server order and distributes turns by
  the configured weights. `first` chooses the first healthy administrative server with capacity.
  Least-connections selects the eligible named server with the least active work and rotates
  deterministic ties from the pool cursor. A capacity permit is acquired before connection
  preparation and, for reusable HTTP, remains held by the physical upstream socket through idle
  pooling and later requests. Nonreusable HTTP and L4 attempts release it at transport teardown.
  When every eligible server is at capacity, `queue_timeout_ms` bounds the wait for a released slot.
  L4 and `connection_reuse = "never"` HTTP requests join one pool-level FIFO at scheduler admission;
  only its head can claim whichever eligible server releases capacity next. Timeout and cancellation
  remove exactly one waiter and advance the next head. Reusable HTTP retains the connector's
  reuse-first capacity path, so this FIFO guarantee does not apply to requests sharing upstream
  connections or HTTP/2 streams.
- `passive_health` is optional per pool. `observe = "layer4"` counts only connect failures and
  timeouts; `observe = "layer7"` counts every attributed upstream failure. `on_error = "count"`
  ejects after `error_limit` consecutive observed failures, while `"immediately"` ejects on the
  first failure and `"mark_down"` applies the mark-down action after the error limit. `mark_down`
  controls whether ejection also marks the active health state down, and `mark_up` controls whether active recovery restores it. Initial and
  maximum ejection backoff are bounded to one day, and `recovery_threshold` is bounded to 100.
  Missing passive policy retains the bounded runtime defaults; it does not disable failure
  accounting. Ejection and recovery remain generation-owned and never revoke an existing lease.
- HTTP services MUST contain at least one route. Route pool references MUST resolve.
- Forward HTTP/1 services support absolute-form requests and classic CONNECT only when H1 is enabled.
  Opt-in RFC 9298 CONNECT-UDP uses an HTTP/1.1 Upgrade request and bounded Capsule Protocol
  DATAGRAM relay; the policy defaults to disabled, and `connect_udp = { enabled = true, allowed_ports = { 443 } }`
  requires H1 in `enabled_versions` and is honored only by a `forward_http1` listener. When enabled,
  its allowed-port list must contain 1 through 64 unique nonzero ports. This does not enable
  CONNECT-UDP on H2 or H3.
  Socket-bound listeners may terminate downstream TLS when `tls_required = true` and their profile
  advertises `http/1.1`; the TLS handshake and negotiated protocol are bounded before HTTP parsing.
  Unix listeners remain plaintext. CONNECT ports are explicit. Authentication is absent,
  Bearer-token-file, or Basic htpasswd-file;
  Basic username handling either preserves the client spelling or lowercases the client username
  before exact htpasswd lookup. An optional credential TTL caches a bounded, salted digest of each
  externally validated username/password pair; cache misses securely reload and verify the current
  file. Secret files are descriptor-safe regular files with restrictive ownership/modes. Ordered access
  rules use method, source CIDR, destination-port, authenticated, local, link-local, manager, and
  all matchers with optional negation. Destination domain/CIDR rules apply to the complete DNS
  answer, deny rules override allow rules, and `deny_private` rejects non-public addresses. Resolver
  cache/query/address/TTL limits, connection/body/header limits, connect/idle/lifetime deadlines,
  header privacy, and metadata-only audit mode are explicit. Forward HTTP/2 listeners implement
  only authority-only classic CONNECT and reject non-CONNECT or arbitrary forwarding forms. Forward
  HTTP/3 listeners expose authority-only classic CONNECT and bounded absolute-form forwarding;
  CONNECT-UDP remains an HTTP/1-only capability. Reverse H3 uses the UDP `http3` listener contract;
  forward H3 uses `forward_http3`. Both require TLS 1.3 with only `h3` ALPN, and neither silently
  falls back to another downstream protocol.
- A forward service's optional `cache` references a named memory or persistent `cache_store`. Only
  absolute-form H1 or H3 GET/HEAD requests with a safe request shape are eligible. CONNECT, unsafe
  methods, ranges, unsafe conditionals, proxy/authenticated requests, cookies, private or
  `Set-Cookie` responses, unsupported `Vary`, trailers, and oversized or incomplete responses bypass
  admission or fail closed. Cache fills are collapsed and revalidated through the same bounded cache
  contract as reverse HTTP; configured bearer-protected `PURGE` handles an exact request key or
  surrogate tag.
- HTTP/3 reverse services require a bounded service and route request body, request buffering, no
  response buffering, no gzip, no upgrade header mutation, and an exact `3/3` upstream pool for every
  proxy route. Fixed, redirect, descriptor-pinned static-file, and bounded memory/persistent cache
  actions are active; compression and upgrades remain explicitly unsupported. H3 cache uses the
  shared freshness, revalidation, stale-if-error, collapsed-fill, authenticated purge, surrogate-tag,
  bounded admission, and listener-metric contract. Trailer-bearing responses are forwarded but not
  admitted. The runtime uses bounded QUIC admission, field sections, streams, request bodies (at most
  64 MiB), response bodies, safe response framing, and graceful GOAWAY drain; migration and 0-RTT are
  disabled.
- `automatic_response_headers` defaults to true. When enabled, the runtime generates downstream
  HTTP/1 Date and Connection headers and HTTP/2 Date headers. When disabled, none of those headers
  are generated; mandatory HTTP/2 hop-header removal still applies.
- Listener `downstream_timeouts` independently represents optional client, request-header, and HTTP
  keepalive deadlines from 1 through 86400000 milliseconds. Request and keepalive deadlines are
  accepted only on HTTP listener protocols. The HTTP runtime enforces all three through downstream
  read deadlines; configured Unix listener modes are applied after descriptor-safe reservation.
- A missing route `host` matches any authority. `normalized_host` accepts an exact DNS name/IP
  literal or a single-label wildcard such as `*.example.com` and normalizes names to lowercase.
  `exact_authority` preserves and compares the authority bytes exactly, including any port.
  `ascii_case_insensitive_exact_authority` compares only ASCII case-insensitively and does not add,
  remove, or normalize a port. The nginx leading-wildcard and leading-dot selectors retain their
  documented suffix semantics.
- `path_prefix` defaults to `/`, matches only complete path segments, and has trailing slashes
  normalized away except for `/`. A missing or empty `methods` list matches every method;
  configured methods accept 1 through 32 authored HTTP-token bytes and normalize ASCII letters to
  uppercase during validation. Digits and RFC token punctuation are preserved. Route and cache method
  lists sort after normalization; ordered forward-access method lists preserve authored order.
- Route precedence is exact host, wildcard host, then host catch-all; within a host class the
  longest path prefix wins, and source order resolves any remaining tie. No match returns `404`.
- Routes with identical normalized host, path, and method matchers are rejected. Source order only
  resolves ties between distinct overlapping matchers.
- Requests with duplicate/conflicting authorities, userinfo authorities, dot path segments,
  repeated separators, backslashes, malformed escapes, encoded unreserved characters, or encoded
  path separators are rejected with `400` before upstream selection. Route prefixes use the same
  path policy so configured routes remain reachable. Accepted percent-triplet hex digits are
  canonicalized to uppercase for matching.
- Each route owns a `policy` with a nullable body limit, separate connect/read/write deadlines, and
  explicit request/response buffering booleans. Deadlines default to `30000`, buffering defaults to
  false, and the body limit defaults to 10 MiB while explicit `null` means unbounded. Request
  buffering reads the complete body before upstream connection; response buffering requires a
  positive body limit and only admits a response with a fixed `Content-Length` at or below that
  limit. Unbounded, chunked, trailer-bearing, or oversized responses fail as upstream errors before
  their body is forwarded. Buffering-off is the active Pingora streaming behavior.
- The version-1 service fields `upstream_io_timeout_ms` and `max_request_body_bytes` remain accepted
  because current canonical files use them. New configuration uses route policy. The service I/O
  timeout defaults to `30000` and applies independently to upstream connect, read-inactivity, and
  write-inactivity operations; it also establishes the request deadline's upper bound, so I/O
  progress does not extend the total request deadline. Omitted `max_request_body_bytes` defaults to `10485760`; explicit
  `null` means unbounded streaming, and a configured limit MUST be positive. Oversized declared
  bodies return `413` before contacting an origin. A streamed overflow aborts forwarding and
  returns `413` when an origin response has not already committed. Canonical rendering preserves
  the unbounded policy as explicit `null`.
- `max_retries` is the number of additional connection attempts after the first, defaults to `0`,
  and MUST be at most `3`. A transient connection-establishment failure before any request bytes are
  sent may retry any non-upgrade request because no request is replayed. A refused HTTP/2 stream is
  replayed only for bodyless `GET` and `HEAD` requests; `method_safety = "get_head"` and
  `body_safety = "empty"` express that replay boundary. Every retry also requires the configured
  `target` to be selectable. `target = "next_server"` requires a distinct named server;
  `target = "same_server"` reselects the same named server. `delay_ms` defaults to `0`, is bounded to
  `60000`, and is applied before each route retry. `response_statuses` optionally lists unique 5xx
  statuses that trigger bounded retries; a status-only policy is valid when its trigger list is
  empty. The `empty_response`, `response_timeout`, and `junk_response` triggers cover bounded
  pre-body response failures. Trying alternate addresses for one DNS endpoint is transport fallback
  and does not consume `max_retries`; route retry begins only after that bounded address set is
  exhausted. Established-connection errors other than configured response/error triggers, upgrades,
  and unsafe or body-bearing request replays are never retried. Each
  attempt is clamped to the remaining total request deadline, whose default upper bound is the
  service `upstream_io_timeout_ms` and which is reduced by the listener's active request timeout.
  `final_redispatch = true` changes only the last configured retry from `same_server` to
  `next_server`; it requires positive `max_retries` and `target = "same_server"` and otherwise
  fails validation. It defaults to `false`.
- L4 services reference a pool. Connect and idle timeouts default to `10000` and `300000`
  milliseconds; an optional lifetime timeout has no default. Configured timeout values MUST be
  nonzero. An L4 service MUST NOT reference a TLS-enabled upstream pool; opaque TLS pass-through
  uses an ordinary plaintext-configured TCP pool and does not terminate or originate TLS.
- Unknown fields and unknown protocol values are errors.
- Every authored source and deterministic rendered output is limited to 1 MiB. Generic source trees
  are additionally limited to 128 structural levels, 100,000 nodes, 256 KiB per string, 64 template
  inheritance levels, and 4,096 recorded native dependency paths.
- Restricted Lua has the same 1 MiB source limit, 4 MiB of additional memory, and one million
  instructions. No Lua standard libraries are loaded and binary chunks are rejected. Lua does not
  support generic templates or native-server references.

### Host replacement policy objects

The remaining host-required behavior uses closed typed objects rather than arbitrary nginx or
HAProxy directives:

- Pools optionally define one-day-bounded queue/connect/server timeouts and
  `connection_reuse = "never" | "safe" | "always"`.
- Health checks add startup state, regular/fast/down intervals, exact HTTP status, HTTP/1.0 or 1.1,
  and an optional Host authority. Omitted HTTP status and version retain 200 and HTTP/1.1 behavior.
- `nginx_leading_wildcard` stores a suffix matched by one or more leading labels;
  `nginx_leading_dot` additionally matches the suffix itself. Regex names remain unsupported.
- Dynamic request-header values are closed to appended X-Forwarded-For, downstream scheme, and one
  named incoming header. Values derived from request headers carry an explicit 1 through 8192 byte
  output bound. Hop-by-hop mutations remain forbidden except for the exact standard nginx WebSocket
  pair, which is bounds-checked but left under Pingora's upgrade ownership. Proxy policies optionally
  replace one canonical incoming path prefix with one canonical upstream path prefix while preserving
  the request query.
- Basic access loads one bounded no-follow regular htpasswd file with mode `0400`, `0600`, `0440`,
  or `0640` and accepts bcrypt `$2a$`/`$2b$`/`$2y$` and Apache APR1 `$apr1$` entries. Mixed schemes
  and bcrypt costs are supported; malformed salts/digests, duplicate users, excessive bcrypt costs,
  and unsupported hashes fail preparation. Verification is semaphore-bounded, runs off the async
  executor, and performs one check for every configured scheme/cost class.
  Cookie attribute rules are keyed by exact cookie name and set or clear Secure/HttpOnly and SameSite.
- Static actions choose root or alias path mapping and support an ordered closed `try_files` list,
  index lookup, exact/human autoindex sizes, UTC/local autoindex timestamps, bounded MIME mappings,
  literal headers, optional nginx-format `mtime-size` ETag emission, status-to-relative-file error
  responses, single byte
  ranges, and 416 responses. Disabling ETag suppresses generated tag matching and output, while
  `If-None-Match: *` still tests whether the selected representation exists and takes precedence
  over `If-Modified-Since`.
  Roots are descriptor-pinned; files are opened no-follow and stream in 64 KiB chunks up to 8 GiB.
- Gzip exposes level 1 through 9, bounded exact content types, `min_length_bytes`, a minimum request
  version of HTTP/1.0 or HTTP/1.1, optional suppression when `Via` is present, and optional
  `Vary: Accept-Encoding`. For compatibility with persisted minimal policies, omitted new fields
  mean 20 bytes, HTTP/1.0 allowed, `Via` allowed, and `Vary` enabled. Native nginx import does not
  use those compatibility defaults: it materializes nginx's effective 20-byte, HTTP/1.1,
  `gzip_proxied off`, and `gzip_vary off` policy. Streaming compression emits gzip only, combines
  repeated `Accept-Encoding` fields, honors quality zero and wildcard rules, and applies an exact
  coding before a wildcard. Compression is limited to nginx's 200, 403, and 404 response status
  set; eligible representations still emit `Vary` when request version or `Via` policy suppresses
  compression. HTTP and RTMP access logs are either disabled or use the
  implementation's fixed structured format at one validated absolute path; custom format strings
  are deliberately absent. JSONL logging uses a bounded asynchronous writer opened through
  descriptor-pinned ancestors. HTTP access events omit URI, query, Authorization, Cookie, and
  values derived from credentials. RTMP events use a separate fixed schema containing only event,
  result, listener/service/application/stream/session identifiers, role, byte/message counters,
  timestamps/duration, a bounded failure code, and a safe correlation ID; they omit queries, tokens,
  credentials, URLs, payloads, and client addresses. Queue saturation is nonblocking and exposed
  through aggregate bounded metrics; no log rotation or retention policy is implied.

Anonymous `endpoints` remain decode-only compatibility for current version-1 files. Validation
assigns deterministic `endpoint-N` identities, clears the legacy collection, and deterministic
rendering emits only `servers`.

The server runtime enforces aggregate/listener admission, Unix modes, downstream and route-local
policies, nginx suffix routing, bounded headers/auth/cookies, static extensions, gzip and HTTP
logging, named-server capacity and bounded queue waits, pool deadlines/reuse, startup/on-connect DNS,
all four balancing algorithms, configurable passive health, and the extended health policy.
Bounded request buffering and bounded fixed-length response buffering are active; only unbounded
buffering fails startup. Supported memory/persistent cache policies are active for reverse HTTP and
eligible HTTP/1 and HTTP/3 forward requests, while unsupported cache forms fail closed. RTMP relay/fanout
controls and canonical named-recorder policies compile into the current runtime, including bounded
HLS/DASH media, same-daemon auto-push, and isolated RTMP exec profiles; unsupported native RTMP
semantics and enhanced codecs remain blocked. Importers
remain separate adapters, except that finalized nginx/HAProxy/Apache/Squid/Varnish references can be
composed by
the declarative source resolver where documented.

### Downstream certificates and TLS profiles

`certificates` and `tls_profiles` default to empty collections. A certificate source may be a direct
file pair, an operator-owned Certbot live/archive lineage, a managed ACME state root, or a labeled
self-signed development identity:

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
    policy = {
      cipher_list = nil,
      dh_parameters_path = nil,
      session_cache = nil,
      session_timeout_seconds = nil,
      session_tickets = false,
      prefer_server_ciphers = true,
      client_auth = {
        mode = "disabled",
        ca_certificate_path = nil,
        allowed_dns_names = {},
      },
    },
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
  1 through 100 declared `dns_names`; DNS names are lowercased and IP literals are canonicalized,
  including IPv4-mapped IPv6 literals to IPv4. Duplicates after normalization are rejected. DNS
  identities MUST be ASCII names of at most 253 bytes with labels of at most 63 bytes. Trailing
  dots and wildcards other than one leading `*.` label are rejected.
- Direct-file certificate and key paths MUST be distinct, valid UTF-8 absolute paths of at most
  4096 bytes. Certbot live and archive directory paths follow the same lexical rules and MUST be
  distinct. NUL, repeated `/`, a trailing `/`, and `.` or `..` segments are rejected lexically.
- Runtime preparation requires regular, nonempty files and reads each file twice with identical
  content. The certificate chain is limited to 1 MiB and 16 certificates total; it MUST contain the
  leaf first and at least one ordered issuer. The private key is limited to 256 KiB and, on Unix,
  MUST have exactly mode `0400`, `0600`, `0440`, or `0640`.
- TLS policy defaults preserve the runtime's Mozilla intermediate cipher and DH choices, disable
  the server session cache and tickets, retain OpenSSL's default session timeout, and prefer server
  cipher order. `cipher_list`, `dh_parameters_path`, `session_cache`, and
  `session_timeout_seconds` override those defaults only when present.
- Downstream client authentication defaults to `disabled`. `optional` requests a client certificate
  but permits an absent certificate; `required` rejects the handshake when no certificate is sent.
  Enabled modes require one bounded absolute `ca_certificate_path`. The CA bundle is read twice for
  stable identical content, limited to 1 MiB and 128 CA certificates, and each certificate must be
  current, unique, CA-capable, and usable by strict OpenSSL `SSL_CLIENT` chain validation. The
  parsed bundle is retained in the immutable TLS profile generation; listener rotation does not
  reread or mutate an existing connection's policy.
- A client certificate must contain at least one exact DNS or IP SAN; the common name is never a
  fallback. An empty `allowed_dns_names` list accepts any SAN-bearing certificate trusted by the
  configured CA bundle. Otherwise at least one exact normalized SAN must be listed. Wildcard client
  identities are rejected. Client-auth mode, CA presence, and allowed-identity count are reported
  in redacted topology/status output; CA paths, SAN values, PEM, and private key material are not.
- The same `disabled`, `optional`, and `required` client-auth policy applies to reverse `http3` and
  `forward_http3` listeners. Their Quinn/rustls TLS setup uses the immutable prepared CA and exact
  SAN policy; it never rereads the CA path or infers client identity from HTTP request headers.
  Missing certificates in `required` mode, invalid chains, untrusted issuers, disallowed SANs, and
  malformed certificates fail the QUIC handshake before HTTP/3 request bytes are processed.
- A shared `session_cache` has a 1-through-255-byte ASCII `name` and an exact `size_bytes`. Runtime
  planning estimates one session per 256 bytes, enables OpenSSL's bounded server cache, and derives
  a distinct 32-byte session-id context from the profile and cache names. DH parameter files are
  bounded to 64 KiB, read twice for stable identical content, parsed as PEM parameters, and applied
  before the listener starts.
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
- The leaf MUST contain DNS and/or IP SANs; the common name is not a fallback. After lowercase DNS
  normalization and identical IP canonicalization, including IPv4-mapped IPv6 to IPv4, the complete
  SAN identity set MUST exactly equal the declared `dns_names` set.
- Every TLS profile MUST reference a nonempty list of unique certificates and name one listed
  `default_certificate`. Two certificates in the same profile MUST NOT claim the same normalized
  DNS SAN. During a handshake, an exact DNS SNI match wins over a one-label wildcard match; IP,
  unknown, non-DNS, and absent SNI select the explicit default certificate.
- `min_version` accepts only `"1.2"` (the default) or `"1.3"`. `alpn` defaults to
  `{ "http/1.1" }`; the only accepted policies are `{ "http/1.1" }`, `{ "h2" }`, and
  `{ "h2", "http/1.1" }` in that order.
- Downstream TLS session caching and tickets are disabled. Every new handshake selects one identity
  and takes one immutable certificate-generation snapshot through the TLS callback, so an atomic
  generation publication cannot mix key and chain material and existing connections retain their
  selected generation. Publication is independent per identity and requires the same certificate
  identity and exact declared DNS-name set.
Direct-file identities are loaded into a startup generation and are then registered with a process-lifetime
`FileReconciler`/`FileWatcherSupervisor`. The direct-file watcher observes the configured PEM parent
directories, debounces events, periodically rescans, validates a stable complete certificate/key pair,
and atomically publishes each valid replacement generation. An invalid or unstable candidate is not
published: the last valid active generation is retained and direct-file watcher health becomes degraded
until a later valid reconciliation succeeds. This watcher observes externally written PEM files only; it
does not edit canonical configuration or provide direct-file certificate upload/edit API actions.
Canonical configuration changes continue through the separate canonical configuration watcher and
generation-activation path. Configured Certbot identities have their own process-lifetime lineage
watcher that validates and atomically publishes complete replacement generations. Managed ACME jobs and
certificate-management API actions also publish validated replacement generations. Self-signed
development identities remain in-memory startup generations without a replacement watcher.

Downstream HTTP/2 is available only on a TLS listener whose ALPN policy includes `h2`. Plaintext
HTTP listeners are HTTP/1.1-only; h2c is not supported. Pingora advertises a 64 KiB decoded
header-list limit and 100 concurrent streams. DATA reads and writes follow H2 flow control rather
than buffering an entire request or response; configured request-body and listener connection
limits remain active per stream and connection. H2 request-body read deadlines and response write
deadlines are clamped to the active request deadline, and cancellation or timeout resets the
affected upstream stream. gRPC has no separate configuration object: it is proxied over a
compatible downstream and upstream H2 path, preserving DATA and trailing metadata. An H2-only
listener rejects an incompatible ALPN offer during negotiation. A client that omits ALPN can
complete TLS, but OxiRoute closes the stream before HTTP parsing instead of allowing Pingora's
HTTP/1.1 fallback. HTTP/2 stream takeover and WebSocket-style upgrades are not interpreted as
opaque H2 bodies.

### Upstream TLS and HTTP versions

An HTTP upstream pool enables verified TLS by adding `tls`. `ca_certificate_path` is optional; when
omitted, Pingora uses its default trust roots.

```lua
upstream_pools = {
  {
    name = "secure-origin",
    servers = {
      {
        name = "origin-1",
        endpoint = { type = "dns", host = "origin.example.com", port = 443 },
      },
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
  `1.1/2`, `2/2`, and exact `3/3`. Flexible `1.1/2` permits ALPN fallback to HTTP/1.1; `2/2`
  requires H2 and rejects a downgrade before HTTP headers. Under `2/2`, H2 DATA, trailers, stream
  reset, flow-control, and deadline failures remain H2 failures and are never retried by changing
  the request to HTTP/1.1. Exact `3/3` requires `tls` and uses a
  separate one-request-per-connection QUIC/H3 client with TLS 1.3, SNI, `h3` ALPN, disabled 0-RTT,
  bounded request/response fields and bodies, deadlines, cancellation, response trailers, and
  bounded safe retries. It never falls back to HTTP/1.1 or HTTP/2. Any range with `max = "2"`
  requires `tls`; upstream h2c is not supported.
- A service route using an exact `3/3` pool must be attached only to an `http3` listener. It cannot
  share that service with a non-HTTP/3 listener. A route using an H3 pool with unsupported listener
  or upstream semantics is rejected during configuration validation.
- Upstream TLS always verifies both the certificate chain and hostname. A pinned, documented
  Pingora connector hook applies security level 2, a TLS 1.2 minimum, ECDHE+AEAD TLS 1.2 ciphers,
  and standard TLS 1.3 AEAD suites before handshake. The negotiated digest is checked again before
  request headers as defense in depth.
- `health_check` and `tls` are mutually exclusive on one pool because current active checks are
  plaintext TCP or HTTP/1.1. TLS-enabled pools are valid only for HTTP services and are rejected by
  L4 services. Any pool containing a Unix endpoint rejects both `health_check` and `tls`.

### Active pool health checks

`health_check` is optional on every upstream pool. Omitting it leaves the pool's servers in the
selectable `unchecked` state. When present, it has this strict schema:

| Field | Required | Default | Constraint |
| --- | --- | --- | --- |
| `type` | yes | none | `tcp` or `http` |
| `interval_ms` | no | `10000` | `1000` through `86400000` inclusive |
| `timeout_ms` | no | `1000` | `1` through `30000` inclusive and less than or equal to `interval_ms` |
| `healthy_threshold` | no | `1` | `1` through `100` inclusive |
| `unhealthy_threshold` | no | `3` | `1` through `100` inclusive |
| `startup` | no | `checking` | `healthy`, `unhealthy`, or `checking` |
| `fast_interval_ms` | no | none | `1000` through `86400000` inclusive |
| `down_interval_ms` | no | none | `1000` through `86400000` inclusive |
| `host` | HTTP only | none | Optional HTTP authority, at most 255 bytes, without userinfo; any port MUST be numeric |
| `path` | HTTP only | none | Required query-free absolute path, at most 2048 bytes, accepted by the request-path ambiguity policy |
| `expected_status` | HTTP only | `200` | `200` through `599` |
| `http_version` | HTTP only | `1.1` | `1.0` or `1.1` |

TCP checks accept no HTTP-specific fields and succeed when a connection to the endpoint is
established. HTTP checks use the configured path, version, optional Host, and expected status.
`timeout_ms` bounds the complete probe. DNS
endpoints are resolved for each probe and each returned address may be attempted; Unix endpoints
cannot be health checked.

Health-enabled servers start in the configured `healthy`, `unhealthy`, or non-selectable `checking`
state. The healthy threshold must be met by consecutive successes before a checking or unhealthy
server becomes `healthy`; the unhealthy threshold must be met by consecutive failures before a
checking or healthy server becomes `unhealthy`.
Success resets the failure streak and failure detail, while failure resets the success streak.
Round robin and least-connections selection skip unknown and unhealthy endpoints, and a matched
HTTP route whose pool has no selectable endpoint returns `503`.

Each server runs its first probe immediately, then waits `fast_interval_ms` while checking,
`down_interval_ms` while unhealthy, or `interval_ms` while healthy; an omitted state-specific
interval uses `interval_ms`. A slow server does not shift another server's schedule, even within the
same pool. All pools share a limit of 32 concurrent probes, and a server never overlaps its probes.

### Cache policy timeline

Canonical cache stores and reverse per-route or forward per-service cache policies compile into the
runtime. Memory and descriptor-safe disk stores share the bounded cache contract below; disk lookup,
touch, admission, revalidation, and purge work run through a bounded blocking-I/O executor.
Unsupported request forms such as ranges and unsafe conditional preconditions bypass cache reuse and
continue to the origin.

- A matching `status_ttls` entry overrides both origin freshness and `default_ttl_ms`.
- Otherwise, explicit origin freshness is used when `use_origin_cache_control` is true; absent or
  ignored origin freshness falls back to `default_ttl_ms`.
- `grace_ms` permits stale serving only after a configured failure in `stale_on`; it does not enable
  ordinary stale hits or background stale-while-revalidate behavior.
- `keep_ms` retains a stale representation only for conditional revalidation. Once TTL plus keep
  expires, lookup becomes a miss and removes the resident entry.
- `grace_ms` cannot exceed `keep_ms`. Status TTLs can target final statuses from 200 through 599,
  except partial `206` and not-modified `304` responses.

Persistent cache record version 2 stores this retention mode. Version 1 records remain readable as
RFC-policy records without a finite canonical keep window.

A prepared cache entry is bound to the shared cache identity that validated it. Memory and disk fill
guards reject an entry prepared by another cache before eviction, quota, or disk publication; cache
clones share the identity, and recovered disk entries are rebound only to the cache that recovered
them. A persistent root is exclusively leased and may be reused by an overlapping generation only
when its complete store configuration is unchanged.

The active reverse HTTP slice admits GET and HEAD representations, preserves collapsed forwarding,
origin revalidation, conditional `ETag`/`Last-Modified` hits, bounded surrogate tags, exact/base
PURGE, and bearer-protected tag purge. The forward HTTP/1 slice uses the same bounded storage and
timeline for eligible absolute-form GET/HEAD requests, including collapsed fills, origin
revalidation, bounded streaming capture, exact/base PURGE, and listener cache outcome metrics. A
route or forward service using purge must also configure bearer authorization.
Range requests and `If-Match`, `If-Unmodified-Since`, `If-Range`, or streamed/upgraded responses
are not admitted to the cache.

This schema is pre-release and may change without compatibility code until a public release
persists it.

## Canonical model and future extensions

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
- Further endpoint-selection algorithms beyond the current `round_robin`, bounded
  `weighted_round_robin`, `least_connections`, and `first` set; broader passive-health and
  circuit-breaker behavior; TLS, SNI, and verification.
- HTTP minimum and maximum versions.
- Retry and circuit-breaker limits.

### L4 service

- TCP or UDP transport.
- Pool reference, affinity policy, timeouts, limits, and optional bounded inspection.
- `udp` is optional on an L4 service and is used when the service is attached to a `udp` listener.
  Its `max_datagram_bytes`, `max_sessions`, `max_session_bytes`, `max_queue_datagrams`, and
  `max_queue_bytes` fields are positive bounded integers. Queue and session byte limits must each
  be at least `max_datagram_bytes`; omitted UDP policy fields use bounded defaults.

### Certificate and TLS profile

Certificates are reusable lifecycle objects. TLS profiles reference them and define
listener behavior.

```lua
certificates = {
  {
    name = "public-example",
    source = {
      type = "acme_managed",
      directory_url = "https://acme-v02.api.letsencrypt.org/directory",
      state_root = "/var/lib/oxiroute/acme",
      contacts = { "mailto:ops@example.com" },
      terms_agreed = true,
      challenge = "tls_alpn01",
      key_type = "ecdsa_p256",
      allowed_dns_suffixes = { "example.com" },
    },
    dns_names = { "example.com", "www.example.com" },
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

The canonical managed source uses `challenge = "http01"`, `"dns01"`, or `"tls_alpn01"`. DNS-01 sources
must provide an exact statically linked provider name, a protected credential file, and a bounded
`timeout_seconds` value. A wildcard DNS name is valid only with DNS-01; the configured
`allowed_dns_suffixes` policy applies to the wildcard's base name. Credential contents are never
rendered into API, event, metric, or UI state. Managed sources may set `retained_revisions` (1-32,
default 3) and `retention_days` (1-3650, default 30); garbage collection always preserves the active
revision and the newest retained revisions.

### RTMP service and recorder

The current RTMP model supports live applications, structured push/pull targets, bounded fanout,
service outbound policy, bounded HTTP callbacks, named local/HTTP VOD sources, HLS/DASH media
segmenter policies, same-daemon auto-push, isolated exec profiles, and canonical recorder policies.
The strict nginx-RTMP
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
    outbound_chunk_size = 4096,
    max_inbound_message_size = 8388608,
    ack_window_size = 5000000,
    access_log = { type = "disabled" },
    applications = {
      {
        name = "live",
        live = true,
          idle_streams = true,
          push_targets = {
            { host = "127.0.0.1", port = 1936, application = "$name" },
          },
          relay = {
            dns_refresh_ms = 60000,
          },
          fanout = {
            max_subscribers = 1024,
            max_queue_messages_per_subscriber = 256,
            max_queue_bytes_per_subscriber = 8388608,
          },
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

`outbound_chunk_size` defaults to 4096, is bounded to 1 MiB, and is announced by the server session
on wire. `max_inbound_message_size` defaults to 8 MiB, is bounded to 8 MiB, and rejects an
assembled inbound RTMP message before its payload is allocated. `ack_window_size` defaults to
5,000,000 bytes and is announced to the peer during session startup. Push and pull targets are
unique structured host, port, application, and stream tuples;
they are resolved and pinned during runtime planning, reject any resolved direct listener loop, and
are valid only for live applications. Application `$name` expands to the exact source stream name;
the destination stream name is always the exact source stream name. Each publisher incarnation owns
an independent bounded relay worker, while each pull target owns one bounded reconnecting source
worker. An unavailable target retries every three seconds and can recover later; relay backpressure
never blocks local viewers or recorders. `relay.dns_refresh_ms` defaults to 60,000 ms and accepts
1,000 through 300,000 ms. Before each initial connection or reconnect, a due refresh resolves the
canonical hostname again; the complete answer is bounded and rechecked against the outbound policy,
selected address family, and direct-listener loop protection. A failed refresh retains the last valid
address and is retried at the next interval. The canonical hostname and URL never change, and
refresh attempts, successes, failures, and the last failure category are exposed only through the
bounded RTMP state and metrics fields. Fanout limits default to 1024
subscribers, 256 queued messages, and 8 MiB per subscriber and compile per application.
Other `$` substitutions are rejected instead of being treated as implicit templates.

`outbound_policy` applies to relay and callback destinations. It checks every resolved address,
rejects private/local destinations by default, pins the admitted address for reconnects, and applies
the RTMP/RTMPS and chain-depth policy. `callbacks` supports bounded `on_connect`, `on_disconnect`,
`on_publish`, `on_publish_done`, `on_play`, `on_play_done`, `on_done`, and `on_update` endpoints.
Authorization callbacks require a 2xx response; update callbacks may be disabled with a zero update
timeout, and strict update mode closes the RTMP session after a failed update.

Recorder fields are:

| Field | Required | Default | Constraint |
| --- | --- | --- | --- |
| `name` | yes | none | Unique nonblank canonical name within the application. |
| `start` | no | `"continuous"` | `"continuous"` or `"manual"`. |
| `root_directory` | yes | none | Normalized absolute UTF-8 directory path, at most 4096 bytes. |
| `suffix_template` | no | `".flv"` | At most 128 bytes; only UTC `%Y`, `%m`, `%d`, `%H`, `%M`, `%S`, and `%%`; no NUL or path separator. |
| `append_unix_seconds` | no | `false` | Appends `-<segment-start Unix seconds>` before the suffix. |
| `timezone` | no | `"utc"` | `"utc"` or an explicit IANA name such as `"America/Bahia"`; historical DST comes from `chrono-tz`. nginx imports require one uniquely consumed captured-host timezone overlay. |
| `time_basis` | no | `"segment_start"` | `"segment_start"` renders suffix time fields from the current segment's opening timestamp; `"segment_end"` uses its closing timestamp. |
| `segment_naming` | no | `"safe_unique"` | Sequenced safe names or `"nginx_compatible"`, which rerenders the suffix and `record_unique` seconds for every segment; publication remains no-replace and collision-safe. |
| `rotation_interval_ms` | no | `null` | `null` or `1` through `2147483647`. |
| `max_queue_messages` | no | `256` | `1` through `65536`. |
| `max_queue_bytes` | no | `8388608` | `1` through `1073741824`, and no greater than `max_storage_bytes`. |
| `shutdown_timeout_ms` | no | `5000` | `1` through `60000`. |
| `max_storage_bytes` | no | `null` | `null` for no byte quota, or `1` through `1099511627776`. |
| `max_storage_files` | no | `null` | `null` for no file-count quota, or `1` through `1000000`. |
| `max_active_recorders` | no | `8` | `1` through `256`. |

Repeated `/` separators in `root_directory` are collapsed. `/`, a trailing `/`, NUL, `.` or `..`
segments, relative paths, and non-UTF-8 paths are rejected. Recorders sharing one normalized root
MUST configure identical storage-byte, storage-file, and active-recorder limits.

Runtime planning opens the existing root one component at a time without following symlinks and
performs a read-only ownership/quota preflight. The root MUST be owned by the daemon user and MUST
NOT be writable by group or other users. Candidate config validation neither creates the root nor
creates a lease, probe, partial, or recording file. Actual RTMP service activation opens and pins the
root, acquires ownership directly on that immutable directory descriptor, may clean exact abandoned
partials only under exclusive ownership, and can still fail if the root changed after preflight.
Errors identify the service/application/recorder but redact the root path.

Existing regular files count against root quotas. Stores for the same directory identity share
byte, file, and active-recorder counters within one daemon process. The ownership protocol protects
partial cleanup across processes, but quota counters are not distributed: multiple daemon
processes can collectively exceed the configured limits and require deployment-level isolation.
The suffix does not select a container: recorder output remains FLV even when the suffix is `.mp4`.
RTMP `access_log = { type = "disabled" }` explicitly emits no session access records while
transport, protocol, relay, and recorder failures remain operationally observable. RTMP file access
logging uses the same safe no-follow parent preflight and bounded asynchronous sink as HTTP, with
the distinct RTMP schema and shutdown flush behavior described above.

## Deterministic Rendering

The backend, not the browser, renders typed JSON. KDL is the default preview and `config compose`
output. Revision-checked API saves render the syntax selected by the configured root path: KDL,
restricted Lua, UCI, or HOCON.

- Rendering MUST be deterministic for the same typed model.
- A successful UI save normalizes formatting and field order.
- Comments, source formatting, Lua expressions, HOCON substitutions/merges, UCI record names,
  templates, and native-reference declarations do not round-trip through a typed save.
- A root using `templates`, `nginx_server`, `haproxy_server`, `squid_server`, `apache_server`, or `varnish_server` is marked compositional. The backend
  rejects typed replacement of that root with `E_COMPOSITIONAL_ROOT`; operators must edit the source
  graph directly or explicitly flatten it with `config compose` into a separately owned file.
- The API MUST state that normalization will occur before accepting a save.

## Revisions and activation

- `diskRevision` is the SHA-256 hash of the exact authored root-file bytes.
- `candidateRevision` is the SHA-256 hash of deterministic normalized KDL after template expansion,
  native resolution, composition, defaults, and validation. `activeRevision` identifies the
  compiled runtime generation using that effective revision.
- The implemented API requires one raw 64-hex `If-Config-Revision` header on writes.
- The backend MUST re-read and compare immediately before writing.
- A mismatch returns a conflict and does not write.
- Validation and writes prepare the complete candidate runtime, management UI assets, and Certbot
  watcher prerequisites before any disk mutation. The sole live-reservation exception is an
  explicitly detected active Unix-listener mode change: the plan is validated without rebinding the
  active path and the complete candidate is marked restart-required.
- Writes use a unique same-directory temporary file, complete write, permission setting,
  file sync, atomic rename, and parent-directory sync.
- A changed save normally returns `saved_pending_activation`; an idempotent save of the active
  generation returns `unchanged_active`. The parent-directory watcher and generation supervisor
  prepare, start, and atomically publish reloadable changed generations in-process. A candidate that
  changes the mode of an active Unix listener instead returns `saved_restart_required`,
  `activationState = "restart_required"`, and `restartRequired = true`; candidate preparation does
  not mutate or silently reuse the active socket, and the complete candidate is applied on process
  restart.
- The watcher debounces root-directory and resolved-native dependency events, watches exact resolved
  files plus include/glob parent directories, rebuilds that set after each successful resolution, and
  periodically re-resolves the root and native references. Invalid edits are rejected while the last
  active generation continues serving.

## Diagnostics

Offline native import reports retain rich evidence: stable code, severity, stage, source range,
include/import stack, related ranges, explanation, and suggested resolution. The management
configuration API intentionally returns a compact redacted diagnostic shape containing only
`code`, `severity`, `stage`, and `message`; native-reference failures crossing that boundary expose
the importer name and diagnostic-code counts rather than source paths, source text, rejected values,
or operating-system errors. Use the offline report for source-level migration detail.

Initial code families:

- `E_SYNTAX`, `E_UNKNOWN_FIELD`, `E_INVALID_VALUE`, `E_DUPLICATE_IDENTITY`
- `E_DUPLICATE_BIND`, `E_UNRESOLVED_REFERENCE`, `E_INCLUDE_CYCLE`
- `E_UNSUPPORTED_FEATURE`, `E_SEMANTICS_NOT_REPRESENTABLE`
- `E_CERTIFICATE_INVALID`, `E_SECRET_UNAVAILABLE`, `E_RUNTIME_PREPARE`
- `W_NATIVE_VALIDATION_REQUIRED`, `W_NONPORTABLE`, `W_ORDER_DEPENDENT`

Warnings never convert an unsafe or unrepresentable service into an active one.
