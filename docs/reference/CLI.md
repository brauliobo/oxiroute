# CLI Reference

The `oxiroute` binary is both the daemon and the offline/operator client. `oxr` is the packaged
short alias. `oxiroute CONFIG` stays shorthand for `serve CONFIG` when `CONFIG` is an existing file or
looks like a path (it contains `/` or ends in `.kdl`, `.kdl2`, `.lua`, `.uci`, `.hocon` or `.conf`); any
other unknown word is a usage error with suggestions, so a mistyped command never starts the daemon.
Without arguments the help is printed.

## Global Options

```sh
oxiroute [--context NAME] [--endpoint URL | --socket PATH] [--token-file FILE] [--timeout DURATION] [--output table|json|plain] [--quiet] COMMAND
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
- `--context NAME` (or `OXIROUTE_CONTEXT`) selects a named context (see "Contexts") whose address, token
  source and timeout apply where the flags and variables above are not given. Precedence, highest first:
  flags, variables, the context (`--context`, `OXIROUTE_CONTEXT`, then the current one of the contexts file),
  discovery of the packaged setup. A context's token is never sent to an endpoint given by `--endpoint` or
  `OXIROUTE_ENDPOINT`.
- `--output json` writes one JSON value to stdout; diagnostics use stderr. Read commands print the
  management API body unchanged, so its field names stay the stable contract for scripts.
- Public `/ready` and `/metrics` probes do not need the management token. Other management operations
  use the bearer token according to their route contract.

Squid report output includes `capabilities`, a versioned target-checkout registry. Its `parity` is
`partial` and `completeParity` is false while any family or directive is partial, unsupported,
obsolete, or not planned. Only forms with both runtime integration and failure coverage are marked
`compatible`.

## Contexts

A context names a daemon: its management address, where its token comes from and a default timeout. They live
in `$XDG_CONFIG_HOME/oxiroute/contexts` (`~/.config/oxiroute/contexts` when `XDG_CONFIG_HOME` is unset), a JSON
file the CLI creates with mode `0600` in a `0700` directory and refuses to read when it has another mode. The
file never holds a token: a `token` key must be an object naming a source, and a file with a token written into
it is refused with a message that does not repeat it.

```json
{
  "schemaVersion": 1,
  "current": "prod",
  "contexts": {
    "prod": {
      "endpoint": "http://192.0.2.10:9900",
      "token": { "file": "/etc/oxiroute/prod.token" },
      "timeout": "15s"
    },
    "staging": { "endpoint": "http://192.0.2.20:9900", "token": { "env": "STAGING_TOKEN" } },
    "lab": { "endpoint": "unix:/run/oxiroute/lab.sock" }
  }
}
```

A token source is `{"file": PATH}` (absolute; a regular file with mode `0400` or `0600`, as for
`--token-file`), `{"env": VARIABLE}` (the variable holds the token itself, read at each use) or
`{"command": [PROGRAM, ARG...]}` (run without a shell, standard input closed, standard output is the token, for
a secret manager). A token has the same bounds everywhere: 32 to 512 visible ASCII characters.

| Command | What it does |
| --- | --- |
| `overlay show` | The settings kept with `--persist`: target, setting, value and `status` (`applied`, `diverged` after a later runtime change, `orphan` when the target is gone), with the file's revision and state (`empty`, `applied`, `diverged`, `unreadable`) |
| `overlay clear` | Deletes the overlay file; the running state does not change |
| `overlay promote [--wait]` | Writes the overlay's connection limits into the configuration file in its own format (comments and logic are not kept; a composed file is refused) and removes them from the overlay; states, health, checks, weights and listener states stay in it |
| `context list` | One row per context: `*` for the current one, name, address, token source as `file:PATH`, `env:VARIABLE` or `command:PROGRAM`, and timeout. The token itself is never printed; `--output json` carries `tokenSource` and `tokenRef` |
| `context add NAME --address URL [--token-from-file FILE \| --token-from-env VARIABLE \| --token-from-command PROGRAM ARG...] [--default-timeout 15s] [--use] [--replace]` | Adds a context, or replaces one of the same name with `--replace`; `--use` makes it current. `--token-from-command` takes every word after it, so give it last |
| `context use NAME` | Makes a context the current one |
| `context remove NAME` | Removes a context, and the current mark when it was the current one; the token file or variable it named is left alone |
| `context current` | Prints the name of the context the next command uses (`--context` and `OXIROUTE_CONTEXT` win over the current one); exits `3` when there is none |

An unknown context, a context file that is not valid or has another mode than `0600`, and a token source that
cannot be read or does not hold a valid token exit `3`.

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
(a registered server's drain or maintenance, stored with its registration), `overlay` or `mixed`. Add
`--persist` to `listener`/`pool`/`server` `ready`, `drain`, `maintenance`, `set-health`, `check`,
`max-connections`, `weight` and `evacuate` to keep the change across restarts in the overlay file
(`<configuration file>.overlay.json`, mode 0600) beside the configuration; the table output then adds a
`persistence` column. The configuration file is left alone until `overlay promote`.

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
| Monitoring | `top` | Live terminal dashboard of listeners, upstreams, clients, connections, requests, RTMP and generations; one measured snapshot with `--once`, `--json`, `--csv` or without a terminal |
| Contexts | `context list|add|use|remove|current` | Name daemons and their token sources and pick the one commands talk to (see "Contexts") |
| Documentation | `completions`, `man`, `version` | Shell completions, the manual page and the build version |
| Lifecycle | `generation status|reload|rollback|drain [--wait]` | Prepare, publish, revert, or drain generations; `drain --wait` waits for the traffic references up to `--timeout` |
| Configuration | `config check|compose|get|show|validate|diff|apply|edit|fmt|history|rollback` | Check, compare and change the daemon's configuration in its own format (see "Configuration commands") |
| Events | `events list|follow` | Page through the bounded in-memory event ring, or follow its server-sent event stream with reconnect |
| Audit | `audit list|status|tail` | Query and tail the redacted audit log of administrative actions |
| Setup | `init proxy|static|rtmp`, `token create|rotate` | Write a minimal valid configuration in the format you choose, and create or rotate the management token file |
| Diagnosis | `doctor` | Check the configuration, listen addresses, upstreams, certificate files, file modes and the systemd unit's sandbox in one prioritised report with monitoring exit codes |
| Requests | `requests list|show` | Inspect the requests currently inside the router |
| Routing | `route test` | Ask the daemon which route, action and pool would serve a URL, or a host and path, with the route table of the active generation |
| Runtime inventory | `listener`, `pool`, `server` | Read or safely mutate admission/health state |
| TLS | `tls list|show|inspect|check|reconcile|renew|revoke|delete|acme` | Certificate inventory with UTC expiry and time left, live handshake inspection, an expiry check with monitoring exit codes, renewal and ACME control |
| RTMP | `rtmp stream`, `rtmp publisher`, `rtmp viewer`, `rtmp recorder`, `rtmp relay`, `rtmp recording` | Inspect live streams, publishers and viewers, disconnect sessions, control recorders, and list, download and delete recordings with their quota |
| Migration | `import nginx|haproxy|squid|apache|varnish` | Emit offline reports or finalized previews |
| Build | `version` | Print the build version |

`oxiroute requests list [--pool P] [--route R] [--older-than 30s] [--phase queued] [--limit N]` lists
the requests currently inside the router, oldest first, and `oxiroute requests show REQUEST_ID`
shows one. Phases are `routed`, `queued`, `connecting`, `awaiting_headers`, `streaming` and
`retry_wait`. There is no command to cancel one request; `server evacuate` and `pool evacuate` move the
replayable requests of a server (see below).

`oxiroute route test https://example.com/api/users` (or `route test HOST PATH`, optionally with `--method` and `--listener`) shows which route, action and pool would serve the request on each HTTP listener, using the route table of the active generation, without sending it. It exits 6 when no route serves it.

