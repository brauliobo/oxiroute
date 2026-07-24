# Native configuration import specification

## Contract

Import means parsing and translating a documented subset into the canonical model. It does
not mean loading foreign modules or reproducing undocumented implementation accidents.

The target import result includes:

- Parsed source files and include graph.
- Source product and detected version/capability profile.
- Canonical candidate objects with provenance.
- Blocking errors and non-blocking warnings.
- Unsupported directives grouped by affected service.
- A compatibility summary suitable for CLI, API, and UI display.

The daemon MUST NOT activate an affected service if a directive that changes its routing,
security, TLS, listener, or upstream semantics cannot be represented.

## Current implementation boundary

Import is currently a Rust library capability in `oxiroute-import`. The daemon has no import CLI,
management API, UI workflow, watcher, or activation integration.

- nginx exposes an explicit HTTP-fragment importer: the expanded root may contain only one `http`
  block. It has bounded byte-preserving parsing, deterministic includes/globs, provenance, HTTP
  inheritance, an occurrence ledger, blocked-service reports, and conditional canonical lowering.
  It rejects complete nginx files rather than implying support for `events`, process, or module roots.
- The strict nginx HTTP subset can finalize exact/default virtual hosts with representable flat
  routes, fixed responses or redirects, static socket/DNS/Unix origins, explicit proxy defaults,
  exact response-control suppression, response hide/pass defaults, and shared named upstream pools.
  URI replacement, incompatible routing/admission policy, and static index behavior remain blocking;
  a blocked service is retained only in the report draft.
- nginx-RTMP has an independent 117-directive parser/registry plus deterministic include expansion,
  effective inheritance, a terminal occurrence ledger, provenance, blocked-service reporting, and
  strict conditional canonical finalization for an exact listener/application/recording subset.
  Import never accesses a configured recording root. Most directive families remain blocking, and
  the daemon still has no import integration.
- HAProxy has ordered `-f` file/directory loading, byte-preserving lexing/parsing, defaults and
  frontend/backend/listen resolution, a terminal decision ledger, stable diagnostics, provenance,
  and conservative canonical lowering.
- HAProxy finalizes only complete, error-free candidates. The tested finalizable slice is strict
  static TCP with explicit compatible modes, exact socket or Unix binds, exact positive
  per-listener `maxconn`, static socket/DNS/Unix servers, explicit `roundrobin` or `leastconn`, zero
  retries, and representable connect/client/server timeout scopes. DNS names remain canonical and
  are not resolved during import.
- The synthetic `hostrouter-static-representable.cfg` fixture proves that an audited-shaped Unix
  frontend plus DNS `leastconn` backend can finalize; it is not audited-host evidence. The complete
  audited active candidate remains blocked by logging/stats/process policy, aggregate admission,
  HTTP `leastconn` accounting, checked-server startup eligibility, retry/redispatch behavior,
  timeout scopes, and forwarded-header policy.
- Squid has a bounded source/include, parser, and typed semantic-report foundation. Final recheck
  verifies source bytes, root/include path identities, and include-glob result sets; it emits no
  canonical candidate and has no runtime integration.
- Varnish has a bounded VCL source/include, parser, typed semantic-report, decision-ledger, and
  invocation-model foundation. It has no canonical lowering, runtime, or daemon integration.
- Apache httpd import is not implemented.

Coverage manifests and importer tests enforce that a candidate cannot finalize while any blocking
error remains and that no fallback service or route is invented.

## Planned operating modes

### One-time import

Read native files, emit an import report, and write a new canonical OxiRoute file only
after explicit confirmation. Native files remain untouched.

### Watched import

Watch native source directories, resolve the complete include graph after a change, build a
candidate, and activate only when the whole import validates. The UI shows native disk,
canonical candidate, and active revisions separately.

UI edits in watched mode write an OxiRoute overlay or require conversion to canonical-only
ownership. They MUST NOT rewrite native files because comments, modules, and unsupported
semantics cannot be safely round-tripped.

## Target common subset

- TCP bind addresses and ports.
- HTTP and HTTPS virtual hosts with exact names.
- Exact and prefix paths.
- Static HTTP upstream endpoints.
- Explicit URI-prefix replacement.
- Equal-weight round robin.
- Certificate, private-key, and bundle paths.
- Frontend and backend TLS on/off.
- Static timeouts whose scope is unambiguous.
- nginx stream TCP/UDP and HAProxy TCP as product-specific L4 features.

## nginx

The public `import_http_fragment` entry point is intentionally not a complete native-config API.
Target support beyond its current strict subset includes:

