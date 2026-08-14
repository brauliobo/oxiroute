# Operating OxiRoute

This page is the task-oriented layer over [OPERATIONS.md](../OPERATIONS.md) and the complete
[management CLI matrix](../MANAGEMENT_CLI.md).

## Establish Baseline

```sh
export OXIROUTE_ENDPOINT=http://127.0.0.1:9900

oxiroute ready
oxiroute status
oxiroute generation status
oxiroute listener list
oxiroute pool list
```

For the packaged service, the client discovers `/etc/oxiroute/management.token` automatically. Set
`OXIROUTE_MANAGEMENT_TOKEN_FILE` only for a custom token path; an authenticated command reports a
local token-file error when the selected file is missing or unreadable. The daemon still requires
the path assignment in `/etc/oxiroute/oxiroute.env` when its management listener is enabled.

`ready` is the cheap admission check. `status` reports build and generation revisions. Use
`monitoring` for host/process/load and traffic evidence:

```sh
oxiroute --output json monitoring > /tmp/oxiroute-monitoring.json
oxiroute --output json topology > /tmp/oxiroute-topology.json
oxiroute metrics
```

## Drain Before Maintenance

Drain rejects new admissions while existing work retains its generation reference. It does not
pretend to own per-session cancellation handles.

```sh
oxiroute listener drain web
oxiroute server drain --pool web endpoint-1
oxiroute generation drain --timeout-ms 5000
```

Restore admission explicitly:

```sh
oxiroute server ready --pool web endpoint-1
oxiroute listener ready web
```

For the same server name in several pools, repeat `--pool`; the client sends one prevalidated batch:

```sh
oxiroute server drain --pool public-v4 --pool public-v6 origin-a
```

## Reload And Roll Back

Candidate preparation validates the complete runtime plan before publication. A rejected candidate
leaves the active generation unchanged. The runtime exposes the distinction between disk, candidate,
active, previous, and quarantined generations:

```sh
oxiroute config validate /etc/oxiroute/oxiroute.kdl
oxiroute generation reload
oxiroute generation status
oxiroute generation rollback
```

The file watcher observes rename-based replacements through the parent directory and periodically
reconciles effective revisions, including strict native references. Use generation status as the
authority for completion; a durable configuration save may first report `saved_pending_activation`.

Restart compatibility depends on the runtime mode. In direct mode, changing the filesystem mode of
an active Unix listener at the same path is valid but `restartRequired` and is not published as a
live rebind. Other ordinary listener topology changes remain eligible for in-process activation.

In supervised mode, an incompatible change to the complete listener or control-listener descriptor
topology is `restartRequired`. This includes descriptor identity, order, role, kind, bind, Unix mode,
protocol, or count. Policy and service-only changes remain eligible for in-process activation.

Validation exposes the backend `I_RESTART_REQUIRED` diagnostic before the write. A successful save
returns `saved_restart_required`: the canonical file is durable, the active generation remains
unchanged, and the saved configuration takes effect after the next process restart.

## Manage Pool State

Observed health and administrative state are separate. Prefer drain or maintenance for traffic
admission and use a health override only when you intentionally want to change selection eligibility:

```sh
oxiroute server show --pool web endpoint-1
oxiroute server maintenance --pool web endpoint-1
oxiroute server check --pool web endpoint-1 disable
oxiroute server set-health --pool web endpoint-1 down
oxiroute server set-health --pool web endpoint-1 auto
oxiroute server max-connections set --pool web endpoint-1 200
oxiroute server max-connections reset --pool web endpoint-1
oxiroute server refresh-dns --pool web endpoint-1
```

DNS refresh is explicitly non-atomic because resolution is external. Inspect every returned outcome
before treating a partial result as a successful rollout.

## Events And Shutdown

The CLI exposes bounded cursor polling over the in-memory event ring:

```sh
oxiroute events list --after 0 --limit 100
oxiroute events follow --after 0 --limit 100 --interval-ms 1000
oxiroute shutdown
```

Events are live bounded operational delivery and are not durable audit history. Query durable,
redacted control history separately with the authenticated API:

```sh
curl -s -H "Authorization: Bearer $TOKEN" \
  'http://127.0.0.1:9900/api/v1/audit?after=0&limit=100' | jq '.records'
curl -s -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:9900/api/v1/audit/status | jq '.audit'
```

Audit writes are bounded by retention and size limits. A persistence failure is reported as audit
component degradation and does not fail the underlying control operation.
Signals and authenticated shutdown share one bounded process shutdown path.

## Public And Restricted Endpoints

On a configured statistics bind:

- `GET /ready` is public and returns `200` only for an active, non-degraded generation with listening
  traffic listeners.
- `GET /metrics` is public for Prometheus scraping.
- `/stats` and `/api/v1/status` require a loopback peer and the statistics bearer token.
- Management configuration routes require the management bearer token.

Read [SECURITY.md](SECURITY.md) before changing binds, token file ownership, or service permissions.
