# Troubleshooting

Start with evidence. Do not disable health checks, authentication, or revision checks to make a
symptom disappear.

## Fast Triage

```sh
oxiroute ready
oxiroute status
oxiroute generation status
oxiroute --output json monitoring
oxiroute --output json topology
```

| Symptom | Check | Likely meaning |
| --- | --- | --- |
| The CLI exits `4` with `cannot connect to ... is the daemon running?` | The `management { bind }` address, `--endpoint` or `OXIROUTE_ENDPOINT` | Nothing listens there: the daemon is down, or the endpoint names another port |
| The CLI exits `4` with `did not answer within ...` | The daemon's load and logs; `--timeout` | The daemon accepted the connection but did not answer in time (default 10s per answer); raise `--timeout` only after finding why it is slow |
| `ready --wait` or `generation reload --wait` exits `4` with `did not complete within ...` | The last observation in the message, `generation status`, `events list` | The daemon did not become ready, or the candidate did not become active, within `--timeout` (30s by default) |
| `requests show ID` exits `6` | `requests list` | The request is no longer in flight; only live requests can be shown |
| A list command prints nothing and `no <things>` on stderr | The configuration | There are no such entries; the exit code is `0` |
| `ready` returns nonzero | `/ready`, `status`, generation state | No active non-degraded generation or a traffic listener is not listening |
| Matched HTTP route returns `503` | Pool availability and endpoint health | The route exists, but no endpoint is selectable |
| Health remains `unknown` | Probe path, host, timeout, startup policy | Checks have not completed successfully enough to make the endpoint eligible |
| Dashboard keeps old values | UI stale banner and management listener logs | The last valid sample is retained after a transient refresh failure |
| `generation status` shows `lastRejection` | `diagnostics` in `lastRejection`, the `oxiroute::config` error log | The file on disk was refused and the previous configuration is still running; fix the named setting and save again |
| A server is drained but jobs still run on it | `requests list --pool P`, `server evacuate` report | Drain lets running requests finish. `server evacuate` moves those that sent no response and are replayable; `leftRunning` names why the rest stayed (see the [runbook](RUNBOOK.md#evacuate-a-server)) |
| A drained server takes traffic again | `server list`, `generation status` (`generation`, `activeRevision`) | Administrative state follows pool and server name; a server renamed in the file is a new server and starts ready, and a restart resets it |
| Configuration save returns `409` | Disk and active revisions | Another writer changed the authoritative file; reconcile instead of overwriting |
| `config apply` or `edit` exits `8` with "cannot write" | `status` shows `configWritable: false`; the message names the cause | The daemon's configuration directory is read-only (`ProtectSystem=strict`) or not writable by its user: add `ReadWritePaths=/etc/oxiroute` in a drop-in and make the directory writable by the service group, or edit the file on the host |
| `config apply` or `edit` exits `7` | The file changed on disk since it was read (`409 config_changed`) | Run `config diff --against disk FILE` or `config show`, merge the change and apply again; `edit` keeps your text in the file it names |
| `config validate`, `diff` or `apply` exits `3` | The diagnostics: code, `FILE:LINE`, setting and the `hint:` line | The file is refused; fix the named setting. A format other than the daemon's validates and diffs but needs `--convert` to apply |
| Configuration save returns `422` | `path` and `detail` of each diagnostic (the failing setting and rule), compositional flag | Draft or runtime preflight failed, or a compositional root cannot be flattened |
| Configuration save says restart required | Runtime mode and `/config/listeners` diagnostic | A kept TCP address changed `ipv6_only`, or the direct runtime's `registry` block changed; only a process restart applies it. Other listener changes, a Unix socket mode included, reload live |
| The daemon exited and `status` is refused after a worker was killed | `journalctl -u oxiroute` for `supervised active worker lost` and `exited:` | The supervising master fails closed when its serving worker dies and exits non-zero; the service manager restarts it (`Restart=on-failure` in the packaged unit). See the [runbook](RUNBOOK.md#outage-playbook) |
| Manual recorder button is disabled | Publisher, recorder phase, capability, observed codec | The requested operation is not safe or supported for this stream |
| Import produces a report but no preview | Blocking diagnostic or deployment requirement | The source contains behavior that cannot be represented safely |
| Metrics show large string counters | API shape | Decimal strings preserve exact `u64` values; do not parse through a JavaScript number |

## Origin And Routing

Verify the origin directly before inspecting proxy routing:

```sh
curl -i http://127.0.0.1:3000/healthz
curl -i http://127.0.0.1:8080/healthz
```

Then inspect the topology route and pool. Route precedence is exact host, wildcard host, catch-all;
within a class, the longest path prefix wins and source order resolves the remaining tie. A missing
route is `404`; a matched route with no selectable endpoint is `503`.

## Authentication And Files

Management token failures usually come from the token file rather than the HTTP request:

- the file must be a regular no-follow file;
- mode must be `0400` or `0600`;
- token bytes must be visible ASCII, 32 to 512 bytes after one trailing line ending is removed;
- every authenticated request must contain exactly one bearer header.

The CLI checks `--token-file`, `OXIROUTE_MANAGEMENT_TOKEN_FILE`, the bounded assignment in the
packaged environment file, and then `/etc/oxiroute/management.token`. `ready`, `metrics`, and offline
commands do not read a token. Other commands fail locally with a missing/unreadable token-file error
when no usable path is available; check the selected file as the daemon or operator identity.

Check permissions as the daemon user. Never paste a token into logs or issue it in a shell command
that will be captured by process inspection.

## Import Failures

Use `--emit report`, read blocking errors first, and keep the native source unchanged. A finalized
preview can still contain deployment warnings; reproduce user/group/chroot/logging/process behavior
outside OxiRoute before cutover.

## Restart-Required Configuration

A restart-required validation or save is not a failed write. Read the `I_RESTART_REQUIRED` diagnostic
for the specific cause: the diagnostic names the setting and says which value the running process
keeps. `ipv6_only` of a
kept address and the direct runtime's `registry` block wait for a process restart while the rest of
the file is applied. Adding, removing or moving listeners, and changing the
protocol, service or TLS profile of a listener, reload live (see "Listener changes on reload" in the
operations guide).

Validation reports the candidate as restart-required before writing. A successful save reports
`saved_restart_required`; the canonical file is durable and the rest of it is applied, but the
named setting keeps its running value until the process restarts.

## A Listener Change Was Not Applied

If a saved listener is not serving, look in the service log (`journalctl -u oxiroute`) for
`supervised master kept revision ... could not bind its listeners`. The warning names the listener
and the operating-system error, for example an address in use or a port below 1024 without the
capability to bind it. The running revision keeps serving unchanged, and the reload is retried
within 30 seconds, so fixing the cause is enough. If the log shows nothing, an earlier reload may
still be draining long-lived connections: the log shows `ReplacementCommitted` for that reload and no
`ReplacementCompleted` yet, and the next reload starts when it has finished (3 hours at most by
default).

## Collect A Useful Bug Report

Include:

- OxiRoute version and platform;
- sanitized configuration or the smallest reproducer;
- `status`, generation status, and the relevant monitoring/topology response;
- exact command and exit category;
- whether the failure is startup, validation, activation, protocol, or shutdown.

Do not include bearer tokens, private keys, certificate contents, recording roots, stream query
arguments, or unredacted native source unless the report is being handled securely.
