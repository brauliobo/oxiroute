# OxiRoute management CLI

`oxiroute` is the installed daemon, offline configuration/import tool, and operator client. `oxr`
is its short alias. Management commands default to `OXIROUTE_ENDPOINT`, or
`http://127.0.0.1:9900` when the variable is unset. Every management API operation except the
explicitly public `/ready` and `/metrics` probes uses the Bearer token loaded from `--token-file` or
`OXIROUTE_MANAGEMENT_TOKEN_FILE`; when neither is supplied, the client reads the plain
`OXIROUTE_MANAGEMENT_TOKEN_FILE=/path` assignment from the packaged `/etc/oxiroute/oxiroute.env`
and then checks `/etc/oxiroute/management.token`. The explicit option wins over the environment,
the environment wins over the package assignment and built-in default, and an absent default does
not affect public commands. The package file is read as a bounded assignment only; it is never
executed or interpreted as shell. Token contents are never included in output or diagnostics. The
CLI opens token and configuration files as bounded, regular, no-follow files. Token bytes are
zeroized after use and token files must have mode `0400` or `0600`.

An authenticated command without a discovered token reports a local missing/unreadable token-file
error. A configured or discovered token file is still rejected when it is inaccessible, non-regular,
symlinked, oversized, incorrectly permissioned, unstable while read, or contains an invalid token.

This CLI table describes current command ownership, not broad product compatibility. `Supported`
means the current runtime owns the operation; it does not promote a foundation or partial protocol
to stable parity. Native import remains an offline or compositional-source adapter, and the current
management listener stays loopback-only.

The management listener is configuration-owned and is restricted to a loopback address. Configure
it as `127.0.0.1:9900` to use the client default. `--output json` emits one JSON value on stdout;
diagnostics use stderr. Exit categories are stable: `2` usage, `3` local input/token, `4` transport,
`5` authentication, `6` missing resource, `7` revision/state conflict, `8` remote/protocol failure,
and `9` intentionally unsupported.

## Capability matrix

Status meanings:

| Status | Meaning |
|---|---|
| Supported | The active OxiRoute runtime owns the state and the API performs the operation. |
| Config generation | Change canonical config, validate it, and activate a generation; it is not an in-place runtime mutation. |
| External | Another process or operating-system facility owns the operation. |
| Intentionally unsupported | OxiRoute cannot perform the operation truthfully with its current ownership model. |

