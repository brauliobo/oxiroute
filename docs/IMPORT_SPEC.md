# Native configuration import specification

## Contract

Import means parsing and translating a documented subset into the canonical model. It does
not mean loading foreign modules or reproducing undocumented implementation accidents. Every
offline report uses one stable JSON schema with the source product, detected version when available
(otherwise explicit `null`), capability profile, bounded source table, include graph, source fingerprints,
candidate provenance, requirements, blockers, overlays, and ordered diagnostics.

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

Import is implemented in `oxiroute-import` and exposed by the daemon binary as an offline report or
preview command. There is no import management API or UI workflow; standalone preview is emitted
only for a fully finalized candidate, defaults to deterministic KDL, and accepts
`--format kdl|lua|uci|hocon`. The complete nginx root command includes the strict nginx-RTMP
subset when that root contains an `rtmp` block.

KDL, HOCON, and UCI roots may instead contain `nginx_server`, `haproxy_server`, `squid_server`, `apache_server`, and `varnish_server` references. The
normal source resolver runs those references through the same complete import pipelines, composes
only fully finalized candidates with inline canonical objects in source order, and lets the daemon
watch/reconcile the effective result while retaining successful native report provenance in the
resolved candidate metadata. This is runtime integration for strict source references, not general
activation of standalone import reports. Restricted Lua cannot declare native references.

- nginx exposes an explicit HTTP-fragment importer: the expanded root may contain only one `http`
  block. It has bounded byte-preserving parsing, deterministic includes/globs, provenance, HTTP
  inheritance, an occurrence ledger, blocked-service reports, and conditional canonical lowering.
  It rejects complete nginx files rather than implying support for `events`, process, or module roots.
- nginx also exposes `import_root`, which loads one complete include graph, resolves HTTP and RTMP
  from that same snapshot, records one root-level terminal decision per expanded occurrence, and
  retains process, events, module, and error-log directives as typed deployment requirements.
  Certificate/key and htpasswd paths plus explicit bearer/upstream-TLS options become value-bearing
  operational overlays; every supplied option must have one unique normalized identity and be used
  by lowering before finalization. Import does not open private keys or secret files.
- The strict nginx HTTP subset carries nginx's first-loaded accepted exact and wildcard name claims
  into lowering, including the exact-plus-wildcard claims of a leading-dot name. It can finalize
  exact/default virtual hosts with representable flat
  routes, fixed responses or redirects, static socket/DNS/Unix origins, explicit proxy defaults,
  exact response-control suppression, response hide/pass defaults, and shared named upstream pools.
  URI replacement and incompatible routing/admission policy remain blocking;
  a blocked service is retained only in the report draft.
- nginx-RTMP has an independent 117-directive parser/registry plus deterministic include expansion,
  effective inheritance, a terminal occurrence ledger, provenance, blocked-service reporting, and
  strict conditional canonical finalization for an exact listener/application/recording subset,
  including outbound chunk size, disabled access logging, bounded suffixes tied to an explicit
  host IANA timezone overlay, static RTMP push targets whose application is literal or `$name`,
  and supported named `recorder` blocks. Import never accesses a configured recording root. Most
  directive families remain blocking. The complete nginx-root path composes the finalized HTTP and
  RTMP candidates, and KDL/HOCON/UCI `nginx_server` references feed that result into the normal
  canonical resolver and watcher-driven generation path. There is no separate management import
  API, import UI, or dedicated `import rtmp` daemon command.
- HAProxy has ordered `-f` file/directory loading, byte-preserving lexing/parsing, defaults and
  frontend/backend/listen resolution, a terminal decision ledger, stable diagnostics, provenance,
  and conservative canonical lowering.
- The offline HAProxy CLI accepts explicit `--node-ip` and `--gpu1-defined` preprocessing inputs;
  it never reads or infers the native service environment.
- HAProxy finalizes only complete, error-free candidates. An explicit preprocessing API accepts a
  typed node IP and GPU-presence bit, expands only `${NODE_IP}` and `defined(GPU1)`, fingerprints
  those inputs, retains immutable original source snapshots and inactive spans, and emits a source
  map from generated spans back to the covering original bytes. The finalizable subset covers HTTP and TCP,
  aggregate/listener/server admission, socket/DNS/Unix endpoints, `roundrobin`, `leastconn`, and
  `first`, reusable HTTP least-connections, healthy-startup checks with exact timeout preservation,
  one literal `http-check send` GET request with a canonical URI, HTTP/1.0 or HTTP/1.1, and at most one
  literal Host header,
  bounded retries with the safe `retry-on conn-failure/conn-refused` subset and bare final
  redispatch, independent timeout scopes including upstream queue deadlines, source-CIDR `forwardfor`
  exceptions, cumulative inherited `default-server` health intervals/capacity and passive observation,
  thresholds, and mark-down policy, and server-close connection reuse. A backend-scoped, uniquely consumed
  one-request-per-connection overlay can provide the same lifecycle boundary when captured
  surrounding evidence, such as hostrouter's nginx HTTP/1.0/no-keepalive hop, establishes it. DNS
  names remain canonical and are not resolved during import. A positive `use_backend` condition may
  combine exactly one exact `hdr(host)` ACL, optionally with `-i`, and one case-sensitive `path_beg` ACL;
  the conjunction lowers to one canonical route with both selectors and retains both ACL references.
