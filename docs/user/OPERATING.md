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
cancel running work.

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

## Evacuate A Server

`evacuate` is drain plus a one-shot cancel: HTTP requests running on the server that have not started
responding and can be replayed are cancelled and dispatched again to another server of the same pool, with
the same request id and no error for the caller. The administrative state is `drain` (same persistence,
`server ready` un-drains). The command prints how many requests moved, how many were forced and how many were
left running, by reason:

```sh
oxiroute server evacuate --pool web endpoint-1
oxiroute server evacuate --force --pool web endpoint-1   # also cancel what cannot move
oxiroute pool evacuate web
```

Upgrades, forward proxy and `CONNECT`, HTTP/3, TCP, UDP and RTMP traffic is drain only. The rules, the
route settings that make a `POST` replayable and the report are in the
[runbook](RUNBOOK.md#evacuate-a-server).

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

A file written to disk that cannot be loaded, validated or prepared is not applied and does not
stop the running configuration. It is logged as an error with the code and the failing setting (and
line, for a syntax error), counted by `oxiroute_config_rejections_total`, listed by `events list`
as `configuration_reload` / `rejected`, and shown by `generation status` as `lastRejection` with
`degraded: true` until the next acceptable file replaces it.

Listener changes apply on reload in both runtimes, so adding, removing or moving a listener does
not need a restart and does not interrupt traffic:

- A new bind address is bound before traffic moves to the new revision.
- A listener that keeps its bind address keeps its socket, and its policy, service, protocol and TLS
  profile can change at the same time.
- A listener whose address is removed (or moved) stops accepting once the new revision is active;
  requests, streams and relay connections it already accepted finish normally, and the address is
  free again straight away.
- If a new address cannot be bound, the whole reload is rejected and the running revision keeps
  serving unchanged. The log names the listener, and the same revision is retried within 30 seconds
  (or on `systemctl reload oxiroute`) once the address is free.

Validation and a save report the effect in an `I_LISTENER_CHANGES` diagnostic that lists the added,
removed and kept listeners. The old generation closes its idle keep-alive connections at once and
keeps serving work already in progress until it ends or the `retirement.drain_deadline_ms` of that
configuration passes (default: the larger of three hours and the longest HTTP
`upstream_io_timeout_ms`; `OXIROUTE_RELOAD_DRAIN_SECONDS` is deprecated). In supervised mode a
further reload waits while a long request keeps the old worker alive.

Only one change still needs a restart, in either runtime: a Unix listener that keeps its path and
changes its filesystem mode is valid but `restartRequired`, because the new socket would need the
path the running one owns. Validation exposes the backend `I_RESTART_REQUIRED` diagnostic before the
write. A successful save returns `saved_restart_required`: the canonical file is durable, the active
generation remains unchanged, and the saved configuration takes effect after the next process
restart.

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

These settings are runtime state, not configuration. They stay on the server (matched by pool and
server name) when the configuration file changes or you reload, apply or roll back a generation, so
a drained server stays drained. A server removed from the file loses its state, and a server added
to it starts ready. A restart of the process resets them.

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