| Domain | Operation | Status | Command or reason |
|---|---|---|---|
| Process | status | Supported | `oxiroute status` |
| Process | readiness | Supported | `oxiroute ready` |
| Process | drain admissions | Supported | `oxiroute drain` |
| Process | graceful shutdown | Supported | `oxiroute shutdown`; authenticated loopback request enters the daemon's existing shutdown path. |
| Generation | validate/prepare | Supported | `oxiroute config validate FILE`; performs complete runtime preparation without publication. |
| Generation | reload/activate | Supported | `oxiroute generation reload`; loads and fully prepares persisted canonical config, then queues an opaque candidate. The supervisor publishes it only after the replacement runtime reports ready. |
| Generation | rollback | Supported | `oxiroute generation rollback`; prepares the retained previous configuration as a new candidate and uses the same readiness-before-publication path. |
| Generation | drain | Supported | `oxiroute generation drain --timeout-ms N`; closes the generation admission gate and immediately reports remaining references. The request does not block a Pingora worker for the timeout. |
| Generation | status | Supported | `oxiroute generation status` |
| Generation | events | Supported | `oxiroute events list` and `events follow`; bounded cursor polling over a 2,048-event in-memory ring. A partial page advances only to its last returned event and reports `hasMore`, so backlog pages are not skipped. |
| Config | get | Supported | `oxiroute config get` |
| Config | diff | Supported | `oxiroute config diff FILE`; local structural JSON comparison against normalized active config. |
| Config | validate | Supported | `oxiroute config validate FILE` |
| Config | apply | Supported | `oxiroute config apply FILE`; uses the current disk revision as a precondition. |
| Config | native import | Config generation | `oxiroute import nginx|haproxy|squid|apache|varnish ...`; import remains an offline evidence-producing operation. Varnish accepts repeated `--arg` options for explicit varnishd facts. |
| Listener | list/show | Supported | `oxiroute listener list|show NAME` |
| Listener | ready/drain/maintenance | Supported | Admission state; existing connections are not revoked. |
| Listener | capacity visibility | Supported | List/show includes configured maximum and active/rejected counters. |
| Listener | capacity mutation | Config generation | Change `listeners[].max_connections`, validate, and activate. |
| Pool | list/show | Supported | `oxiroute pool list|show NAME`; includes queue and availability counters. |
| Pool | ready/drain | Supported | Batch changes every server in each exact pool. Drain keeps health checks running. |
| Server | list/show | Supported | `oxiroute server list|show --pool POOL SERVER` |
| Server | ready/drain/maintenance | Supported | Atomic administrative state. Drain rejects new selections while existing leases complete and checks continue; maintenance also suspends checks. |
| Server | health override auto/up/down | Supported | `server set-health`; override is separate from observed health and `auto` restores observed-health selection. |
| Server | checks enable/disable | Supported | `server check`; maintenance still suspends enabled checks. |
| Server | max-connections set/reset | Supported | Runtime override; reset restores configured capacity. |
| Server | DNS refresh | Supported | `server refresh-dns`; resolves immediately and replaces startup-pinned addresses when applicable. Runtime-resolved endpoints are resolved and reported. |
| Server | queue/counter visibility | Supported | Pool/server reads and `monitoring` expose active work, queue depth/totals/timeouts/cancellations, checks, and failures. |
| Server | address, FQDN, check port, TLS, weight mutation | Config generation | These fields determine immutable endpoint/TLS/selection plans and require full preflight plus generation activation. |
| TLS | list/status | Supported | `oxiroute tls list` |
| TLS | reconcile | Supported | `oxiroute tls reconcile [--certificate NAME]` invokes active Certbot reconcilers. |
| TLS | obtain/renew certificates | External | Certbot or another configured certificate producer owns issuance and renewal. |
| RTMP | streams list/show | Supported | `oxiroute rtmp stream list|show ID` |
| RTMP | publisher disconnect | Intentionally unsupported | The catalog records publisher identity but does not own a safe session cancellation handle. The command exits `9`. |
| RTMP | recorder start/stop | Supported | `oxiroute rtmp recorder start|stop STREAM RECORDER`; only manual recorders are mutable. |
| RTMP | relay reconnect | Intentionally unsupported | Relay workers expose status but no targeted cancellation/reconnect handle. The command exits `9`. |
| Metrics | Prometheus exposition | Supported | `oxiroute metrics`; `/metrics` is intentionally public for local scraping. Other statistics reads require the configured statistics admin token. |
| Metrics | monitoring/topology | Supported | `oxiroute monitoring`, `oxiroute topology` |
| Events | bounded list/follow | Supported | Cursor and limit are validated; follow polls and never requests an unbounded stream. |
| Cache | purge | Configured HTTP route or forward service | Configured reverse HTTP routes and forward HTTP/1 services accept bearer-protected `PURGE` for an exact request key or configured surrogate tag. No separate CLI/API cache purge command is exposed. |

All mutations carry the exact active-generation revision. The CLI reads that revision immediately
before issuing a mutation; the server acquires a generation mutation permit before resolving any
target, and publication cannot cross a live permit. All server batches carry exact
`{pool, server}` targets. The API validates every pool/server before applying any mutation, so a
stale revision or unknown target leaves the entire batch unchanged. Empty batches and partially
matched CLI targets are rejected. DNS refresh is the explicit exception after target validation:
resolution is external and non-atomic, so the API returns every per-server outcome with
`atomic: false` and uses `207` when any lookup fails. A single exact server across several pools is
one command:

```sh
oxiroute server drain --pool public-v4 --pool public-v6 origin-a
```

## HAProxy Runtime API mapping

OxiRoute does not emulate HAProxy's text socket. The following table maps intent to typed client/API
operations and calls out cases that must use canonical generations or external ownership.