- `http`, `server`, `listen`, `server_name`, ordinary prefix/exact `location`, static `proxy_pass`.
- `upstream` with static `server` entries.
- Basic certificate/key/protocol directives.
- `stream` TCP and UDP listeners with static upstreams.
- `include`, including deterministic glob expansion.

Blockers:

- Variables in destinations, `if`, `rewrite`, `map`, scripting, and dynamic resolution.
- Regex and named locations.
- Module-specific auth, caching, rate, WAF, or embedded-language behavior.
- Build-dependent directives when the capability profile is unavailable.

nginx location precedence and `proxy_pass` URI replacement MUST be translated explicitly;
source order alone is incorrect.

### nginx-RTMP strict subset

The separate `import_rtmp` entry point can finalize only an error-free graph containing one `rtmp`
block, non-overlapping IP socket listeners without options, servers, uniquely named applications,
and the inheritable `live`, `idle_streams`, and exact recording policy below:

- `record off`, `record all`, or `record all manual`/`record manual all`.
- Recording only on `live on` applications and only with a secure absolute `record_path`.
- Default `.flv` or a separator-free, at-most-128-byte `record_suffix` containing literals and
  `%%`; nginx local-time calendar formats do not lower to canonical UTC formats.
- `record_unique on|off` and a continuous-only `record_interval` of 1 through 2147483647 ms.
- Canonical recorder queue/shutdown/storage defaults where nginx has no exact equivalent.

The importer resolves these scalar policies across `rtmp`, `server`, `application`, and include
boundaries without opening `record_path`. It emits canonical listener/service/application/recorder
provenance. Any relevant blocking error prevents finalization; safe servers may remain in the draft,
but no placeholder is invented for a blocked server.

Blocking forms include listen options, overlapping sockets, duplicate scalar/application identities,
missing or insecure paths, recording without `live on`, bare `record manual`, partial
audio/video/keyframe masks, manual intervals, local-time suffix formats, `record_append`,
`record_lock`, size/frame/notify policy, named `recorder {}` blocks, global RTMP policy, access,
push/pull, callbacks, exec, VOD, HLS/DASH, logs, stats, and native control behavior. This strict
subset is not full nginx-RTMP compatibility and is not an audited-host claim unless the authoritative
coverage manifest maps an audited fixture.

## HAProxy

Target support beyond the current strict TCP slice:

- `global` values required to interpret supported sections.
- `defaults`, `frontend`, `backend`, and `listen`.
- `bind`, `mode http`, `mode tcp`, `default_backend`.
- Simple ordered `use_backend` rules using exact host or path-prefix ACLs.
- `balance roundrobin` and static-TCP `leastconn`; broader HTTP accounting remains a blocker.
- Static socket, DNS, and Unix `server` endpoints and basic frontend/backend TLS where the canonical
  transport has equivalent semantics.

Blockers:

- Generic UDP, arbitrary sample expressions, maps, stick tables, Lua, and dynamic servers.
- Complex health checks, runtime server state, SPOE, and unsupported QUIC modes.

HAProxy `-f` file/directory ordering and named `defaults from` inheritance MUST be retained.

## Apache httpd

Target support:

- `Listen`, `<VirtualHost>`, `ServerName`, and exact `ServerAlias`.
- `SSLEngine`, certificate file, and key file directives.
- Static `ProxyPass`, `ProxyPassReverse`, `balancer://`, and `BalancerMember`.
- Equal-weight `byrequests`.
- `Include` and `IncludeOptional` ordering.

Blockers:

- `ProxyPassMatch`, `RewriteRule [P]`, complex `<Location>`/directory merges, and balancer-manager state.
- Generic TCP/UDP because stock httpd does not provide equivalent listeners.

The first matching `ProxyPass` behavior MUST not be converted into nginx-style longest
prefix behavior.

## Squid

The first subset follows the explicit-forward-proxy milestone:

- Listener/port declarations required for explicit HTTP proxying.
- Direct upstream mode.
- A documented subset of source, destination, method, and port ACLs.
- Ordered allow/deny access rules.
- Basic static authentication only when semantics match.
- Access-log destination and basic timeout/limit settings.

Caching, helpers, adaptation, interception, peers, SSL bump, and delay pools remain blocking
until their independent implementations pass compatibility tests.

## Validation

When native binaries are available, the importer SHOULD optionally run their read-only
validators (`nginx -t`, `haproxy -c`, `httpd -t`) before translation. Native validation is
additional evidence, not proof that translated behavior is equivalent.

The planned complete corpus groups fixtures by product and category: `valid`, `invalid`,
`unsupported`, and `edge`. Each fixture will record its expected include graph, canonical model,
diagnostics, and optional native-validator output. Current nginx, HAProxy, Squid, and Varnish
fixtures live under `crates/oxiroute-import/tests/fixtures/<product>/`.