- The strict HTTP subset also preserves HAProxy's default three connection retries, exact
  `path -i` ACLs used by conditional fixed health responses; those lower to an ASCII case-insensitive
  exact canonical path selector ahead of the proxy fallback, and `hdr(host) -i` as an ASCII
  case-insensitive exact authority without port widening. Dynamic sample expressions, `unless`,
  negated ACLs, duplicate Host/path criteria, unsupported ACL criteria, and conjunctions wider than
  that exact two-criterion family block the affected candidate. A conditional backend with no native
  default receives one final fixed `503` catch-all so unmatched requests do not acquire a fabricated
  upstream destination. `unix@` listener binds retain the Unix path and explicit mode.
- The synthetic `hostrouter-static-representable.cfg` fixture proves that an audited-shaped Unix
  frontend plus DNS `leastconn` backend can finalize; it is not live-host evidence. Live sanitized
  fixture trees are mapped in `coverage/host-cases.json` as live-origin hashed/read-only captured
  evidence. Their metadata separates direct origin hashes and exact hash commands from checked-in
  post-sanitization per-file hashes, records the sanitizer steps, and states that raw bytes were not
  stored. This is not cryptographic signer authentication. Logging and process policy remain typed
  deployment warnings that an operator MUST reproduce outside OxiRoute; import does not implement
  HAProxy syslog formats, user/chroot/daemon, or worker topology. A dedicated HTTP frontend/listen
  containing a supported `stats uri` (which implicitly enables HAProxy stats), refresh, optional
  `stats enable`, and optional `stats admin if LOCALHOST` shape lowers to an independent canonical
  page and no ordinary HTTP service. Effective frontend/bind `maxconn` and client, HTTP-request, and
  HTTP-keep-alive timeouts lower to page admission/downstream policy. Response rules and other active
  unrepresented listener policy fail closed rather than disappearing. Authentication,
  hide/version and other stats forms remain activation requirements, as does exact
  `http-request use-service prometheus-exporter if { path /metrics }`. The exact Prometheus form emits canonical OxiRoute stats
  only when a uniquely matched operator migration overlay explicitly accepts different metric
  families and the broader OxiRoute stats routes. Standalone HAProxy reports identify product
  `haproxy` and capability profile `haproxy-strict`; native version remains null unless the caller
  supplies version evidence, while ordinary imports retain every bounded source snapshot in the
  report source table and preserve generated-to-original provenance when preprocessing is used.
- Squid has a bounded source/include, parser, typed semantic report, and strict canonical lowering
  path for direct authenticated HTTP/1 forwarding. Final recheck verifies source bytes,
  root/include path identities, and include-glob result sets. The resulting candidate is integrated
  with the daemon runtime; cache and refresh behavior is reported as externalized, and native
  activation requires explicit `externalize_cache` acceptance when refresh rules are present.
- Varnish has a bounded VCL source/include graph, parser, typed semantic report, decision ledger,
  invocation model, exact static HTTP/cache lowering, offline report/preview, and
  KDL/HOCON/UCI native-reference integration. The finalized subset covers static network/Unix
  backends, legacy round-robin/fallback directors, listeners, request/response headers, and the
  canonical memory/disk cache timeline. VMODs, dynamic VCL, custom subroutines, invalidation,
  synthetic responses, and mismatched invocation/cache semantics remain blocking.
- Apache httpd has a bounded byte-preserving source/include graph, parser, semantic resolver,
  occurrence ledger, source-aware provenance, and strict canonical lowering for explicit IP or
  bounded wildcard listeners, multi-address virtual hosts, exact host authorities, inherited
  server defaults, static HTTP/HTTPS ProxyPass destinations, equal-weight `balancer://` pools, and
  certificate/key path references. Include and IncludeOptional expansion is byte-sorted,
  deterministic, and rechecked before finalization; a missing optional match is recorded as
  `optional_missing` without becoming an error. Global server directives are merged into each
  virtual host with inherited provenance. Apache's first matching ProxyPass order is retained only
  when it is equivalent to the canonical runtime's longest-prefix selector; unsafe overlaps block
  the candidate. Unsupported rewrites, regex proxying, directory/location merges, module scripts,
  authentication/authorization, ProxyPassReverse response rewriting, dynamic balancer state, and
  unsupported modules block the affected candidate instead of disappearing.

