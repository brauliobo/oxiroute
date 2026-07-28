# Operations

## CLI

`oxiroute serve CONFIG` starts the daemon. A lone positional `CONFIG` remains accepted for
existing invocations. Offline commands are:

```sh
oxiroute config check /etc/oxiroute/oxiroute.lua
oxiroute import nginx /etc/nginx/nginx.conf --root-prefix / --output report
oxiroute import haproxy /etc/haproxy/haproxy.cfg --output preview
oxiroute version
```

Import `report` output includes diagnostics and unresolved deployment/activation requirements.
`preview` succeeds only for a finalized canonical candidate and writes deterministic Lua to stdout.

## Generations

Candidate preparation validates and compiles routes, pools, TLS identities, static roots, access
secrets, recording stores, health supervisors, relay policies, management assets, stats credentials,
and all listener reservations before publication. It does not write the canonical file. Matching
listener reservations are process-owned and reused by later generations.

Activation swaps active and previous generation references under one lock, closes the old logical
accept gate, and retains the previous generation for explicit rollback. HTTP/1, HTTP/2, WebSocket,
TCP, and RTMP references are counted independently and drained under a caller-supplied deadline.
Failed candidates are dropped without changing active state. Canonical management writes remain
revision-checked and invalid drafts do not alter disk.

The watcher observes the parent directory so rename-based replacement is visible, debounces event
bursts, and periodically reconciles exact SHA-256 disk revisions. Invalid snapshots are rejected and
do not replace active generation state. Operational logs on the `oxiroute::operations` target are
structured JSON and never include source text, secret values, configured paths, or OS error strings.

This release does not claim cross-process inherited-file-descriptor upgrade. Listener reuse is
strictly process-local.

## Shutdown

Pingora is configured with a 3-second connection grace period followed by a 2-second runtime
shutdown deadline. SIGTERM requests graceful shutdown; packaging uses a 10-second systemd stop
deadline. Tests send SIGTERM and reserve force-kill only as a bounded test cleanup fallback.

## Readiness

`GET /ready` returns 200 only when an active non-degraded generation exists and all configured
traffic listeners report `listening`; otherwise it returns 503. `GET /api/v1/status` reports the
build version, disk/candidate/active/previous revisions, degradation, and listener states.

`GET /metrics` exports process, listener, pool, server, queue, health, retry, certificate, RTMP
relay/recording, and generation families in Prometheus text format. `/stats` provides a compact
read-only HAProxy-oriented pool/server view. `/metrics` and `/ready` are public on a configured
statistics bind; `/stats` and `/api/v1/status` require a loopback peer and the statistics Bearer
token.
