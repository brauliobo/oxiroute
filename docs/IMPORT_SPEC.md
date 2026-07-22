# Native configuration import specification

## Contract

Import means parsing and translating a documented subset into the canonical model. It does
not mean loading foreign modules or reproducing undocumented implementation accidents.

Every import result includes:

- Parsed source files and include graph.
- Source product and detected version/capability profile.
- Canonical candidate objects with provenance.
- Blocking errors and non-blocking warnings.
- Unsupported directives grouped by affected service.
- A compatibility summary suitable for CLI, API, and UI display.

The daemon MUST NOT activate an affected service if a directive that changes its routing,
security, TLS, listener, or upstream semantics cannot be represented.

## Operating modes

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

## Initial common subset

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

Initial support:

- `http`, `server`, `listen`, `server_name`, ordinary prefix/exact `location`, static `proxy_pass`.
- `upstream` with static `server` entries.
- Basic certificate/key/protocol directives.
- `stream` TCP and UDP listeners with static upstreams.
- `include`, including deterministic glob expansion.

Initial blockers:

- Variables in destinations, `if`, `rewrite`, `map`, scripting, and dynamic resolution.
- Regex and named locations.
- Module-specific auth, caching, rate, WAF, or embedded-language behavior.
- Build-dependent directives when the capability profile is unavailable.

nginx location precedence and `proxy_pass` URI replacement MUST be translated explicitly;
source order alone is incorrect.

## HAProxy

Initial support:

- `global` values required to interpret supported sections.
- `defaults`, `frontend`, `backend`, and `listen`.
- `bind`, `mode http`, `mode tcp`, `default_backend`.
- Simple ordered `use_backend` rules using exact host or path-prefix ACLs.
- `balance roundrobin`, then `leastconn`.
- Static `server` endpoints and basic frontend/backend TLS.

Initial blockers:

- Generic UDP, arbitrary sample expressions, maps, stick tables, Lua, and dynamic servers.
- Complex health checks, runtime server state, SPOE, and unsupported QUIC modes.

HAProxy `-f` file/directory ordering and named `defaults from` inheritance MUST be retained.

## Apache httpd

Initial support:

- `Listen`, `<VirtualHost>`, `ServerName`, and exact `ServerAlias`.
- `SSLEngine`, certificate file, and key file directives.
- Static `ProxyPass`, `ProxyPassReverse`, `balancer://`, and `BalancerMember`.
- Equal-weight `byrequests`.
- `Include` and `IncludeOptional` ordering.

Initial blockers:

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

Fixtures live by product and category: `valid`, `invalid`, `unsupported`, and `edge`.
Each fixture records expected include graph, canonical model, diagnostics, and optional
native-validator output.