Coverage manifests and importer tests enforce that a candidate cannot finalize while any blocking
error remains and that no fallback service or route is invented.

Native references expose only the options defined by the source adapter. nginx accepts one path and
optional `root_prefix`, `host_timezone`, `default_access_log_file`, `recording_root`, and
`default_error_server`; HAProxy accepts one or more ordered paths plus optional `node_ip` and
`gpu1_defined`; Squid accepts one `path` and optional `externalize_cache`, which must be true when
refresh rules are present; Apache accepts one `path` to a complete httpd root; Varnish accepts one
`path` and an optional ordered `arguments` array containing the explicit varnishd invocation facts.
Relative paths resolve from the OxiRoute source directory.
Shadow listener offsets and other standalone CLI-only overlays are not reference fields.

Referenced files remain administrator-owned and read-only to OxiRoute, but they are intentionally
read using the daemon account's filesystem access. nginx include expansion and HAProxy ordered roots
retain their importer bounds and fail closed. References never invoke nginx, HAProxy, a shell, or
process-environment discovery. Management-facing resolver failures are redacted to stable importer
names and diagnostic code counts; operators use the offline report command for detailed diagnostics.

## Planned operating modes

### One-time import

Read native files, emit an import report, and write a new canonical OxiRoute file only
after explicit confirmation. Native files remain untouched.

### Watched import workflow

The current source-reference path re-resolves the complete importer input after native dependency
events and on a periodic reconciliation interval, and activates only a fully prepared candidate.
Successful resolutions register the exact resolved files and the parent directories needed for literal
includes, include globs, and ordered source roots; the registration set is rebuilt after each successful
resolution so additions, removals, and renames are observed without waiting for the interval. It does
not provide a native-source editor, import report history, or separate native revision in the UI.

Typed API/UI saves reject a compositional root instead of flattening or rewriting it. Operators may
edit the OxiRoute/native sources directly or use `config compose` to create a separate flattened,
canonical-only file. The UI MUST NOT rewrite native files because comments, modules, and unsupported
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

The public `import_http_fragment` entry point intentionally remains fragment-only. Use `import_root`
for a complete process/events/http/rtmp root. Target support beyond the current strict subset includes:

- `http`, `server`, `listen`, `server_name`, ordinary prefix/exact `location`, static `proxy_pass` with URI-prefix replacement, and static root/alias actions.
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
- Default `.flv` or a separator-free, at-most-128-byte `record_suffix` containing bounded calendar
  fields; nginx calendar fields lower with the uniquely consumed host IANA timezone overlay, segment-start time
  basis, and compatible naming.
- `record_unique on|off` and a continuous-only `record_interval` of 1 through 2147483647 ms.
- Named `recorder <name>` blocks whose effective policy is within the same exact subset. Canonical
  recorder names are retained and become independently controllable only when their canonical
  `start` mode is `manual`.
- Canonical recorder queue/shutdown/storage defaults where nginx has no exact equivalent.

The importer resolves these scalar policies across `rtmp`, `server`, `application`, and include
boundaries without opening `record_path`. It emits canonical listener/service/application/recorder
provenance. Any relevant blocking error prevents finalization; safe servers may remain in the draft,
but no placeholder is invented for a blocked server.

Blocking forms include listen options, overlapping sockets, duplicate scalar/application identities,
missing or insecure paths, recording without `live on`, bare `record manual`, partial
audio/video/keyframe masks, manual intervals, unsupported suffix fields, enabled `record_append`,
enabled `record_lock`, nonzero size/frame limits, enabled notify policy, named recorder blocks with
unsupported effective fields, global RTMP policy, access, dynamic push/pull, callbacks, unsupported
exec forms, VOD,
HLS/DASH, file logs, stats, and native control behavior. This strict subset is not full nginx-RTMP
compatibility and is not an audited-host claim unless the authoritative coverage manifest maps an
audited fixture.

## HAProxy

The current strict HTTP/TCP slice includes:

- `global` values required to interpret supported sections.
- `defaults`, `frontend`, `backend`, and `listen`.
- `bind`, `mode http`, `mode tcp`, `default_backend`.
- Simple ordered `use_backend` rules using exact host or path-prefix ACLs, including
  case-insensitive exact Host authority without port normalization and a fixed `503` fallback when
  no default backend exists.
- `balance roundrobin`, `leastconn`, and `first` with canonical connection accounting.
- Static socket, DNS, and Unix `server` endpoints and basic frontend/backend TLS where the canonical
  transport has equivalent semantics.
