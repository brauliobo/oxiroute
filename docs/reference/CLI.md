# CLI Reference

The `oxiroute` binary is both the daemon and the offline/operator client. `oxr` is the packaged
short alias. A lone configuration path remains an alias for `serve CONFIG`.

## Global Options

```sh
oxiroute [--endpoint URL] [--token-file FILE] [--output table|json|plain] [--quiet] COMMAND
```

- `--endpoint` defaults to `OXIROUTE_ENDPOINT`, then `http://127.0.0.1:9900`.
- `--token-file` explicitly selects the token file and wins over all automatic discovery.
- `OXIROUTE_MANAGEMENT_TOKEN_FILE` is the next source; when unset, the packaged
  `/etc/oxiroute/oxiroute.env` assignment is checked, followed by the built-in
  `/etc/oxiroute/management.token` path when that file exists.
- `--output json` writes one JSON value to stdout; diagnostics use stderr.
- Public `/ready` and `/metrics` probes do not need the management token. Other management operations
  use the bearer token according to their route contract.

Squid report output includes `capabilities`, a versioned target-checkout registry. Its `parity` is
`partial` and `completeParity` is false while any family or directive is partial, unsupported,
obsolete, or not planned. Only forms with both runtime integration and failure coverage are marked
`compatible`.

## Command Families

| Family | Commands | Use |
| --- | --- | --- |
| Process | `serve`, `status`, `ready`, `metrics`, `monitoring`, `topology`, `drain`, `shutdown` | Start and inspect the daemon |
| Monitoring | `check` | One line and a Nagios exit code for cron and monitoring systems |
| Lifecycle | `generation status|reload|rollback|drain` | Prepare, publish, revert, or drain generations |
| Configuration | `config check|compose|get|diff|validate|apply` | Inspect and change canonical configuration |
| Events | `events list|follow` | Poll the bounded in-memory event ring |
| Requests | `requests list|show` | Inspect the requests currently inside the router |
| Runtime inventory | `listener`, `pool`, `server` | Read or safely mutate admission/health state |
| TLS | `tls list|reconcile` | Inspect identities and invoke external-lineage reconciliation |
| RTMP | `rtmp stream`, `rtmp recorder`, `rtmp relay` | Inspect live sessions and supported recorder actions |
| Migration | `import nginx|haproxy|squid|apache|varnish` | Emit offline reports or finalized previews |
| Build | `version` | Print the build version |

`oxiroute requests list [--pool P] [--route R] [--older-than 30s] [--phase queued] [--limit N]` lists
the requests currently inside the router, oldest first, and `oxiroute requests show REQUEST_ID`
shows one. Phases are `routed`, `queued`, `connecting`, `awaiting_headers`, `streaming` and
`retry_wait`. There is no command to cancel one request; `server evacuate` and `pool evacuate` move the
replayable requests of a server (see below).

## `evacuate`

```sh
oxiroute server evacuate [--force] --pool POOL [--pool POOL ...] SERVER
oxiroute pool evacuate [--force] POOL [POOL ...]
```

`server evacuate` drains the server and cancels its in-flight HTTP reverse proxy requests that sent no
response and are replayable, which are dispatched again to another server of the pool without an error for
the caller. `--force` also cancels the requests that cannot move. `pool evacuate` drains every server of
each pool, so there is no other server left in the pool: by default it only drains and reports the running
requests as `no_destination`, and `--force` cuts them. The command prints the JSON report (`outcome`, `force`, `cancelledAndRedispatched`, `forced`, `leftRunning` by reason)
and honours `--output`. `server ready` un-drains. Details and limits:
[runbook](../user/RUNBOOK.md#evacuate-a-server).

## `check`: monitoring exit codes

```sh
oxiroute check [--pool P ...] [--warn-available-below N] [--crit-available-below N]
               [--max-queue-wait 5s] [--max-inflight-age 60s] [--max-retired-age 1h] [--json]
               [--canary --url http://host:port/path [--host H] [--expect-status 200] [--canary-max-latency 2s]]
```

`check` prints one plugin line (`OXIROUTE CRITICAL - pool farm 0/4 available | farm_available=0;4;1 ...`)
and exits with the monitoring-plugin codes. **These replace the exit categories in the next section for
this command only**; a monitoring system reads them as written:

| Code | Meaning | Examples |
| --- | --- | --- |
| `0` | OK | Every judged pool has all servers available and nothing below is wrong |
| `1` | WARNING | A server is missing from a pool, a queued request waited longer than `--max-queue-wait`, a stuck request, a refused configuration file, a retired generation draining longer than `--max-retired-age`, a registry lease late for a heartbeat |
| `2` | CRITICAL | A pool has fewer than `--crit-available-below` (default 1) available servers, the process is not ready, an overdue request (a request past its total deadline, which is a router bug), the registry store is degraded, a failed `--canary` |
| `3` | UNKNOWN | The endpoint cannot be reached or does not answer a status page, the answer is not understood, a `--pool` does not exist, or an option is wrong (a usage error is `3`, not `2`) |

`check` reads the public `/status.json`, so it needs no token: use `--endpoint`, or `--socket PATH` for the
read-only status socket of the registry (group-readable, no sudo). Global flags go before `check`.
`--json` (or the global `--output json`) prints one JSON object with `status`, `exitCode`, `summary`,
`findings` and the full `details` page instead of the line. Without `--warn-available-below` a pool warns when
any server is missing; `--max-inflight-age` is not judged unless given. `--canary` also requests the URL
through the router's own listener, with the `Host` of `--host`, and requires `--expect-status`; only plain
`http://` URLs are supported.

## Output And Exit Categories

Stable exit categories are designed for scripts (every command except `check`, see above):

| Code | Category |
| --- | --- |
| `2` | CLI usage |
| `3` | Local input or token file |
| `4` | Transport/connectivity |
| `5` | Authentication |
| `6` | Missing resource |
| `7` | Revision or state conflict |
| `8` | Remote/protocol failure |
| `9` | Intentionally unsupported |

Use JSON output for automation and treat `409`/exit `7` as a reconcile decision, not as permission to
retry an old mutation.

## Examples

```sh
oxiroute config check /etc/oxiroute/oxiroute.kdl
oxiroute --output json server show --pool public-v4 --pool public-v6 origin-a
oxiroute server drain --pool public-v4 --pool public-v6 origin-a
oxiroute generation rollback
oxiroute import nginx /etc/nginx/nginx.conf --output report
oxiroute import squid /etc/squid/squid.conf --output report
oxiroute import apache /etc/httpd/conf/httpd.conf --output report
oxiroute import apache /etc/httpd/conf/httpd.conf --shadow-port-offset 10000 --output preview
oxiroute import varnish /etc/varnish/default.vcl --output report
```

See [MANAGEMENT_CLI.md](../MANAGEMENT_CLI.md) for every subcommand, capability status, revision
guarantee, and HAProxy intent mapping.