## `init` and `token`: first run

```sh
oxiroute init proxy                       # writes ./oxiroute.kdl
oxiroute token create --token-file ./management.token
oxiroute doctor oxiroute.kdl
oxiroute serve oxiroute.kdl
```

`init` writes a configuration that loads, validates and prepares: `proxy` (HTTP reverse proxy to one pool),
`static` (static files from `public/`, which `init` creates) or `rtmp` (an RTMP `live` application), with
a management API on `127.0.0.1:9900` (the CLI's default endpoint). `--format` chooses `kdl` (default), `lua`,
`uci` or `hocon`; `--file -` prints instead of writing, and an existing file is kept unless `--force`.
`token create` writes the management token as 64 random hex characters in a file only its owner can read
(`--token-file`, else the selected context's token file) and never replaces one; `token rotate` swaps in a new token atomically with the same owner and mode; the
daemon takes the new token at its next reload (`generation reload` with the old token, or `SIGHUP`) or restart. Neither prints the token.

## `top`

```sh
oxiroute top [--interval 1s] [--window 2s|10s|60s|life] [--once] [--json] [--csv]
             [--view listeners|upstreams|clients|conns|requests|rtmp|generations]
             [--filter EXPR] [--sort COLUMN] [--reverse] [--top N]
             [--no-mouse] [--ascii] [--color auto|always|never] [--theme dark|light|mono] [--reset-layout]
```

A read-only live dashboard with a listeners view, an upstream pool and server tree, clients, connections,
in-flight requests, RTMP streams and generations, with sparkline meters, mouse support, an incremental
filter, rates over a 2s, 10s or 60s window and a layout that can be collapsed, hidden and remembered
(`$XDG_STATE_HOME/oxiroute/top.json`; `--reset-layout` forgets it). It reads the management API with the
management token (`--token-file`); registry sockets do not serve statistics. `--once`, `--json` (schema
version 2: the view's rows with their units), `--csv` or a missing terminal take two samples one window apart,
print one measured view and exit. The clients and connections views need a server that tracks them and say so
on one line when it does not. Keys, columns, the JSON schema and semantics:
[Live dashboard](../MANAGEMENT_CLI.md#live-dashboard).

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
and honours `--output`. `server ready` un-drains (`undrain` is an alias of `ready` for servers, pools and listeners). Details and limits:
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

## `tls`: certificate expiry

```sh
oxiroute tls list [--expiring-within 30d]
oxiroute tls show NAME
oxiroute tls inspect HOST:PORT [--servername NAME] [--ca-file FILE]
oxiroute tls check [HOST:PORT] [--certificate NAME]... [--warn 30d] [--crit 7d]
                   [--servername NAME] [--verify [--ca-file FILE]] [--json]
```

`list` and `show` print the expiry of every certificate in UTC with the time left (`in 29d04h`, or
`expired 2h05m ago`) and `--expiring-within` keeps the certificates that expire within that long or already
have. `inspect` handshakes with any TLS server and prints the certificate it presents; it never trusts the
chain to read it and reports separately whether the chain and the name would be trusted (extra roots with
`--ca-file`).

`tls check` is a monitoring plugin like `oxiroute check`, and its exit codes replace the exit categories for
this command only: `0` OK, `1` WARNING (a certificate expires within `--warn`, 30d by default), `2` CRITICAL
(within `--crit`, 7d by default, expired, or not trusted with `--verify`), `3` UNKNOWN (the daemon or the
server cannot be reached, a `--certificate` does not exist, `--crit` is longer than `--warn`, or an option is
wrong). Without `HOST:PORT` it judges the certificates the daemon reports, with the management token; with it,
the certificate that server presents, without a token. Durations take `ms`, `s`, `m`, `h` and `d`.

```sh
oxiroute tls check --warn 21d --crit 7d
# OXIROUTE TLS WARNING - www expires 2026-12-02T10:00:00Z (in 14d02h) | 'www'=1216800s;1814400;604800;0
oxiroute tls check example.com:443 --verify
```

`renew`, `reconcile`, `revoke`, `delete` and `acme rollover|pause|resume|cancel` call the ACME and
reconciliation endpoints the dashboard uses and follow the usual exit categories; `revoke` and `delete` need
`--yes`.

## `doctor`

`oxiroute doctor [CONFIG] [--unit FILE] [--offline] [--json]` answers "why will this not start, or not
serve?" on the host that runs the daemon. It reads the configuration file and the systemd unit, so it works
before the daemon is installed or started:

| Check | What it looks at |
| --- | --- |
| `config` | the file parses, validates and prepares as a runtime generation (the listen addresses are checked by `listeners`); configuration warnings are reported as warnings |
| `files` | the configuration is not writable by group or others; the management token file is a regular file with mode `0400` or `0600`; private keys, DNS credentials, access token and htpasswd files and the registry token file are not open to other users; the overlay file (`<file>.overlay.json`) loads and is not open to group or others; the history directory (`<file>.history/`) and its versions are private |
| `contexts` | the contexts file is valid and private (mode `0600`), the selected context exists, and the token files the contexts name are regular files with mode `0400` or `0600` holding a token: critical for the selected context, a warning for the others; an unresolvable context also stops the upstream probes |
| `listeners` | each listen address can be bound: in use by another process, not assigned to this host, a socket directory that does not exist, a port below 1024 without root or `CAP_NET_BIND_SERVICE` (the unit's `AmbientCapabilities=` counts). With a daemon answering at `--endpoint`, the addresses it holds are fine; `--offline` does not ask |
| `upstreams` | each static pool server accepts a connection within the global `--timeout`: a pool with no reachable server is critical, one with some unreachable is a warning per server |
| `certificates` | the expiry of the certificate files (`files` and Certbot sources): warning within 30d, critical within 7d or expired |
| `unit` | every `ReadWritePaths=` entry exists unless prefixed `-`; every path the configuration writes is writable under the unit's `ProtectSystem=`, `ReadWritePaths=`, `StateDirectory=` and similar settings; with a management API the configuration directory must be writable for `config apply`, `--persist` and the history (a warning) |

The unit is `--unit`, else the first `oxiroute.service` in `/etc/systemd/system`, `/run/systemd/system`,
`/usr/lib/systemd/system` or `/lib/systemd/system`, with its drop-ins. Without `CONFIG` the file named by the
unit's `ExecStart=... serve FILE` is read, else `oxiroute.kdl`. The unit's `Environment=` and
`EnvironmentFile=` stand for the daemon's environment (`OXIROUTE_MANAGEMENT_TOKEN_FILE`, `OXIROUTE_AUDIT_DIR`).

Problems print first, worst first, each with the command or setting that fixes it; passing checks follow.
Like `check`, its exit codes replace the exit categories: `0` OK, `1` WARNING, `2` CRITICAL, `3` UNKNOWN (an
option is wrong). `--json` prints the report as one object with a `findings` array of `severity`, `check`,
`subject`, `message` and `fix`.

```sh
oxiroute doctor /etc/oxiroute/oxiroute.kdl
# OXIROUTE DOCTOR CRITICAL - 1 critical, 1 warning, 5 ok
# CRITICAL unit         /usr/lib/systemd/system/oxiroute.service: disk cache pages /var/cache/oxiroute is read-only under the unit's sandbox
#     fix: add ReadWritePaths=/var/cache/oxiroute to the unit (systemctl edit oxiroute.service) and restart it
# WARNING  upstreams    pool web: app-2 (192.0.2.12:8080): connection refused
#     fix: start the server, or fix its address, firewall and DNS; `oxiroute server drain` takes it out of rotation
```

## Output And Exit Categories

Stable exit categories are designed for scripts (every command except `check`, `tls check` and `doctor`, see above):

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

The commands that run until they are stopped (`events follow`, `audit tail`) end with status `130` on
SIGINT and `143` on SIGTERM, after flushing what they printed, unless `--count` ends them first with `0`.

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
| `config history` | Lists the earlier versions of the daemon's file that it keeps (every save through the API keeps the text it replaces): id (12 hexadecimal digits of the SHA-256 of the text), save time, size, and whether the entry equals the stored text. They are bounded by the `config_history` settings (20 versions, 16 MiB by default), kept as `0600` files in `<file>.history/` next to the configuration file, and edits made directly on disk are not kept |
| `config show --rev ID` | Prints one version from `config history` (any prefix of at least 6 digits, or `current`), exactly as kept; inline RTMP secrets are shown as `<redacted:...>` placeholders. A version that no longer validates is withheld because its secrets cannot be located |
| `config diff --rev A [--rev B]` | Lists the settings that changed from version A to version B (`current`, the stored file, when only A is given), with the same markers as `config diff FILE`; `--text` adds the unified diff of the redacted texts and `--exit-code` exits `1` when the versions differ. It needs no file |
| `config rollback ID [--wait]` | Restores a version as the daemon's file. It is a normal validated apply: it uses the stored revision as a precondition (exit `7` when the file changed meanwhile), refuses a version that no longer validates (exit `3`, nothing written) and prints the `apply` result. The text it replaces becomes the newest entry of `config history`, so the rollback is itself a revision that can be undone. `--wait` returns once the restored revision is active |
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
