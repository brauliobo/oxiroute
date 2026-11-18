# CLI Reference

The `oxiroute` binary is both the daemon and the offline/operator client. `oxr` is the packaged
short alias. `oxiroute CONFIG` stays shorthand for `serve CONFIG` when `CONFIG` is an existing file or
looks like a path (it contains `/` or ends in `.kdl`, `.kdl2`, `.lua`, `.uci`, `.hocon` or `.conf`); any
other unknown word is a usage error with suggestions, so a mistyped command never starts the daemon.
Without arguments the help is printed.

## Global Options

```sh
oxiroute [--endpoint URL | --socket PATH] [--token-file FILE] [--timeout DURATION] [--output table|json|plain] [--quiet] COMMAND
```

The global options work before and after the command name (`oxiroute status --output json`).

- `--endpoint` defaults to `OXIROUTE_ENDPOINT`. Without it the CLI looks for a local daemon: the
  `OXIROUTE_ENDPOINT` assignment in `/etc/oxiroute/oxiroute.env`, then the management `bind` of
  `/etc/oxiroute/oxiroute.kdl` or `/etc/oxiroute/oxiroute.lua`, then `http://127.0.0.1:9900`.
- `--token-file` explicitly selects the token file and wins over all automatic discovery.
- `OXIROUTE_MANAGEMENT_TOKEN_FILE` is the next source; when unset, the packaged
  `/etc/oxiroute/oxiroute.env` assignment is checked, then the management `token_file` of the packaged
  configuration, followed by the built-in `/etc/oxiroute/management.token` path when that file exists.
- `--timeout DURATION` (`500ms`, `15s`, `2m`) limits the connection attempt and each answer. Without it the
  CLI waits 5s to connect and 10s for an answer. A daemon that accepts the connection but does not answer
  is reported as `did not answer within <limit>` and a connection attempt that hangs as `cannot connect to
  <endpoint> within <limit>`; both exit 4, with different text from `cannot connect ...; is the daemon
  running?` (nothing listens) and `stopped answering the request` (the daemon closed the connection).
- `--output json` writes one JSON value to stdout; diagnostics use stderr. Read commands print the
  management API body unchanged, so its field names stay the stable contract for scripts.
- Public `/ready` and `/metrics` probes do not need the management token. Other management operations
  use the bearer token according to their route contract.

Squid report output includes `capabilities`, a versioned target-checkout registry. Its `parity` is
`partial` and `completeParity` is false while any family or directive is partial, unsupported,
obsolete, or not planned. Only forms with both runtime integration and failure coverage are marked
`compatible`.

## Waiting

`oxiroute ready --wait` keeps asking `/ready` until the daemon answers 200, which suits a service manager
or a deploy script that starts the daemon and continues once it serves. A refused connection or a `503`
counts as not ready yet. `oxiroute generation reload --wait` requests the reload, then polls
`generation status` until the candidate revision is the active revision (it prints that status) or the
file is rejected (exit 8, naming the first diagnostic). Both wait up to `--timeout`, 30s when it is not
given, and then exit 4 with `<what> did not complete within <limit>: <last observation>`. Without `--wait`,
`reload` returns as soon as the daemon accepts the request.

## Completions And Manual Page

`oxiroute completions bash|zsh|fish` prints a completion script and `oxiroute man` prints the manual page
(roff). Neither needs a daemon or a token. `--name oxr` produces the script or page for the short alias; the
packages install both names (`/usr/share/bash-completion/completions/{oxiroute,oxr}`,
`/usr/share/zsh/site-functions/_{oxiroute,oxr}`, `/usr/share/fish/vendor_completions.d/{oxiroute,oxr}.fish`,
`/usr/share/man/man1/{oxiroute,oxr}.1`). To try one without a package:

```sh
source <(oxiroute completions bash)
oxiroute man | man -l -
```

## Output

`--output table` (the default) prints what an operator reads: aligned columns with units (`1.5K`,
`2m05s`, `3.8M`), short revisions, and `-` for a missing value. A single object (`status`,
`generation status`, `config get`, `server show`) prints as aligned `label  value` lines. `status`
leads with the build version, health state, active revision and generation age, then lists the
listeners; `monitoring` adds process, host and traffic figures, listeners and pools; `config get`
prints the configuration format, revisions and the number of entries per section instead of the
document (use `--output json` for the document).