- Dedicated supported stats pages, `unix@` listener modes, exact health timeout preservation,
  reusable HTTP least-connections, and bare redispatch as delayed same-server retries with a final
  immediate next-server attempt. Only same-server retries wait the imported
  `min(timeout connect, 1s)` delay.

Blockers:

- Generic UDP, arbitrary sample expressions, maps, stick tables, Lua, and dynamic servers.
- Complex health checks beyond the literal `http-check send` request shape, arbitrary check headers or
  bodies, runtime server state, SPOE, and unsupported QUIC modes.
- Redispatch interval arguments, broader ACL expressions, unsupported stats/authentication forms,
  and server-selection options without an equivalent request-lifetime contract.

HAProxy `-f` file/directory ordering and named `defaults from` inheritance MUST be retained.

## Apache httpd

Current strict subset:

- `Listen`, `<VirtualHost>` with explicit IP/port identities, `ServerName`, and exact `ServerAlias`.
- `SSLEngine`, certificate file, and key file directives.
- Static HTTP/HTTPS `ProxyPass`, `balancer://`, and `BalancerMember`.
- Equal-weight `byrequests`.
- `Include` and `IncludeOptional` ordering, including silent missing optional matches.
- Inherited server defaults for exact `ServerName`, `ProxyPreserveHost`, TLS paths, rewrite-off
  state, and static `ProxyPass` rules.
- Audited `LoadModule` deployment requirements.

Blockers:

- `ProxyPassMatch`, `ProxyPassReverse`, rewrites, module scripts, authentication/authorization,
  complex `<Location>`/directory merges, and balancer-manager state.
- Generic TCP/UDP because stock httpd does not provide equivalent listeners.

The first matching `ProxyPass` behavior MUST not be converted into nginx-style longest prefix
behavior. The importer rejects an order whose result would differ from the canonical runtime;
descending-prefix source order is the exact lowered subset.

## Squid

The integrated first subset follows the explicit-forward-proxy milestone:

- Listener/port declarations required for explicit HTTP proxying.
- Direct upstream mode.
- A documented subset of source, destination, method, and port ACLs.
- Ordered allow/deny access rules.
- Basic static authentication only when semantics match.
- Disabled access logging, header privacy, explicit DNS nameservers, and bounded canonical runtime
  defaults.
- Ordered static `cache_peer <host> parent <http-port> 0` entries without options, with bounded
  peer attempts and source-order preservation.
- One global `always_direct allow all` or `never_direct allow all` rule for direct fallback policy.

`oxiroute import squid <root>` emits the same report/preview contract as the nginx and HAProxy
commands. Its report additionally contains a machine-readable `capabilities` registry for Squid
checkout `6f4c814`, including family/directive status, rationale, current evidence, required test
categories, and the explicit `completeParity: false` boundary. KDL, HOCON, and UCI sources can
reference a root through `squid_server`; deterministic previews round-trip through every canonical
format. A native source containing `refresh_pattern` must explicitly set `externalize_cache = true`
before the direct, non-caching candidate can activate.

Basic lowering preserves Squid's case-insensitive username default, explicit `casesensitive`, realm,
helper file, and credential TTL. Other helper settings remain blocking. CONNECT port inference
requires one exact unconditional `deny CONNECT !ports` guard before any rule that could allow
CONNECT; conditional, ranged, reordered, or multiple guards fail closed.

Cache storage and policy directives, helpers, adaptation, interception, sibling/dynamic/credentialed
peer forms, peer access ACLs, SSL bump, delay pools, and legacy protocols remain unsupported or not
planned in the registry and blocking in the importer. Parsed refresh rules may be externalized only
through reviewed CLI import or that explicit native opt-in. A form is `compatible` only when its
runtime and failure-path tests are listed in the registry; parsing or typed classification alone
never promotes a form.

## Validation

When native binaries are available, the importer SHOULD optionally run their read-only
validators (`nginx -t`, `haproxy -c`, `httpd -t`) before translation. Native validation is
additional evidence, not proof that translated behavior is equivalent.

The bounded corpus groups fixtures by product and category: `valid`, `invalid`, `unsupported`, and
`edge`. Each fixture records its expected include graph, canonical model, diagnostics, and optional
native-validator output. Current synthetic nginx, HAProxy, Squid, Apache, and Varnish fixtures live
under `crates/oxiroute-import/tests/fixtures/<product>/`; authenticated sanitized host trees and
metadata live under `crates/oxiroute-import/tests/fixtures/live/<host>/`. Coverage manifests and
focused importer tests remain the evidence boundary; they do not claim complete native parity.
