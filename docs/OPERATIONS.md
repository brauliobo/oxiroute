# Operations

## CLI

`oxiroute serve CONFIG` starts the daemon. A lone positional `CONFIG` remains accepted for
existing invocations. Offline commands are:

```sh
oxiroute serve /etc/oxiroute/oxiroute.kdl
oxiroute config check /etc/oxiroute/oxiroute.kdl
oxiroute import nginx /etc/nginx/nginx.conf --root-prefix / --output report
oxiroute import nginx /etc/nginx/nginx.conf --root-prefix / --host-timezone America/Bahia --default-access-log-file /var/lib/oxiroute/http-access.jsonl --recording-root /mnt/cloud/4tb/cam-rtmp --default-error-server nginx/1.30.2 --format kdl --output preview
oxiroute import haproxy /etc/haproxy/haproxy.cfg --output preview
oxiroute import haproxy /etc/haproxy/haproxy.cfg --node-ip 10.0.0.15 --gpu1-defined --output preview
oxiroute import haproxy /etc/haproxy/haproxy.cfg --node-ip 10.0.0.15 --gpu1-defined --shadow-port-offset 10000 --output preview
oxiroute config compose edge.kdl legacy.lua openwrt.uci site.conf
oxiroute config compose --format hocon edge.kdl legacy.lua
oxiroute version
```

Import `report` output includes diagnostics and unresolved deployment/activation requirements.
`preview` succeeds only for a finalized canonical candidate and writes deterministic KDL by default;
`--format kdl|lua|uci|hocon` selects another rendering.
HAProxy files using `${NODE_IP}` or `defined(GPU1)` require the corresponding explicit
`--node-ip` and `--gpu1-defined` preprocessing inputs; the importer fingerprints those values.
HAProxy `log`/`httplog` and user, group, chroot, daemon, PID, process, and thread directives are
reported as deployment warnings. A finalized preview does not mean those settings were implemented;
the target service unit/container and logging pipeline MUST reproduce the required ownership,
isolation, process topology, and log handling before cutover.
`--shadow-port-offset` is preview-only and shifts imported IP socket listener ports after exact
lowering, then revalidates the complete canonical configuration for side-by-side canaries.
`--recording-root` replaces exactly one native nginx recording root with an explicit canonical
no-symlink path and fails closed when the source has zero or multiple recording roots.
`config compose` loads finalized inputs of any supported syntax in order and rejects conflicting
process-wide settings, duplicate names, and invalid cross-references. It emits deterministic KDL by
default; `--format kdl|lua|uci|hocon` selects another output adapter. This output is a flattened typed
configuration: templates and native references are resolved rather than copied. Run `config check`
on the composed file in the target host environment to verify runtime preparation and referenced
files before activation.

The standalone `import nginx|haproxy --output preview` commands accept only fully finalized native
candidates and emit deterministic KDL by default. `--format kdl|lua|uci|hocon` selects preview
syntax. The flag does not change `--output report`; report output remains importer evidence.

`serve` defaults to `oxiroute.kdl`. Extensionless paths and `.kdl`/`.kdl2` use KDL 2.0; `.lua` uses
restricted Lua, `.uci` uses OpenWrt UCI, and `.hocon`/`.conf` use HOCON.

## Generations

Candidate preparation validates and compiles routes, pools, TLS identities, static roots, access
secrets, recording stores, health supervisors, relay policies, management assets, stats credentials,
and all listener reservations before publication. It does not write the canonical file. Matching
listener reservations are process-owned and reused by later generations.

Activation swaps active and previous generation references under one lock, closes the old logical
accept gate, and retains the previous generation for explicit rollback. HTTP/1, HTTP/2, WebSocket,
TCP, UDP pseudo-sessions, QUIC/H3 connections, and RTMP references are counted independently and
drained under a caller-supplied deadline. Failed candidates are dropped without changing active
state. Canonical management writes remain revision-checked and invalid drafts do not alter disk.

The watcher observes the parent directory so rename-based replacement is visible, debounces event
bursts, and periodically reconciles exact SHA-256 disk revisions. Invalid snapshots are rejected and
do not replace active generation state. Operational logs on the `oxiroute::operations` target are
structured JSON and never include source text, secret values, configured paths, or OS error strings.

For compositional KDL/HOCON/UCI roots, periodic reconciliation re-resolves templates and native
references even when the root bytes are unchanged. A native change can therefore produce a new
candidate revision while `diskRevision` remains unchanged. Native import failure diagnostics crossing
the management boundary contain stable code counts, not source text, paths, or native diagnostic
messages.

Configuration sources are privileged administrator input. HOCON includes and process-environment
fallbacks are disabled, UCI is parsed as data without shell execution, and Lua has no standard
libraries. Native references are the deliberate exception to no-I/O parsing: they read the named
nginx/HAProxy roots and their importer-defined include graphs with the daemon account's filesystem
permissions. They do not execute native binaries, shell expansions, or infer process environment.

