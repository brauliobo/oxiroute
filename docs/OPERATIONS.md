# Operations

## CLI

`oxiroute serve CONFIG` starts the daemon. A lone positional `CONFIG` remains accepted for
existing invocations. Offline commands are:

```sh
oxiroute config check /etc/oxiroute/oxiroute.lua
oxiroute import nginx /etc/nginx/nginx.conf --root-prefix / --output report
oxiroute import nginx /etc/nginx/nginx.conf --root-prefix / --host-timezone America/Bahia --default-access-log-file /var/lib/oxiroute/http-access.jsonl --recording-root /mnt/cloud/4tb/cam-rtmp --default-error-server nginx/1.30.2 --output preview
oxiroute import haproxy /etc/haproxy/haproxy.cfg --output preview
oxiroute import haproxy /etc/haproxy/haproxy.cfg --node-ip 10.0.0.15 --gpu1-defined --output preview
oxiroute import haproxy /etc/haproxy/haproxy.cfg --node-ip 10.0.0.15 --gpu1-defined --shadow-port-offset 10000 --output preview
oxiroute config compose nginx.lua haproxy.lua
oxiroute version
```

Import `report` output includes diagnostics and unresolved deployment/activation requirements.
`preview` succeeds only for a finalized canonical candidate and writes deterministic Lua to stdout.
HAProxy files using `${NODE_IP}` or `defined(GPU1)` require the corresponding explicit
`--node-ip` and `--gpu1-defined` preprocessing inputs; the importer fingerprints those values.
`--shadow-port-offset` is preview-only and shifts imported IP socket listener ports after exact
lowering, then revalidates the complete canonical configuration for side-by-side canaries.
`--recording-root` replaces exactly one native nginx recording root with an explicit canonical
no-symlink path and fails closed when the source has zero or multiple recording roots.
`config compose` loads finalized canonical inputs in order and rejects conflicting process-wide
settings, duplicate names, and invalid cross-references. Run `config check` on the composed file in
the target host environment to verify runtime preparation and referenced files before activation.

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