Commands that change runtime state (`listener`/`pool`/`server` `ready`, `drain`, `maintenance`, `set-health`,
`check`, `max-connections`, `weight`, `evacuate`, `refresh-dns`, `generation rollback|drain`, `process drain`)
print `persistence`: `runtime` (this process only: it survives reloads but not a restart), `registry`
(a registered server's drain or maintenance, stored with its registration) or `mixed`. To keep a change on a
configured server across a restart, edit the configuration file (`config edit`).

`--output plain` prints one `key=value` record per line with raw numbers and no padding, for `grep`,
`awk` and `read`. Keys carry their unit as a suffix (`age_ms`, `received_bytes`); values with spaces
are double-quoted; an unknown value is left out. A command that prints more than one kind of record
starts each line with `record=<name>`.

A command that finds nothing prints no table and one line on stderr (`no certificates`,
`no requests`, `no streams`) and exits 0. `requests show ID` for an id that is not in flight exits 6.
`oxiroute metrics` prints the Prometheus text exposition unchanged in `table` and `plain`.

Commands that change a pool, server or listener print one row per target (`TARGET SETTING VALUE
CHANGE`). `CHANGE` is `changed` when the target had another value before, `unchanged` when it already
had it (draining a drained server) and `applied` when the previous value cannot be compared (resetting
a weight). The comparison reads the target first, so `--output json` skips it and passes the server
body through, where `changed` counts the targets of the request.

## Command Families

| Family | Commands | Use |
| --- | --- | --- |
| Process | `serve`, `status`, `ready`, `metrics`, `monitoring`, `topology`, `drain`, `shutdown` | Start and inspect the daemon |
| Monitoring | `check` | One line and a Nagios exit code for cron and monitoring systems |
| Monitoring | `top` | Live terminal dashboard of listeners, pools, servers and requests; one snapshot with `--once`, `--json` or without a terminal |
| Documentation | `completions`, `man`, `version` | Shell completions, the manual page and the build version |
| Lifecycle | `generation status|reload|rollback|drain` | Prepare, publish, revert, or drain generations |
| Configuration | `config check|compose|get|show|validate|diff|apply|edit|fmt` | Check, compare and change the daemon's configuration in its own format (see "Configuration commands") |
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

## `top`

```sh
oxiroute top [--interval 1s] [--once] [--json] [--view listeners|pools|servers|requests]
             [--filter TEXT] [--sort COLUMN] [--reverse]
```

A read-only live dashboard with a listeners, pools, servers and requests view, rates and latency
percentiles computed from successive snapshots, sorting, name filtering and pause. It reads the management
API with the management token (`--token-file`); registry sockets do not serve statistics. `--once`,
`--json` (the snapshot document of `GET /api/v1/stats`) or a missing terminal print one snapshot and exit.
Keys, columns and semantics: [Live dashboard](../MANAGEMENT_CLI.md#live-dashboard).

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
| `2` | CRITICAL | A pool has fewer than `--crit-available-below` (default 1; a pool with `registration` and no servers yet is OK with a note unless `--warn-available-below` or `--crit-available-below` is given) available servers, the process is not ready, an overdue request (a request past its total deadline, which is a router bug), the registry store is degraded, a failed `--canary` |
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

## Configuration commands

The configuration commands speak the native formats of the daemon: KDL, Lua, HOCON, UCI, and a JSON
document (HOCON is a superset of JSON). A file's format comes from its extension (no extension is KDL)
or from `--format kdl|lua|hocon|uci|json`.

| Command | What it does |
| --- | --- |
| `config check FILE` | Offline: parse, validate and prepare a file without a daemon. Exit `3` and `oxiroute: ...` on stderr when it is refused |
| `config show [FILE]` | Prints the authored text of the file the daemon runs, exactly as stored (`--disk`, the default), or the running configuration rendered canonically (`--active`; comments and Lua logic do not exist there). With `FILE` it prints that file and needs no daemon. Inline RTMP secrets in the daemon's text are shown as `<redacted:...>` placeholders that are restored when the text is applied |
| `config get` | The typed JSON view of the stored configuration: format, revisions, entries per section (`--output json` for the document) |
| `config validate FILE` | Asks the daemon to resolve the file exactly as it loads its own (parse, templates, native references, typed validation, runtime preparation) without storing it. Prints `valid` with the format, candidate and active revisions, whether a restart is required, and any warnings. A refused file exits `3` with one block per diagnostic: its code, `FILE:LINE` for a syntax error (there is no column), the failing setting's path and the rule it broke, and a `hint:` line |
| `config diff FILE` | Lists the settings the file changes against the running configuration (`--against disk` for the stored one), one line each: `+` added, `-` removed, `~` changed with `old -> new`, `^` reordered; list items are addressed by name, as in `/upstream_pools/web/servers/origin-1/max_connections`. Secret values are `<redacted>`. `--text` adds the unified diff of the authored text against the stored file. Equal prints nothing on stdout. `--exit-code` exits `1` when there are differences (the text diff counts only with `--text`) and `0` when there are none; without it the exit code is `0` either way |
| `config apply FILE` | Validates the file, replaces the daemon's configuration file with its text (comments, formatting and Lua logic are kept) using the stored revision as a precondition, and lets the daemon activate it. `--dry-run` validates and writes nothing. `--wait` returns once the new revision is active, up to `--timeout` (30s by default), and fails with the rejection reason when the daemon refuses the saved file. Applying text equal to the stored file writes nothing and says so |
| `config edit` | Opens `$VISUAL`, else `$EDITOR`, else `vi` on a private copy of the stored text, validates it, prints the changes and the text diff, asks for confirmation on a terminal (`--yes` skips it and is required without one) and applies it with the revision precondition (`--wait` as for `apply`). An empty edit or no change cancels. A refused edit offers to reopen the editor on a terminal, and otherwise exits `3`; the edit is kept in a file whose path is printed on every refusal, conflict or declined confirmation |
| `config fmt FILE` | Rewrites a KDL, Lua, HOCON or UCI file in the canonical form of the typed configuration: it prints it, or `--write` replaces the file, `--check` prints nothing and exits `1` when the file is not canonical, `--diff` prints the unified diff and exits `1` when there is one. Comments and the author's ordering are not preserved, defaults are written out, and a file with templates or native references is refused instead of expanded |
| `config compose FILE...` | Prints the merge of finalized configurations in a format; like `fmt` it renders the typed result, so comments are not preserved |

A file in another format than the daemon's own validates and diffs, but `config apply` stores text and
refuses a text in a format the daemon's file is not in (`3`): `--convert` stores the equivalent text
in the daemon's format instead, without the comments and logic of the file. A JSON document, such as the
`config` of `config get --output json` (also accepted inside its envelope), always converts. Native
references in a file resolve against the directory of the daemon's configuration file.

When the daemon cannot write its file (its status reports `configWritable: false`: a read-only mount, as
under the packaged unit's `ProtectSystem=strict`, or a directory its user may not write), `apply` and
`edit` stop before writing with exit `8` and a message that names the cause and the fix (a
`ReadWritePaths=/etc/oxiroute` drop-in and a directory the service group may write, or editing the file
on the host). `config diff --exit-code` and `config fmt --check|--diff` use exit `1`, "ran and found a
difference"; the categories `2` to `9` are unchanged.

## Import

`oxiroute import nginx|haproxy|squid|apache|varnish CONFIG` reads the native file (a relative `CONFIG` is
relative to the current directory; `nginx --root-prefix` only roots the absolute paths inside it). A
`CONFIG` that cannot be read exits `3`. `--emit report` (the default) prints the evidence as JSON, always in
JSON whatever `--format` says; `--emit preview` prints the finalized configuration in `--format`. Both exit
`1` while blockers remain: the report is still printed, a preview is not, and `stderr` lists each blocker
with its code, `FILE:LINE` and reason. For nginx it also names the overlay flags that can resolve blockers
(`--default-access-log-file`, `--x-accel-controls-absent`, `--upstream-tls`, `--bearer-token-file`,
`--recording-root`, `--default-error-server`, `--host-timezone`). Exit `0` means no blockers.

## Examples

```sh
oxiroute config check /etc/oxiroute/oxiroute.kdl
oxiroute config diff --exit-code --text candidate.kdl
oxiroute config apply --wait candidate.kdl
oxiroute config edit
oxiroute --output json server show --pool public-v4 --pool public-v6 origin-a
oxiroute server drain --pool public-v4 --pool public-v6 origin-a
oxiroute generation rollback
oxiroute import nginx /etc/nginx/nginx.conf --emit report
oxiroute import nginx /etc/nginx/nginx.conf --emit preview --format kdl > candidate.kdl
oxiroute import squid /etc/squid/squid.conf --emit report
oxiroute import apache /etc/httpd/conf/httpd.conf --emit report
oxiroute import apache /etc/httpd/conf/httpd.conf --shadow-port-offset 10000 --emit preview
oxiroute import varnish /etc/varnish/default.vcl --emit report
```

See [MANAGEMENT_CLI.md](../MANAGEMENT_CLI.md) for every subcommand, capability status, revision
guarantee, and HAProxy intent mapping.