The supervised launcher transfers authenticated typed listener descriptors across the master/worker
boundary for TCP, Unix, UDP, and QUIC/H3 listeners. It does not claim arbitrary inherited-file-
descriptor upgrade; unsupported descriptor topologies remain on the direct runtime, and listener
reuse during supervised replacement requires an unchanged typed manifest. During replacement, the
old generation stops new admissions before the candidate activates. Existing UDP pseudo-sessions
and H3 requests remain owned by the retired worker; H3 sends GOAWAY and rejects later requests,
while UDP keeps the existing session on the retired generation and admits new sessions only after
the candidate commits. A rejected candidate leaves the active worker and its listener ownership
unchanged.

## Shutdown

Pingora is configured with a 3-second connection grace period followed by a 2-second runtime
shutdown deadline. SIGTERM requests graceful shutdown; packaging uses a 15-second systemd stop
deadline. Tests send SIGTERM and reserve force-kill only as a bounded test cleanup fallback.

Automatic ACME cancellation is cooperative. DNS resolution, connect, TLS handshake, socket I/O,
and polling/sleep loops check cancellation within their configured scheduler interval (at most 50
ms for network and poll waits). Local filesystem/state/fsync and OpenSSL calls, and arbitrary
in-process DNS-provider calls, cannot be preempted; they check before/after when control returns and
providers receive the operation context for cooperative checks. The generation process still applies
the existing five-second orchestration deadline and may detach its generation thread. If a local or
provider call remains blocked, ACME authority and resources are released eventually after it returns;
ACME creates no hidden detached helper. Cancellation before confirmed DNS cleanup leaves the durable
pending journal for recovery, while confirmed cleanup removes it exactly once.

## Readiness

`GET /ready` returns 200 only when an active non-degraded generation exists and all configured
traffic listeners report `listening`; otherwise it returns 503. `GET /api/v1/status` reports the
build version, disk/candidate/active/previous revisions, active-generation age, listener states,
component states, certificate expiry data, and audit component degradation. Process/host sampling
is healthy on Linux x86_64 and aarch64; other platforms keep status available with explicit
`unsupported` process/host component states and null unavailable samples.

Every recognized `/api/v1` or `/api/v2` route, including monitoring, topology, RTMP, listener/pool/server,
generation, TLS, process, configuration, audit, and event routes, requires exactly one management Bearer
token. The only public recognized API probes are exact `GET /ready` and `GET /metrics`. Event
operations preserve their shipped v1 polling/SSE contract at `/api/v1/events` and
`/api/v1/events/stream`. The corrected contract is at `/api/v2/events` and
`/api/v2/events/stream`; both versions also negotiate SSE on their page path with
`Accept: text/event-stream`.
There is no unbounded event queue. Durable audit history is separate from the event ring and is
available through authenticated `/api/v1/audit` and `/api/v1/audit/status`.

`GET /metrics` exports process/host sampling state and values, active-generation age, listener,
pool, server, queue, health, retry, certificate, RTMP relay/recording, generation, and bounded
audit persistence families in Prometheus text format. Unsupported process/host samples are
omitted rather than exported as zeroes.
`/stats` provides a compact
read-only HAProxy-oriented pool/server view. `/metrics` and `/ready` are public on a configured
statistics bind; `/stats` and `/api/v1/status` require a loopback peer and the statistics Bearer
token.

Each canonical `stats.pages[]` socket is separate from those observability binds. It serves only the
configured public HAProxy-compatible `uri_prefix` and returns `404` for `/metrics`, `/ready`,
`/stats`, and `/api/v1/status` unless one is itself the configured prefix. `admin = "localhost"`
adds Ready/Drain/Maintenance forms only to loopback clients. Mutation additionally requires a
`localhost` or loopback-IP Host, a matching Origin or Origin-absent Referer, no forwarded identity,
and the active generation revision. Each page enforces its own connection cap and downstream
timeouts and appears in listener metrics. HEAD preserves GET `Content-Length` without a body; use
`disabled` for an unconditionally read-only page.

Changing the mode of an active Unix listener is not a live reload, even when the candidate contains
other changes. The configuration API saves the valid candidate as `saved_restart_required` with
`restartRequired = true`, leaves the complete active generation and socket mode untouched, and
applies the saved candidate when the process is restarted. Unix listeners retain an exclusive
`<socket>.oxiroute.lock` ownership marker so abnormal termination can safely reclaim unchanged
stale sockets that reject connection attempts. Permission-denied sockets fail closed because their
liveness cannot be proven.
In supervised mode, any incompatible listener/control-listener descriptor topology also produces
`saved_restart_required`; this includes identity, order, role, transport kind, bind, Unix mode,
protocol, or count changes across traffic, management, statistics, statistics-page, UDP, and HTTP/3
listeners. The `I_RESTART_REQUIRED` diagnostic uses `/config/listeners` and explains whether the
cause is the direct Unix mode case or supervised topology, while API status/outcome fields and
audit/event behavior remain unchanged.
The socket directory must be owned by the effective service user or be sticky, and no path ancestor
may be group/world writable unless it has the sticky bit.
