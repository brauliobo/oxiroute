# OxiRoute management CLI

`oxiroute` is the installed daemon, offline configuration/import tool, and operator client. `oxr`
is its short alias. Management commands default to `OXIROUTE_ENDPOINT`, then the named context (`--context`,
`OXIROUTE_CONTEXT` or the current one of `~/.config/oxiroute/contexts`, see [CLI.md](reference/CLI.md#contexts)), or
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
to stable parity. Native import commands remain offline or compositional-source adapters; retained
redacted reports are also available through the authenticated management API/UI, while native files
remain read-only. The current management listener stays loopback-only.

The management listener is configuration-owned and is restricted to a loopback address. Configure
it as `127.0.0.1:9900` to use the client default. `--output json` emits one JSON value on stdout (the
management API body, unchanged); `--output table` and `--output plain` are the curated forms described in
[reference/CLI.md](reference/CLI.md#output). Diagnostics use stderr. Exit categories are stable: `2` usage, `3` local input/token, `4` transport,
`5` authentication, `6` missing resource, `7` revision/state conflict, `8` remote/protocol failure,
and `9` intentionally unsupported. `oxiroute check`, `oxiroute tls check` and `oxiroute doctor` are the exceptions: they exit
with the monitoring-plugin codes `0` OK, `1` WARNING, `2` CRITICAL and `3` UNKNOWN (see below).

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
| Process | readiness | Supported | `oxiroute ready [--wait]`; `--wait` keeps asking until the daemon answers, up to `--timeout` (30s by default). |
| Process | drain admissions | Supported | `oxiroute drain` |
| Process | completions, manual page | Supported | `oxiroute completions bash|zsh|fish [--name oxr]` and `oxiroute man [--name oxr]`; installed by the packages for `oxiroute` and `oxr`. |
| Process | graceful shutdown | Supported | `oxiroute shutdown`; authenticated loopback request enters the daemon's existing shutdown path. |
| Generation | validate/prepare | Supported | `oxiroute config validate FILE`; FILE is any native format (KDL, Lua, HOCON, UCI) or a JSON document, by extension or `--format`; performs complete runtime preparation without publication, through `POST /api/v1/config/source/validate`. `oxiroute config check FILE` checks a native file offline. |
| Generation | reload/activate | Supported | `oxiroute generation reload [--wait]`; `--wait` polls `generation status` until the candidate revision is active or the file is refused, up to `--timeout`. Loads and fully prepares persisted canonical config, then queues an opaque candidate. The supervisor publishes it only after the replacement runtime reports ready. |
| Generation | rollback | Supported | `oxiroute generation rollback`; prepares the retained previous configuration as a new candidate and uses the same readiness-before-publication path. |
| Generation | drain | Supported | `oxiroute generation drain [--wait]`; stops the active generation from admitting new connections on its traffic listeners and reports the traffic references that remain (`drained` when none do). `--wait` keeps the request open until they are gone, up to the global `--timeout` (30s by default, at most 5m), and the CLI allows the answer that long; without it the report is immediate. The management and statistics listeners keep serving, `ready` answers `503` while the generation is drained, and `generation reload` ends the drain by activating a fresh generation. The request does not block a Pingora worker for the timeout. |
| Generation | status | Supported | `oxiroute generation status` |
| Generation | events | Supported | `oxiroute events list` pages over the 2,048-event in-memory ring; a partial page advances only to its last returned event and reports `hasMore`, so backlog pages are not skipped. `events follow` reads the server-sent event stream (see Events below). Both use the `/api/v2/events` contract, so event names are the corrected ones. |
| Config | get | Supported | `oxiroute config get` |
| Config | diff | Supported | `oxiroute config diff FILE [--against active\|disk] [--text] [--exit-code]`; path-aware changes (list items by name, secrets redacted) of any native file against the running or stored configuration, plus the unified text diff with `--text`; `--exit-code` exits `1` on differences. |
| Config | validate | Supported | `oxiroute config validate FILE`; a refusal exits `3` with each diagnostic's code, `FILE:LINE`, path, rule and a hint. |
| Config | show | Supported | `oxiroute config show [FILE] [--active\|--disk]`; the authored text stored on disk (inline RTMP secrets as `<redacted:...>` placeholders), the running configuration rendered canonically, or a local file. |
| Config | edit | Supported | `oxiroute config edit [--yes] [--wait]`; `$VISUAL`/`$EDITOR` on a private copy of the stored text, validate, review the diff, confirm, apply with the revision precondition; an empty edit cancels. |
| Config | fmt | Supported | `oxiroute config fmt FILE [--check\|--write\|--diff]`; canonical form of the typed configuration (comments are not preserved). |
| Config | apply | Supported | `oxiroute config apply FILE [--dry-run] [--wait] [--convert]`; stores the file's own text (comments, formatting and Lua logic kept) using the current disk revision as a precondition (`PUT /api/v1/config/source`); `--wait` returns when the revision is active or fails with the rejection reason; identical text is a no-op; a daemon that cannot write its file (`configWritable: false`) is refused up front with the fix. |
| Config | history | Supported | `oxiroute config history`; the earlier versions of the daemon's file that every API save keeps (`GET /api/v1/config/history`): id, save time, size; bounded by `config_history` (20 versions and 16 MiB by default) and kept as `0600` files in `<file>.history/`. |
| Config | show --rev / diff --rev | Supported | `oxiroute config show --rev ID`, `oxiroute config diff --rev A [--rev B] [--text] [--exit-code]`; one kept version, or what changed between two (`current` is the stored file), with secrets redacted. |
| Config | rollback | Supported | `oxiroute config rollback ID [--wait]`; restores a kept version as a normal validated apply (`POST /api/v1/config/history/{id}/rollback`, revision precondition, nothing written when it no longer validates); the replaced text becomes the newest version. |
| Config | native import | Config generation | `oxiroute import nginx|haproxy|squid|apache|varnish ...`; the CLI remains an offline evidence-producing operation, while retained redacted reports are available through the authenticated management API/UI. Varnish accepts repeated `--arg` options for explicit varnishd facts. |
| Listener | list/show | Supported | `oxiroute listener list|show NAME` |
| Listener | ready/drain/maintenance | Supported | Admission state; existing connections are not revoked. `undrain` is an alias of `ready` for listeners, pools and servers. |
| Listener | capacity visibility | Supported | List/show includes configured maximum and active/rejected counters. |
| Listener | capacity mutation | Config generation | Change `listeners[].max_connections`, validate, and activate. |
| Pool | list/show | Supported | `oxiroute pool list|show NAME`; includes queue and availability counters. |
| Pool | ready/drain | Supported | Batch changes every server in each exact pool. Drain keeps health checks running. |
| Pool | evacuate | Supported | `oxiroute pool evacuate [--force] POOL...`; drains every server of each pool. No server is left to take a moved request, so running requests are reported as `no_destination` and finish as in drain unless `--force` cuts them (see Server evacuate). |
| Server | list/show | Supported | `oxiroute server list [--pool POOL] [--origin static|dynamic]` and `server show --pool POOL SERVER`. `list --pool` also works on a registry read socket or with a read token and adds the lease time left of registered servers. |
| Server | register/renew | Supported | `oxiroute server register --pool POOL SERVER --address A [--weight N] [--priority N] [--max-connections N] [--ttl 15s] [--label K=V]... [--metadata-file F] [--heartbeat]` for pools with `registration`; `--heartbeat` stays in the foreground, renews at a third of the TTL, registers again if the lease was lost, and on SIGTERM or SIGINT announces `draining` and deregisters. |
| Server | deregister | Supported | `oxiroute server deregister --pool POOL SERVER [--missing-ok]`; exit `6` when not registered unless `--missing-ok`. |
| Registry | scoped tokens | Supported | `oxiroute registry token create --tokens-file FILE --id ID (--pool POOL... [--name-prefix P] | --read)` appends a digest line to the tokens file (mode `0640` when created) and prints the secret once; ids are never reused. Offline command: no daemon needed. |
| Server | ready/drain/maintenance | Supported | Atomic administrative state. Drain rejects new selections while existing leases complete and checks continue; maintenance also suspends checks. |
| Server | evacuate | Supported | `oxiroute server evacuate [--force] --pool POOL... SERVER`; atomic `drain` plus a one-shot cancel and transparent re-dispatch to another server of the pool of the HTTP reverse proxy requests that sent no response and are replayable under the route's `retry` policy (at most 3 times each). Prints `{outcome, force, cancelledAndRedispatched, forced, leftRunning}` with `leftRunning` counting by reason (`response_started`, `not_replayable`, `body_not_replayable`, `upgraded`, `limit_reached`, `no_destination`). `--force` also cancels the rest (`503` with `x-oxiroute-reason: evacuated`, or a closed connection once the response started). Upgrades, forward proxy/CONNECT, HTTP/3, L4 and RTMP are drain only. The cancel is not persisted; `server ready` un-drains. |
| Server | health override auto/up/down | Supported | `server set-health`; override is separate from observed health and `auto` restores observed-health selection. |
| Server | checks enable/disable | Supported | `server check`; maintenance still suspends enabled checks. |
| Server | max-connections set/reset | Supported | Runtime override; reset restores configured capacity. |
| Server | weight set/reset | Supported | Runtime override of the weight (1 to 100) used by `weighted_round_robin` and the load key; reset restores the configured weight. |
| Server | state across generations | Supported | Administrative state, health override, check switch, max-connections and weight overrides follow a server by pool and server name through every generation change (file write, `generation reload`, `config apply`, rollback, supervised worker replacement). They are applied before the new generation accepts traffic. Servers that no longer exist lose their state; new servers start ready. Registered servers keep the same state; an operator's drain or maintenance on one, or on its pool, is also stored with its registration, so it survives a restart. Every mutation answer prints `persistence`: `runtime` (this process only, lost at a restart), `registry` (stored with the registration), `overlay` (kept with `--persist` in the overlay file beside the configuration and applied again at every start and reload) or `mixed`. |
| Server | overlay | Supported | `--persist` on `ready`, `drain`, `maintenance`, `set-health`, `check`, `max-connections`, `weight` and `evacuate` (server, pool and listener forms) writes the change to `<configuration file>.overlay.json` before applying it. `overlay show` reports each setting as `applied`, `diverged` or `orphan`; `overlay clear` deletes the file; `overlay promote` folds connection limits into the configuration file. |
| Server | DNS refresh | Supported | `server refresh-dns`; resolves immediately and replaces startup-pinned addresses when applicable. Runtime-resolved endpoints are resolved and reported. |
| Server | queue/counter visibility | Supported | Pool/server reads and `monitoring` expose active work, queue depth/totals/timeouts/cancellations, checks, and failures. |
| Server | address, FQDN, check port, TLS, weight mutation | Config generation | These fields determine immutable endpoint/TLS/selection plans and require full preflight plus generation activation. |
| TLS | list/show | Supported | `oxiroute tls list [--expiring-within 30d]` and `tls show NAME`; the expiry is printed in UTC with the time left (`in 29d04h`, `expired 2h05m ago`). The daemon reports the expiry of every source: files, Certbot lineages, ACME-managed and development certificates. |
| TLS | inspect a server | Supported | `oxiroute tls inspect HOST:PORT [--servername NAME] [--ca-file FILE]` handshakes with any TLS server and prints the certificate it presents (subject, issuer, names, validity in UTC, fingerprint, protocol, and whether the chain and name are trusted). It needs no token. |
| TLS | expiry check | Supported | `oxiroute tls check [HOST:PORT] [--certificate NAME]... [--warn 30d] [--crit 7d] [--servername NAME] [--verify [--ca-file FILE]] [--json]`; monitoring-plugin exit codes like `oxiroute check`: `0` OK, `1` WARNING when a certificate expires within `--warn`, `2` CRITICAL when it expires within `--crit`, has expired or (with `--verify`) is not trusted, `3` UNKNOWN. Without `HOST:PORT` it judges the certificates of the daemon (management token); with it, the certificate that server presents (no token). The line carries one `'NAME'=SECONDS;WARN;CRIT;0` performance datum per certificate. |
| TLS | reconcile | Supported | `oxiroute tls reconcile [--certificate NAME]` invokes active Certbot and ACME reconcilers. |
| TLS | renew | Supported | `oxiroute tls renew [--certificate NAME]` renews ACME-managed certificates now. Certificates from files or Certbot are renewed by their producer; exit `6` when the daemon has no managed certificate of that name. |
| TLS | revoke/delete | Supported | `oxiroute tls revoke NAME --yes [--reason 0..10]` and `oxiroute tls delete NAME --yes` for ACME-managed certificates; `--yes` is required. A certificate a TLS profile still references cannot be deleted (exit `7`). |
| TLS | ACME account and jobs | Supported | `oxiroute tls acme rollover|pause|resume|cancel [--certificate NAME]`. |
| RTMP | streams list/show | Supported | `oxiroute rtmp stream list|show ID` |
| RTMP | publishers and viewers | Supported | `oxiroute rtmp publisher list` and `rtmp viewer list` print the connected sessions of `/api/v1/rtmp/stats/clients` (id, stream, peer, age, session revision). |
| RTMP | kick a session | Supported | `oxiroute rtmp publisher kick SESSION` and `rtmp viewer kick SESSION` read the session's current revision and send it as `If-Rtmp-Session-Revision`, so the daemon drops that exact session; a session that is gone is exit `6`, one that changed role or revision is exit `7`. `rtmp stream disconnect STREAM` kicks the publisher of a stream and leaves the stream and its viewers. The drop is requested (`202`) and completes asynchronously. |
| RTMP | recordings | Supported | `oxiroute rtmp recording list [--stream S] [--recorder R] [--status recording\|complete\|partial] [--limit N] [--cursor C]`, `recording show ID`, `recording delete ID --yes`, and `recording download ID [--to FILE\|-] [--force]`. Ids are stable across restarts. A recording that is still being written cannot be deleted (exit `7`). Downloads are streamed to a temporary file next to the destination and renamed when complete; an existing destination needs `--force`. |
| RTMP | recording quota and retention | Supported | `oxiroute rtmp recording quota` prints each recording root with its recorders, bytes and files used against the quota, the retention (`reject` or `delete_oldest`), the maximum age, and what retention evicted. |
| RTMP | recorder start/stop | Supported | `oxiroute rtmp recorder start|stop STREAM RECORDER`; only manual recorders are mutable. |
| RTMP | relay reconnect | Intentionally unsupported | Relay workers expose status but no targeted cancellation/reconnect handle. The command exits `9`. |
| Metrics | Prometheus exposition | Supported | `oxiroute metrics`; `/metrics` is intentionally public for local scraping. Other statistics reads require the configured statistics admin token. |
| Metrics | monitoring/topology | Supported | `oxiroute monitoring`, `oxiroute topology` |
| Monitoring | check | Supported | `oxiroute check [--pool P ...] [--warn-available-below N] [--crit-available-below N] [--max-queue-wait 5s] [--max-inflight-age 60s] [--max-retired-age 1h] [--json] [--canary --url URL [--host H] [--expect-status 200]]`. Prints one plugin line and exits `0`/`1`/`2`/`3` (OK/WARNING/CRITICAL/UNKNOWN); these replace the exit categories above for this command only. Reads the public `/status.json`, so it needs no token and also works with `--socket` on a read-only registry socket. |
| Setup | init | Supported | `oxiroute init proxy|static|rtmp [--format kdl|lua|uci|hocon] [--file FILE|-] [--force]` writes a minimal configuration that loads, validates and prepares in every format: a proxy to one upstream pool on `0.0.0.0:8080`, static files from `public/` (created beside the file), or an RTMP `live` application on `0.0.0.0:1935`, each with a management API on `127.0.0.1:9900` and its token file `management.token` in the file's directory. The default file is `oxiroute.` and the format's extension in the current directory; an existing file is refused unless `--force`, and `-` prints to stdout. The paths in it are absolute. |
| Setup | management token | Supported | `oxiroute token create` writes the management token file (the one `--token-file` names, else the token file of the selected context, else the one the CLI would read, else `/etc/oxiroute/management.token`) with 32 random bytes as 64 hex characters, mode `0600`, and refuses to replace an existing file. `oxiroute token rotate` replaces the token atomically (temporary file in the same directory, then rename) keeping the file's owner and mode. The token is never printed. The daemon takes a rotated token at its next reload (`SIGHUP`, or `generation reload` sent with the old token) or restart; the CLI uses the file at once. |
| Monitoring | doctor | Supported | `oxiroute doctor [CONFIG] [--unit FILE] [--offline] [--json]`. One pass that reads the configuration file offline and probes this host: it validates and prepares the configuration (listener binds excepted), binds each listen address to see it is free (an address in use is not a problem when a daemon at `--endpoint` answers its status page, unless `--offline`), connects to every static upstream server, reads the expiry of the certificate files (30d warning, 7d critical like `tls check`), checks the modes of the configuration, management token, private key and credential files, of the overlay file and the history directory beside the configuration (an overlay the daemon cannot load is a warning), and of the contexts file and the token files its contexts name (critical for the selected context, warning for the others), and checks the systemd unit: every `ReadWritePaths=` entry exists, and every path the configuration writes (audit log, disk caches, ACME state, registry state, access logs, sockets, recordings, HLS and DASH output) is writable under `ProtectSystem=`, `ReadWritePaths=` and the `StateDirectory=`/`RuntimeDirectory=`/`CacheDirectory=`/`LogsDirectory=` settings, including drop-ins; with a management API, the configuration directory (where `config apply`, `--persist` overlays and the history are written) must be writable too, a warning when it is not. The unit is `--unit` or the first `oxiroute.service` of the systemd directories; its `ExecStart` names the configuration when `CONFIG` is omitted, and its `Environment=` and `EnvironmentFile=` give the daemon's environment. Problems print first, worst first, each with a `fix:` line; exit `0` OK, `1` WARNING, `2` CRITICAL, `3` UNKNOWN (wrong options). |
| Monitoring | live dashboard | Supported | `oxiroute top [--interval 1s] [--window 2s|10s|60s|life] [--once] [--json] [--csv] [--view listeners|upstreams|clients|conns|requests|rtmp|generations] [--filter EXPR] [--sort COLUMN] [--reverse] [--top N] [--no-mouse] [--ascii] [--color auto|always|never] [--theme dark|light|mono] [--reset-layout]`. Read-only terminal dashboard over `GET /api/v1/stats`, `/api/v1/requests`, `/api/v1/rtmp/stats`, `/api/v1/clients` and `/api/v1/connections` with the management token; a measured snapshot (two samples one window apart) instead of a screen with `--once`, `--json`, `--csv` or without a terminal. See "Live dashboard" below. |
| Routing | route test | Supported | `oxiroute route test URL` or `route test HOST PATH [--method M] [--listener L]` asks `GET /api/v1/routes/explain` which route, action and pool would serve a request. The daemon runs the host, path and method through the route table of the active generation, the table that serves traffic, on every HTTP listener or only the one named, and answers per listener with the route id, the action (`proxy`, `fixed`, `redirect`, `static`), the status of a fixed or redirect action, and the pool, rescue pool and capability-select pools with their available and total servers. Nothing is sent to an upstream. A URL names its host without the scheme's default port, and its path without the query. Exit `6` when no route serves the request (`listener_not_found` for an unknown `--listener`). |
| Events | list | Supported | `oxiroute events list [--after CURSOR] [--limit N]`; cursor and limit are validated. |
| Events | follow | Supported | `oxiroute events follow [--after CURSOR \| --since 10m] [--count N]` reads `/api/v2/events/stream`: it prints the retained events after the cursor (or of the window) first, then new ones as they happen. When the connection drops or the daemon sends `shutdown` it reconnects from the last event it printed, and gives up with exit `4` when the daemon stays unreachable for `--timeout` (30s by default). Events lost to the ring (`resync_required`) are reported on stderr and the stream continues from the oldest retained event. `--count N` stops after N events; Ctrl-C (or SIGTERM) ends it with status 130 (143). |
| Audit | list/status | Supported | `oxiroute audit list [--after CURSOR --limit N \| --since 1h] [--category reload\|import\|certificate\|control] [--result requested\|succeeded\|failed\|rejected\|conflict\|partial\|degraded]` and `audit status` over `/api/v1/audit`; `--since` prints every retained record of the window. |
| Audit | tail | Supported | `oxiroute audit tail [--after CURSOR \| --since 1h] [--interval 1s] [--count N]` asks for new records every `--interval` (the audit API has no stream) and prints them; Ctrl-C ends it with status 130. |
| Process | binary upgrade | Supported (supervised runtime) | `oxiroute upgrade [--config PATH] [--binary PATH] [--wait]`. Hands the listeners to a new executable without dropping a connection; it does not use the management API or the token but a local socket of the master's user, found from the configuration path. Fails with exit `4` when no supervising master serves the configuration, `8` when the hand-over is refused or the successor does not become ready (the running master keeps serving). See "Binary upgrade" in [OPERATIONS.md](OPERATIONS.md). |
| Cache | purge | Configured HTTP route or forward service | Configured reverse HTTP routes and forward HTTP/1 services accept bearer-protected `PURGE` for an exact request key or configured surrogate tag. No separate CLI/API cache purge command is exposed. |

## Live dashboard

`oxiroute top` is a read-only terminal dashboard in the spirit of htop and btop. It never mutates state and
uses the management endpoint, `--token-file` and the global `--output` and `--quiet` options like the other
management commands. Registry sockets (`--socket`) do not serve statistics, so `top` refuses them with exit `3`.

```sh
oxiroute top                                      # the live screen on a terminal
oxiroute top --view upstreams --sort 5XX          # start in the upstream tree, sorted by 5xx
oxiroute top --once --filter web                  # one measured text snapshot of rows matching "web"
oxiroute top --view upstreams --json | jq '.rows[] | {key, state: .values.state}'
oxiroute top --view clients --csv --window 10s    # clients as CSV, rates over a 10s window
oxiroute top --reset-layout                       # forget the saved layout
```

### Data sources

| View | Endpoint | Read while the view is open |
|---|---|---|
| Listeners, Upstreams, Generations | `GET /api/v1/stats` (the stats page snapshot, built per request) | always |
| Requests | `GET /api/v1/requests?limit=1000` | yes |
| RTMP | `GET /api/v1/rtmp/stats` | yes |
| Clients | `GET /api/v1/clients?limit=N&sort=bytes\|connections\|requests\|recent&ip=PREFIX` | yes |
| Conns | `GET /api/v1/connections?limit=N&ip=PREFIX&listener=NAME` | yes |

The header, tabs and summary always come from the stats snapshot. The Clients and Conns endpoints are optional:
when the server answers `404`, the view shows one line, `the server lacks per-client tracking` (or
`per-connection tracking`), and keeps working for every other view; when the answer or its body says the
feature is `disabled` (or the document has `enabled: false`), the line says it is disabled on the server and
how to enable it; any other failure is shown with its reason and tried again at the next interval. `--top N`
(1 to 1000, 200 by default) is how many clients the server is asked for; the `other` and `TOTAL` rows come from
the server's own aggregates, so they cover the clients that are not listed. A filter written as `ip:PREFIX` or
`listener:NAME` is also sent to the server.

There is no Routes view: the statistics keep no per-route counters, only the configuration.

### Views

Tabs, with a badge: `1` Listeners (count, `!N` unhealthy), `2` Upstreams (pools, `!N` down), `3` Clients,
`4` Conns, `5` Requests (in flight, red when one is overdue), `6` RTMP, `7` Generations (`#N` active,
`+N` draining). A view whose feed is unavailable stays in the strip without a badge.

- **Listeners**: one row per listener, `TOTAL` last.
- **Upstreams**: an expandable tree, one row per pool with its servers as children (keys `pool` and
  `pool/server`). `STATE`, `HEALTH` (active checks) and `OUTLIER` (passive ejection) show the health, breaker
  and outlier state; closed pools keep their summed counters.
- **Clients**: one row per client address (IPv6 grouped by the server's prefix), sorted by bytes in.
- **Conns**: one row per open connection with its TLS version, SNI and ALPN, upstream and current request.
- **Requests**: one row per in-flight request, oldest first.
- **RTMP**: a tree of live streams with their publisher and subscribers as children.
- **Generations**: the active generation and the retired ones that still drain, with the kinds of requests
  that keep them alive as children.

Columns appear by priority as the terminal narrows and are never truncated: `P1` always, `P2` from 100
columns, `P3` from 120, `P4` only from the column picker. A column you turn on in the picker stays on at every
width.

| View | `P1` | `P2` | `P3` | `P4` |
|---|---|---|---|---|
| Listeners | `NAME` `STATE` `CONN` `REQ/s` `IN/s` `OUT/s` `ERR/s` `P99` | `BIND` `ACC/s` | `PROTO` `P50` `CACHE` | `MAX` |
| Upstreams | `NAME` `STATE` `CONN` `QUEUE` `REQ/s` `IN/s` `OUT/s` `ERR/s` `P99` | `UP` `4XX` `5XX` `HEALTH` `ADDRESS` | `2XX` `P50` `OUTLIER` `ALGO` `WGT` `QWAIT` `INFL` | `3XX` `DOWN` `CHKFAIL` `MAX` |
| Clients | `CLIENT` `CONN` `REQ/s` `IN/s` `OUT/s` `ERR%` | `NEW/s` `IN tot` `OUT tot` `AGE` | `ERRORS` `REQS` `IDLE` `LISTENER` `PROTO` | `LISTENERS` `CONNS` |
| Conns | `CLIENT` `LISTENER` `UPSTREAM` `STATE` `AGE` `IN/s` `OUT/s` | `PROTO` `IDLE` `IN tot` `OUT tot` `REQUEST` | `REQS` `SERVICE` `ID` | `TLS` |
| Requests | `CLIENT` `METHOD` `PATH` `PHASE` `AGE` | `HOST` `LISTENER` `POOL` `SERVER` `WAIT` | `TRY` `ID` | |
| RTMP | `NAME` `ROLE` `SUBS` `IN/s` | `BYTES` `PEER` `AGE` | `AUDIO/s` `VIDEO/s` `SERVICE` | |
| Generations | `NAME` `STATE` `AGE` `DEADLINE` `IN-FLIGHT` | `REVISION` | | |

- `STATE` is `up`, `down`, `drain`, `maint`, `ejected`, `unchecked`, `unknown` for servers, `up`,
  `degraded`, `down`, `empty` for pools and `up`, `drain`, `maint`, `conf`, `stopped`, `failed` for listeners.
  `CONN` is current and maximum connections; `UP` is available and total servers.
- `REQ/s` of a listener counts finished HTTP operations (or TCP relays); of a pool or server it counts
  upstream attempts, including retries. `ERR/s` of a listener counts server errors, upstream errors,
  timeouts and internal errors (for relays: connect, I/O, accounting and PROXY protocol failures); of a pool
  or server it counts attempts that failed to connect, timed out or hit a protocol error. `IN/s` and
  `OUT/s` are bytes from and to the client for a listener, a client or a connection, and bytes to and from
  the server for a pool or server. `2XX`..`5XX` are lifetime response totals. `CACHE` is the hit ratio of
  the listener's HTTP cache. A column is `-` when the proxy does not export the value.
- `other` and `TOTAL` are pinned below the rows and ignore the filter: `TOTAL` sums the whole view (for
  Clients, the server's totals) and `other` is what the listed clients do not cover.
- Not shown, because the proxy exports no counter for them: TLS handshakes, per-worker load, per-listener
  status classes.

### Rates

Rates are counter deltas divided by the time between two server samples (`sampledAtUnixMs`). The window is
`2s`, `10s` (the default), `60s` or `life` (the counters over the uptime): `w` cycles it, `--window` sets it,
and the choice is remembered. The client keeps about a minute of samples; while the history is shorter than the
window the title says `filling` and the rate uses the oldest sample. A rate is `-` when a counter went
backwards, and a new row (a client or connection that appeared inside the window) is rated over its own age.
A restart, a reload into a new generation or a counter reset clears the history and says so. Latency
percentiles (`P50`, `P99`) are histogram bucket upper bounds over the same window (`>5s` past the last bound).

### Screen

- The status line shows the version, uptime, active generation, workers and warnings, the rate window and a
  live marker. Under it, the **meters** panel has a box with a sparkline, the peak and the total for requests,
  in and out, and one for the load (CPU, connections, memory, descriptors); below 100 columns it shrinks to a
  one-line strip. The **summary** panel holds the
  chips of the open view. The **detail drawer** of the selected row opens beside the table from 120 columns
  and below it otherwise, with the recent history of the row's rates.
- Below 60 columns or 12 rows the screen says `terminal too small`. Rendering uses rounded boxes, truecolor
  where `COLORTERM` says so and 16 colors otherwise; `NO_COLOR` or `--color never` use bold, dim and reverse
  instead, and `--ascii` (or a non-UTF-8 locale or `TERM=linux`) draws `+-|` borders and `#` bars.
- Rows keep a stable order and the selection follows the row key, so a refresh never moves the cursor;
  `o` freezes the order and a vanished row stays marked until it returns.

### Showing less

Everything can be collapsed, hidden or switched, and the choices are remembered.

- **Panels** (meters, summary, drawer): `h`, `s`, `i` collapse one to a title line (a click on the title does
  the same), `H`, `S`, `I` hide it. Hidden panels are listed at the right of the footer (`hidden: H Meters`);
  click one or press its key to bring it back.
- **Groups**: `Enter`, `Space` or `Right` open a pool, stream or generation, `Left` closes it or jumps to its
  parent, `+` and `-` open or close every group, and a click on the marker does the same. The state is kept by
  row key across refreshes. A filter opens the groups that hold a match. `t` groups Clients, Conns and
  Requests by listener.
- **Columns**: `c` (`F2`) opens a picker to turn columns on or off and reorder them (`K` and `J`); `r` there
  restores the defaults.
- **Views**: `V` (`F4`) picks which tabs exist and in which order; a hidden view stays in the list so it can be
  turned on again, and `--view` still starts in it.
- **Density**: `d` is the compact mode (no meters, summary or border, one line per row) and `f` maximizes the
  table.

The layout is saved after every change in `$XDG_STATE_HOME/oxiroute/top.json`
(`~/.local/state/oxiroute/top.json` when unset): panels, view order and visibility, columns per view, density,
theme, rate window and units. It names choices only, never hosts, addresses or filters. A file that cannot be
read is reported and the defaults are used; `--reset-layout` removes it. `--once`, `--json` and `--csv` use the
same layout, so a column you hid is hidden there too (JSON still lists it, with `visible: false`).

### Keys and mouse

| Keys | Action |
|---|---|
| `Tab` `Shift-Tab` or `]` `[`, `1`-`9` | next, previous or numbered view |
| `j` `k` or arrows, `PgUp` `PgDn`, `g` `G` or `Home` `End` | move the selection |
| `Enter` | open or close a group, otherwise the detail drawer; in the drawer, jump to the connections or requests of the row |
| `Space`, `Right`, `Left`, `+`, `-` | groups, as above |
| `<` `>` or `,` `.`, `r` | pick the sort column, reverse it; each view keeps its own sort |
| `/` | filter: words, `!word`, `column:text`, `ip:PREFIX`; `Enter` keeps it, `Esc` clears it |
| `w`, `u`, `o`, `t` | rate window, bytes or bits, freeze the order, group by listener |
| `h` `s` `i`, `H` `S` `I`, `d`, `f` | panels, density, focus |
| `c`, `V`, `T`, `m` | columns, views, theme (dark, light, mono), mouse capture |
| `p` or `F9` | pause and resume sampling (`Space` opens groups) |
| `F1` or `?`, `F10` or `q`, `Ctrl-C` | help, quit |

`Esc` closes the top layer (filter, help, picker, drawer); with none open it goes back from a drill-down
(`Enter` in the drawer, for example from a listener to its connections) and then clears the filter. The
mouse selects rows (a double click opens the drawer), sorts by a header click, switches tabs, opens groups
from their marker, collapses panels from their title, triggers the footer chips and scrolls with the wheel;
`--no-mouse` or `m` leaves the terminal its own text selection. A failed refresh shows the error under the
last snapshot and the next interval tries again; a failure on the first request exits with the usual
categories (`4` transport, `5` authentication).

### Printing: `--once`, `--json`, `--csv`

Without a terminal, or with `--once`, `--json` or `--csv`, `top` takes two samples one window apart (2 seconds
unless `--window` says otherwise; `life` needs one), so the rates are real, then prints the view and exits.
`--view` chooses the one view printed (without it the text output prints Listeners and Upstreams, and JSON and
CSV print Listeners); `--filter`, `--sort`, `--reverse` and `--top` apply as on the screen, and `--output json`
means `--json`. The text output is the headline (status, resources, traffic), a line saying what the rates are
over, and one plain table per view with indented children and the `other` and `TOTAL` rows. It shows the
columns of the widest screen tier.

`--json` prints one line, schema version `2`:

```json
{
  "schemaVersion": 2,
  "sampledAtUnixMs": 1790000000000,
  "window": { "name": "2s", "ms": 2000, "complete": true },
  "instance": { "version": "7.1.0", "uptimeMs": 0, "generation": 4, "accepting": true, "draining": 0, "warnings": 0 },
  "summary": {
    "connections": { "current": 7, "limit": 4096 },
    "rates": { "reqPerSec": 0, "errPerSec": 0, "bytesInPerSec": 0, "bytesOutPerSec": 0, "newConnPerSec": 0 },
    "process": { "cpuPercent": 12.5, "rssBytes": 0, "fds": 0, "threads": 0 }
  },
  "view": "upstreams",
  "sort": { "column": "name", "descending": false },
  "filter": "",
  "columns": [ { "key": "req", "title": "REQ/s", "unit": "perSecond", "visible": true } ],
  "rows": [ { "key": "web/origin-a", "depth": 1, "parent": "web", "values": { "req": 0, "state": "up" } } ],
  "other": null,
  "total": { "key": "TOTAL", "depth": 0, "parent": null, "values": {} },
  "truncated": false,
  "population": null,
  "limit": 200,
  "notice": null
}
```

`window.complete` is `false` when the history was shorter than the window and `ms` is `null` for `life`.
Column `unit` is `count`, `perSecond`, `bytes`, `bytesPerSecond`, `percent`, `milliseconds`, `text` or `null`
(a numeric column with no value yet); a cell is a number in that unit, a string, or `null` when there is no
value. Counters that are whole numbers print as integers. `rows` are in screen order with their `parent` key;
`other` and `total` are `null` or a row of the same shape; `population` is `{ "shown": n, "of": n }` for the
views the server truncates (`truncated` is then true); `notice` is the sentence the screen would show when the
feed is missing. Schema version 1 was the raw `GET /api/v1/stats` document: read that endpoint for it.

`--csv` prints `kind,key,` and the visible column keys, then one line per row (`kind` `row`), then `other`
and `total` lines when the view has them; numbers have no unit suffix and fields with commas are quoted.

## Registry endpoints

`--socket PATH` (or `OXIROUTE_ENDPOINT=unix:/absolute/path`) talks to a registry unix socket
instead of the management listener: no token is read or sent, because the socket's file mode and
group are the authorization. A `register` socket accepts `server register`, `deregister` and
heartbeats for its pools; a `read` socket accepts `server list --pool` and registry reads. Against a
registry TCP endpoint, use `--endpoint http://HOST:PORT --token-file FILE` with a scoped token; the
root management token is not accepted there. Exit codes are unchanged: a refused pool or scope is
`5`, a missing registration `6`, a name, address or lease conflict `7`.

```sh
oxiroute registry token create --tokens-file /etc/oxiroute/registry.tokens --id gpu-7 --pool render
oxiroute --socket /run/oxiroute/register.sock server register --pool render gpu-7 \
  --address 192.0.2.17:9000 --ttl 15s --heartbeat
oxiroute --socket /run/oxiroute/status.sock server list --pool render
```

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

`server evacuate` takes the same repeated `--pool` and sends the same exact-target batch to
`POST /api/v1/servers/evacuate`; `pool evacuate` sends `POST /api/v1/pools/evacuate`. Both print the JSON
report and honour `--output`.

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
| `set weight B/S W` | `oxiroute server weight set --pool B S W`; `reset` restores the configured weight. |
| `set server B/S addr A`, `fqdn F`, `check-port P`, `ssl` | Config generation. Endpoint identity, health targets, and upstream TLS are preflighted immutable plans. |
| DNS resolver commands | `oxiroute server refresh-dns --pool B S`; no resolver hold/timeout mutation is exposed. |
| `pause frontend F` | `oxiroute listener maintenance F`; rejects new admissions without closing established connections. |
| `resume frontend F` | `oxiroute listener ready F`. |
| `shutdown sessions server B/S` / frontend session shutdown | Partly supported. `server evacuate` cancels the in-flight HTTP requests of a server that can be replayed on another one; there is no per-session shutdown, and `--force` is the only way to cut the rest. L4 sessions are drain only. |
| `clear counters`, `clear counters all` | Intentionally unsupported. Counters are monotonic process/generation evidence; scrape deltas or activate a new generation. |
| `show map`, `set map`, ACL commands | Config generation. Routes and access policy compile into immutable generation tables; there is no detached mutable map store. |
| certificate show/set/commit commands | Status and Certbot reconcile are supported. Certificate material transactions are externally written and atomically activated by configured reconcilers; raw key/certificate upload is intentionally absent. |
| HAProxy transaction commands | `config validate` plus revision-preconditioned `config apply` (or `config edit`) of the native file; `--wait` replaces a separate `generation reload`. OxiRoute does not expose arbitrary command transactions. |

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

`generation status` reports `generation` (the live generation number: the activation count, or the
supervisor's generation id), `activeRevision`, and `previousRevision` (the last good revision that
the live one replaced; informational in supervised mode, where `generation rollback` is unavailable).

`generation status` and `status` report `restartRequired` (`required`, `candidateRevision`, `since`,
`reasons`) and print a first `RESTART REQUIRED: <code>: <summary>` row (`restart_required` in plain
output, with `(+N more)` for further reasons) while a setting waits for a restart.

`status` leads with `build` (the version) and `build_id` (the serving process). Under the supervising
master it adds `master_build_id`; the direct runtime has no master, so the row is not printed. The JSON
form carries `masterBuildId` as `null` there.

`generation status` reports `lastRejection` (`atUnixMs`, `revision`, `diagnostics`) and
`configRejections` when the configuration file on disk was refused while the previous generation
kept serving. `lastRejection` is cleared by the next file that is accepted, `degraded` is true while
it is set, and `/ready` is not affected.

`422` responses of `config validate`, `config apply` and `generation reload`, and `config check`,
name each failing setting: `path` is a JSON pointer, `detail` the failed rule and `line` the KDL
syntax-error line when known. `config validate`, `config diff`, `config apply` and `config edit` print each
diagnostic as `<code> <file>:<line> at <path>: <detail>` with a `hint:` line and exit `3`.

SIGTERM, SIGINT, and authenticated shutdown use one five-second process deadline. Startup waits and
drain polling are cancellation-aware; after the deadline the daemon stops waiting rather than
starting another independent timeout. Unexpected death of the active runtime is a daemon failure
and exits nonzero.

`oxiroute shutdown` stops a supervised daemon the same way: the worker reports the request in its
status and the master shuts the daemon down. A repeated request while the daemon is stopping is
accepted with the same `shutdown_requested` outcome rather than refused with `409`.

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