| HAProxy Runtime API | OxiRoute mapping |
|---|---|
| `show info` | `oxiroute status` plus `oxiroute monitoring` for process/host counters. |
| `show stat` | `oxiroute monitoring`, `pool list`, `server list`, and `listener list`; JSON counter names are stable. |
| `show servers state` | `oxiroute server list`; returns administrative state, observed health, override, checks, capacity, and active work. |
| `set server B/S state ready` | `oxiroute server ready --pool B S`. |
| `set server B/S state drain` | `oxiroute server drain --pool B S`. |
| `set server B/S state maint` | `oxiroute server maintenance --pool B S`. |
| `set server B/S health up` | `oxiroute server set-health --pool B S up`. |
| `set server B/S health stopping` | Administrative drain: `server drain`; OxiRoute does not overload observed health with administrative state. |
| `set server B/S health down` | `oxiroute server set-health --pool B S down`; `auto` clears the override. |
| `enable server B/S` | `server ready`; check enablement is separate via `server check ... enable`. |
| `disable server B/S` | `server maintenance`; use `server check ... disable` when only probes should stop. |
| `set maxconn server B/S N` | `oxiroute server max-connections set --pool B S N`; `reset` restores canonical capacity. |
| `set weight B/S W` | Config generation. Weight is part of selection policy and is not represented as a mutable runtime field. |
| `set server B/S addr A`, `fqdn F`, `check-port P`, `ssl` | Config generation. Endpoint identity, health targets, and upstream TLS are preflighted immutable plans. |
| DNS resolver commands | `oxiroute server refresh-dns --pool B S`; no resolver hold/timeout mutation is exposed. |
| `pause frontend F` | `oxiroute listener maintenance F`; rejects new admissions without closing established connections. |
| `resume frontend F` | `oxiroute listener ready F`. |
| `shutdown sessions server B/S` / frontend session shutdown | Intentionally unsupported. OxiRoute tracks aggregate leases/connections, not cancellable per-session handles. Drain is safe and supported. |
| `clear counters`, `clear counters all` | Intentionally unsupported. Counters are monotonic process/generation evidence; scrape deltas or activate a new generation. |
| `show map`, `set map`, ACL commands | Config generation. Routes and access policy compile into immutable generation tables; there is no detached mutable map store. |
| certificate show/set/commit commands | Status and Certbot reconcile are supported. Certificate material transactions are externally written and atomically activated by configured reconcilers; raw key/certificate upload is intentionally absent. |
| HAProxy transaction commands | Canonical `config validate` plus revision-preconditioned `config apply`, followed by `generation reload`. OxiRoute does not expose arbitrary command transactions. |

## Generation and shutdown guarantees

Preparation reserves listeners by bind identity, builds runtime plans, loads management and stats
tokens, checks UI and certificate-watcher prerequisites, and creates RTMP runtimes without
publishing. Concurrent preparation cannot cause one caller to activate another caller's candidate:
activation accepts only the opaque candidate returned by that preparation. Renaming a listener or
reordering statistics binds does not force a rebind when the transport identity is unchanged.

Candidate listener tasks start behind a process-owned accept gate. Once every task reports ready,
publication closes the old generation gate and opens the candidate gate atomically. A startup
failure quarantines that candidate, leaves the old active generation accepting, and remains
retryable on the next reconciliation. HTTP/1.1, HTTP/2, WebSocket, TCP, and RTMP admissions retain
generation references through transport teardown. Retired runtimes therefore remain alive until
their admitted work drains; process and listener capacities count shared active work across that
overlap. Process drain state and process uptime are also process-owned and do not reset on reload.

SIGTERM, SIGINT, and authenticated shutdown use one five-second process deadline. Startup waits and
drain polling are cancellation-aware; after the deadline the daemon stops waiting rather than
starting another independent timeout. Unexpected death of the active runtime is a daemon failure
and exits nonzero.

## Socket-loop translations

A common HAProxy drain loop:

```sh
for backend in public-v4 public-v6; do
  printf 'set server %s/%s state drain\n' "$backend" origin-a | \
    socat stdio /run/haproxy/admin.sock
done
```

becomes one prevalidated batch:

```sh
oxiroute server drain --pool public-v4 --pool public-v6 origin-a
```

Equivalent status and recovery commands are:

```sh
oxiroute --output json server show --pool public-v4 --pool public-v6 origin-a
oxiroute server ready --pool public-v4 --pool public-v6 origin-a
```
