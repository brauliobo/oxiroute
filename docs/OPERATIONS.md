# Operations

Operator procedures for pools, drains, rollbacks and outages are in the
[runbook](user/RUNBOOK.md).

## CLI

`oxiroute serve CONFIG` starts the daemon. A lone positional `CONFIG` remains accepted for
existing invocations. Offline commands are:

```sh
oxiroute serve /etc/oxiroute/oxiroute.kdl
oxiroute config check /etc/oxiroute/oxiroute.kdl
oxiroute import nginx /etc/nginx/nginx.conf --root-prefix / --output report
oxiroute import nginx /etc/nginx/nginx.conf --root-prefix / --host-timezone America/Argentina/Buenos_Aires --default-access-log-file /var/lib/oxiroute/http-access.jsonl --recording-root /mnt/cloud/4tb/cam-rtmp --default-error-server nginx/1.30.2 --format kdl --output preview
oxiroute import haproxy /etc/haproxy/haproxy.cfg --output preview
oxiroute import haproxy /etc/haproxy/haproxy.cfg --node-ip 192.0.2.37 --gpu1-defined --output preview
oxiroute import haproxy /etc/haproxy/haproxy.cfg --node-ip 192.0.2.37 --gpu1-defined --shadow-port-offset 10000 --output preview
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
accept gate, and retains the previous generation for explicit rollback. Before the new generation
accepts, the runtime administrative state of the old one (drain, maintenance, health override,
check switch, max-connections and weight overrides) is copied to the servers with the same pool and server
name, so a drained server never takes traffic across a file write, reload, apply or rollback.
`evacuate` sets the same `drain` state; its one-shot cancel of in-flight requests is never carried over.
Servers that no longer exist lose their state and new servers start ready. In supervised mode the
old worker reports that state at quiesce and the master hands it to the replacement at activation;
a state too large for the control channel (over 48 KiB, hundreds of non-default servers) refuses
the replacement and the old worker keeps serving. HTTP/1, HTTP/2, WebSocket,
TCP, UDP pseudo-sessions, QUIC/H3 connections, and RTMP references are counted independently and
drained under a caller-supplied deadline. Failed candidates are dropped without changing active
state. Canonical management writes remain revision-checked and invalid drafts do not alter disk.

The watcher observes the parent directory so rename-based replacement is visible, debounces event
bursts, and periodically reconciles exact SHA-256 disk revisions. Invalid snapshots are rejected and
do not replace active generation state: each one is logged at error level on the `oxiroute::config`
target (once per distinct rejection, not at every reconciliation), emits a `configuration_reload`
event with the outcome `rejected`, raises `oxiroute_config_rejections_total` and is shown as
`lastRejection` in `generation status` and `GET /api/v1/generations` until a later file is accepted,
in the plain and the supervised runtime. The master reports a rejection to the active worker, so
`lastRejection` also covers a reload the master refused (unsupported configuration, a listener that
cannot be bound) or whose replacement worker failed. `degraded` is true while a rejection is
unresolved; `GET /ready` stays 200 because the active generation keeps serving. `config check`, `422` responses
and startup failures name the failing setting (`path`, `detail`, `line`). Operational logs on
the `oxiroute::operations` target are
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
token. The only public recognized API probes are exact `GET /ready`, `GET /metrics` and `GET /status.json`. Event
operations preserve their shipped v1 polling/SSE contract at `/api/v1/events` and
`/api/v1/events/stream`. The corrected contract is at `/api/v2/events` and
`/api/v2/events/stream`; both versions also negotiate SSE on their page path with
`Accept: text/event-stream`.
There is no unbounded event queue. Durable audit history is separate from the event ring and is
available through authenticated `/api/v1/audit` and `/api/v1/audit/status`.

`GET /metrics` exports process/host sampling state and values, active-generation age, listener,
pool (including fail-open selections), server, queue, health, retry, certificate, RTMP
relay/recording, generation, and bounded audit persistence families in Prometheus text format.
Unsupported process/host samples are omitted rather than exported as zeroes.
`/stats` provides a compact
read-only HAProxy-oriented pool/server view.

`GET /status.json` is a public, secret-free summary for dashboards and the future `oxiroute check`:
`state` (`ok`, `warning`, `critical`), the generation age and whether the last configuration file
was refused, one entry per pool (`state`, `available`, `total`, `ejected`, `failOpen`, `queued`,
`oldestQueueWaitMs`, `inflight`, `oldestInflightMs`), and the number of overdue and stuck requests.
A pool with no available server is critical; a pool with a server missing, or whose oldest queued
request has waited over five seconds, is a warning; an overdue request makes the whole status
critical.

### Request tracing and in-flight requests

`oxiroute requests list` (`GET /api/v1/requests`) shows every request currently inside the router
with its request id, route, pool, server, phase, age and attempts, so a request that is lost can be
told from one that never arrived. The same id is in the access log (`requestId`) and in the
`x-request-id` header of the response and of every backend attempt. A request that is quiet for 60
seconds is logged once as stuck and counted in `oxiroute_http_stuck_requests_total{pool,phase}`.

Each `server evacuate` or `pool evacuate` is an event (`server_evacuation` with `pool`, `server` and the counts of
moved, forced and left-running requests) and an audit entry; version 1 of the events API reports it as `unknown`
and a request ended by `--force` is logged with the failure reason `evacuated`.

Every change of a server's health state reported by its active check is an event
(`upstream_endpoint_health`, outcome `health_changed` with `pool`, `server`, `previous`, `next`, the
failure `reason` when the server went down and `changedAtUnixMs`), next to the existing
`upstream_endpoint_ejection` and `upstream_endpoint_recovery` events for passive ejection. Version 1
of the events API reports it with the event name `unknown`; read it from `/api/v2/events` or `/api/v2/events/stream`.

| Metric | Labels | Meaning |
| --- | --- | --- |
| `oxiroute_http_inflight_requests` | `pool` | requests inside the router |
| `oxiroute_http_inflight_oldest_age_seconds` | `pool` | age of the oldest one |
| `oxiroute_http_inflight_overdue` | none | requests older than their deadline plus 5 s; should always be 0 |
| `oxiroute_http_stuck_requests_total` | `pool`, `phase` | requests quiet for 60 s, counted once |
| `oxiroute_pool_queue_depth` | `pool` | requests waiting for capacity on every wait path |
| `oxiroute_pool_queue_oldest_wait_seconds` | `pool` | wait of the oldest queued request |
| `oxiroute_pool_queue_wait_milliseconds` | `pool` | histogram of time spent waiting, 1 ms to 600 s |
| `oxiroute_server_responses_total` | `pool`, `server`, `class` | backend responses by `1xx`..`5xx` or `none` |
| `oxiroute_server_attempts_total` | `pool`, `server`, `outcome` | `ok`, `connect_failed`, `timeout`, `protocol_error`, `status_retry`, `rescued`, `client_closed`, `evacuated` |
| `oxiroute_evacuated_requests_total` | `pool`, `server`, `result` | requests an `evacuate` touched: `redispatched`, `left_running`, `forced`, `failed` (cancelled, then could not be served) |
| `oxiroute_server_request_duration_milliseconds` | `pool`, `server` | histogram, request sent to last byte |
| `oxiroute_server_header_duration_milliseconds` | `pool`, `server` | histogram, request sent to first response byte |
| `oxiroute_server_bytes_sent_total`, `oxiroute_server_bytes_received_total` | `pool`, `server` | bytes towards and from the backend |
| `oxiroute_server_max_connections` | `pool`, `server` | effective connection limit |
| `oxiroute_generation_info` | `generation`, `revision` | value 1 for the serving generation |

The per-server histograms are exported only while at most 256 servers are configured; above that
only the counters are. Labels are limited to configured names, never a path, client or request id.

Alerts worth having: `oxiroute_http_inflight_overdue > 0`, `oxiroute_pool_available_servers == 0`,
`histogram_quantile(0.95, rate(oxiroute_pool_queue_wait_milliseconds_bucket[5m]))` above the callers'
patience, and `increase(oxiroute_http_stuck_requests_total[10m]) > 0`. `/metrics` and `/ready` are public on a configured
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
Every other listener change is a live reload in both runtimes, including adding, removing, moving
and re-protocoling traffic, management, statistics, statistics-page, UDP, and HTTP/3 listeners, and
changing a listener's TLS profile. Since 0.5.3 a supervised reload no longer retains the old
revision for a listener change.

### Server registration

Pools with `registration` take servers at runtime. The registry keeps them in
`registry.state_dir/registrations.json` (atomic writes, a `.bak` copy, an exclusive lock); a lease is
an in-memory deadline, so every registration gets a full TTL when a process starts and heartbeats
write nothing. In supervised mode the active worker persists and expires; every worker, including
a draining one, follows the store, and a retired worker answers writes with `409 generation_inactive`
and closes the connection so clients reconnect. The group sockets are bound privately and renamed
into place, so a replacement worker takes over a path atomically; keep their directory owned by
the service user (a setgid runtime directory gives the group). If the store cannot be written the
registry reports `registry_unavailable`, keeps serving the members it has and retries.
`oxiroute_registry_*` metrics cover members by state, operations, expiries, refusals and the lease
time left of each server, and joins, changes, departures and expiries are `server_update` events.

### Listener changes on reload

A reload compares the sockets of the running revision with those of the new one. A socket is
identified by its transport and bind address (and, for a Unix socket, its path and mode), not by the
listener's name, position or protocol.

- **Kept**: the same bind address. The socket is never closed or re-bound, so connections already
  queued on it are accepted by the new revision. A kept listener may change its protocol, service,
  TLS profile, limits and timeouts at the same time; the new revision applies them to every new
  connection while connections accepted earlier finish under the old revision.
- **Added**: a new bind address. It is bound before any traffic moves to the new revision.
- **Removed**: a bind address the new revision no longer lists. Once the new revision is active the
  socket stops accepting (new connections are refused and the address is free to bind again) while
  the requests, streams, WebSocket and relay connections it already accepted keep running in the
  old worker until they finish.
- **Moved**: a changed address is an add plus a remove.

If the new revision lists an address that cannot be bound (in use, permission denied, or a Unix
path owned by another process) the reload is rejected as a whole. The running revision, its
listeners and its connections are untouched, the master logs one warning that names the listener
(`supervised master kept revision ... because the configuration reload could not bind its
listeners`), and the same revision is tried again at the next reconciliation (at most 30 seconds
later, or immediately on `SIGHUP`, which is what `systemctl reload oxiroute` sends). The configuration API reports
such an address when it validates the candidate, so a failed bind normally surfaces before the file
is written.

The configuration API describes the effect of a change before and after saving through the
`I_LISTENER_CHANGES` diagnostic (path `/config/listeners`), which lists the `added`, `removed` and
`kept` listeners as `name (transport address)`.

A replaced generation (a worker in supervised mode) closes its idle HTTP/1 keep-alive connections
when the next revision is applied, so an idle client never holds up an apply. A request still in
progress finishes, its response carries `Connection: close`, and the client reconnects to the new
generation. Other work (a long request, a WebSocket, a CONNECT tunnel, an L4 relay, an RTMP session,
an HTTP/3 request) continues until it ends or the generation's drain deadline passes, then the
generation is closed. The deadline is, in order, `retirement.drain_deadline_ms` of the replaced
generation's own configuration, the deprecated `OXIROUTE_RELOAD_DRAIN_SECONDS` (1 to 86400) in
`/etc/oxiroute/oxiroute.env`, or the larger of 3 hours and the longest HTTP
`upstream_io_timeout_ms`; an HTTP, forward proxy or L4 service `drain_deadline_ms` only raises it.
`retirement.max_retired_generations` (default 4) bounds how many replaced generations drain at
once in the direct runtime: a further apply stays prepared (`pendingApply` with reason
`retired_cap` in `generation status`, a `generation_retire` event with outcome `cap_reached`) until
a slot frees. In supervised mode one replaced worker drains at a time, whatever the setting, so a
second reload requested while a long request keeps it alive waits for it (the master applies the
newest saved revision afterwards without further action). At the deadline the retired worker
closes its generation and logs a `generation_retire` event with outcome `forced_close`; the master
kills the worker 10 seconds later if it is still there. `generation status` and
`GET /api/v1/generations` list the draining generations as `retiredGenerations` (revision, state,
age, seconds before the deadline, in-flight work by protocol). Under the supervising master the
retired worker is listed by the active worker from what the master reports once a second, and its
in-flight work is the connection total (`inFlightByKind` has a single `connections` entry). The
metrics `oxiroute_generation_retired`, `oxiroute_generation_retired_age_seconds`,
`oxiroute_generation_in_flight{role}` and `oxiroute_generation_forced_close_total{reason}` count
them. Stopping the service (`TimeoutStopSec=15s`) still ends long jobs.

One case still needs a process restart: a Unix socket that keeps its path and changes its mode,
because the new socket would need the path the running one owns.
The socket directory must be owned by the effective service user or be sticky, and no path ancestor
may be group/world writable unless it has the sticky bit.
