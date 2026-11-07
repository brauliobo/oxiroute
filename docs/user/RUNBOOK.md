# Runbook: GPU farms and worker pools

Procedures for operators of a LAN farm (GPU render nodes, voice or inference pools) behind OxiRoute.
Commands are verified against the 0.5.4 CLI. Behavior that changes in 6.0 sits in a marked box; check
the [CONFIG_SPEC.md](../CONFIG_SPEC.md) of the installed version before using 6.0 setting names. See
also [OPERATING.md](OPERATING.md), [TROUBLESHOOTING.md](TROUBLESHOOTING.md),
[MANAGEMENT_CLI.md](../MANAGEMENT_CLI.md).

Examples use the pool `gpu-render` with servers `node-a` (`192.0.2.11:9000`), `node-b`, `node-c`; config
fragments use the notation of CONFIG_SPEC (the KDL file uses the same names).

## Conventions

- Global flags (`--output json`, `--endpoint`, `--token-file`) go before the subcommand.
- The token comes from the packaged discovery (`/etc/oxiroute/oxiroute.env`, then
  `/etc/oxiroute/management.token`). For another file export its path, never the secret:
  `export OXIROUTE_MANAGEMENT_TOKEN_FILE=/path/to/management.token` (mode `0400`/`0600`; use `sudo` when
  root-owned). Never put a token in a command line, `curl -H` included. `ready` and `metrics` need none.
- Scratch files hold the whole configuration: `work=$(mktemp -d)` (mode 0700), delete it afterwards.
- A change is done when `oxiroute status` shows `activeRevision` equal to the new disk revision.

## Add a machine to a pool

1. Start the worker and probe its health path from another host: `curl -si http://192.0.2.13:9000/__fit`
   must answer `200` (see [worker-side health](#worker-side-health-recipes)).
2. Edit the active configuration as JSON, validate, apply:

```sh
oxiroute --output json config get > "$work/cfg.json"
jq '(.config.upstream_pools[] | select(.name=="gpu-render") | .servers) +=
    [{"name":"node-c","endpoint":{"type":"socket","address":"192.0.2.13:9000"},"max_connections":1}]' \
  "$work/cfg.json" > "$work/new.json"
oxiroute config diff "$work/new.json"        # only the new server may differ
oxiroute config validate "$work/new.json"
oxiroute config apply "$work/new.json"       # revision-checked, atomic write, live reload
oxiroute server show --pool gpu-render node-c
```

The server starts `unknown` and turns `healthy` after its first successful probe. With `first`, order is
priority: an appended server only receives overflow. `apply` writes through the daemon, so the service
account needs write access to the configuration directory (else `write_failed`, nothing changes). A root
with templates or native references answers `422 E_COMPOSITIONAL_ROOT`: use the file flow below. `config
get` redacts RTMP token secrets, so never round-trip a configuration with RTMP services through `apply`.

## Remove a machine

```sh
oxiroute server drain --pool gpu-render node-c
oxiroute server show --pool gpu-render node-c          # wait for 0 active leases
oxiroute --output json config get > "$work/cfg.json"
jq '.config.upstream_pools |= map(if .name=="gpu-render"
      then .servers |= map(select(.name!="node-c")) else . end)' "$work/cfg.json" > "$work/new.json"
oxiroute config validate "$work/new.json" && oxiroute config apply "$work/new.json"
```

A dead machine needs no drain. A pool cannot have zero servers (`422`): remove the pool and its routes
together. Or keep known machines listed while off: health takes them out of rotation and their return needs
no apply.

## Drain and un-drain

```sh
oxiroute server drain --pool gpu-render node-a        # no new work; leases finish; checks continue
oxiroute server ready --pool gpu-render node-a
oxiroute server maintenance --pool gpu-render node-a  # drain plus checks suspended
oxiroute pool drain gpu-render                         # every server of the pool
oxiroute server drain --pool gpu-render --pool voice-tts node-a   # same name in several pools
```

Drain is runtime state; nothing is written to the file. `server list` shows it next to observed health.

> **0.5.4: drain resets on every generation change.** A file write, `generation reload`, `config apply`,
> rollback or supervised worker replacement returns all servers to `ready`; a restart does too. After
> any change run `oxiroute --output json server list` and re-drain what must stay drained (for example a
> machine in use by its owner).
>
> **Arrives in 6.0:** drain, maintenance, health override, check switch and max-connections override
> follow a server by pool and server name across every generation change. A removed server loses its
> state, a new one starts `ready`, a restart still resets it.

## Apply configuration safely

Prefer the JSON flow (`config get`, `diff`, `validate`, `apply`): `validate` prepares the complete runtime
against the running process and `apply` refuses a stale base (`409`). Use the file flow for a hand-written
file with comments or templates:

```sh
cfg=/etc/oxiroute/oxiroute.kdl
cp -p "$cfg" "$cfg.bak-$(date +%Y%m%d%H%M%S)"            # rollback point
cp -p "$cfg" "$work/oxiroute.kdl" && "$EDITOR" "$work/oxiroute.kdl"
oxiroute config check "$work/oxiroute.kdl"               # offline: syntax and runtime preparation
cp -p "$work/oxiroute.kdl" "$cfg.new" && mv "$cfg.new" "$cfg"   # same directory: atomic rename
oxiroute status                                          # activeRevision follows within seconds
```

- `config check` prepares a full generation, including binding listeners, so on the host that runs those
  listeners it fails for that reason: check on a staging host, or use `config validate`.
- Write by rename in the same directory with the original owner and mode; the watcher observes the parent
  directory. `systemctl reload oxiroute` or `oxiroute generation reload` forces an immediate reconcile.
- In-flight requests finish on the retired worker. A listener address that cannot be bound rejects the
  whole reload; the old generation keeps serving and the journal names the listener.

**A rejected file write.**

- *0.5.4:* silent. The old generation keeps serving, `/ready` stays OK, and there is no log line, event
  or status field. About 5 s after a write, check that `activeRevision` equals the disk revision in
  `oxiroute status`. `generation reload` then answers `422 E_INVALID_VALUE` without naming the setting;
  find it with `config check` on a copy.
- *Arrives in 6.0:* logged once at error level (`oxiroute::config`) with the failing setting and, for KDL
  syntax, the line; `events list` shows `configuration_reload` / `rejected`;
  `oxiroute_config_rejections_total` rises; `generation status` shows `lastRejection` and
  `degraded: true` until the next acceptable file (`/ready` stays OK). `422` and `config check` print
  `<code>: <path>: <detail>`.

## Roll back a generation

1. Restore the backup by atomic rename: `cp -p "$cfg.bak-STAMP" "$cfg.new" && mv "$cfg.new" "$cfg"`, then
   confirm with `oxiroute status` and re-drain (0.5.4).
2. `oxiroute generation rollback` re-activates the retained previous generation, but in supervised mode
   it answers `409 previous_missing` right after a replacement. In 6.0 supervised mode `previousRevision`
   is informational only. The file restore is the rollback.

Never restart to roll back: it cuts in-flight requests and resets runtime state.

## Outage playbook

```sh
oxiroute ready; oxiroute status; oxiroute generation status
oxiroute pool list; oxiroute --output json server list    # health, lastFailure, passive state
curl -s http://127.0.0.1:9900/metrics | grep -E 'oxiroute_pool_(available|queue)|passive_ejected'
```

**All backends down.** Immediate empty `503` with `x-oxiroute-pool-unavailable: 1`. A pool never queues
when no server is selectable; the queue only waits while servers exist but are at capacity. Read
`lastFailure` (`connect_failed`, `timeout`, `unexpected_status`), fix the workers, and expect recovery
after `down_interval_ms` plus `healthy_threshold` x `fast_interval_ms`.

**Only backend unhealthy.** The app is up but its health path fails (for example a pending migration), so
the single-server pool fails closed with an empty `503`. Verify the origin directly, then
`oxiroute server set-health --pool P S up` so the app serves its own error page; undo with `auto`.
Permanent fix: a `tcp` check or a liveness path that never touches the database. *6.0:*
`resilience.fail_open`.

**Saturation.** `oxiroute_pool_queue_depth` above 0, active connections at the caps,
`oxiroute_pool_queue_timeouts_total` rising (those callers get `503` after `queue_timeout_ms`). Add
machines, lengthen `queue_timeout_ms` to the callers' patience, or for an emergency
`oxiroute server max-connections set --pool P S 2` then `max-connections reset`. Never exceed what the
GPU's VRAM holds.

**Hung backend.** It accepts and never answers; its leases stay held. Set it to `maintenance`, then kill
the process on the worker; OxiRoute cannot cancel a request in flight. In 0.5.4 the read timeout is the
pool `server_timeout_ms` when set (it shadows route `read_timeout_ms`), capped by the service
`upstream_io_timeout_ms`, which is also the total deadline. A retry on `response_timeout` then has no
budget left and the client sees a dropped connection, not a status. Keep short-call and long-job pools
apart. *6.0:* per-phase `policy.timeouts` and `504` with `x-oxiroute-reason`.

**Bad config.** Nothing changed after a write: see the rejected-write detection. Applied but wrong:
[roll back](#roll-back-a-generation).

**Retired generation blocked by an idle keep-alive.** Only one old worker drains at a time, for up to
3 hours (`OXIROUTE_RELOAD_DRAIN_SECONDS`), and a client holding an idle keep-alive connection counts as
in-flight work, so the next reload stays pending (`generation status` shows a candidate; the journal has
`ReplacementCommitted` without `ReplacementCompleted`). Close or restart the client holding it
(`ss -tn state established '( sport = :PORT )'` lists peers) or wait; restart OxiRoute only when nothing
is in flight. Prevent it: set listener `downstream_timeouts.keepalive_timeout_ms`, keep
`OXIROUTE_RELOAD_DRAIN_SECONDS` in `/etc/oxiroute/oxiroute.env` below your longest job, and list known
machines permanently so applies are rare.

## Hardening recipe (0.5.4)

```
upstream_pools = { {
  name = "gpu-render", algorithm = "first", queue_timeout_ms = 600000, connect_timeout_ms = 2000,
  servers = { { name = "node-a", endpoint = { type = "socket", address = "192.0.2.11:9000" }, max_connections = 1 } },
  health_check = { type = "http", path = "/__fit", timeout_ms = 2000, interval_ms = 5000,
                   fast_interval_ms = 1000, down_interval_ms = 5000,
                   unhealthy_threshold = 2, healthy_threshold = 1, startup = "checking" },
  passive_health = { observe = "layer7", error_limit = 2, initial_backoff_ms = 5000, max_backoff_ms = 60000 },
} }
-- proxy action of the route:
policy = { retry = { max_retries = 3, delay_ms = 250, target = "next_server",
                     triggers = { "connect_failure", "connect_timeout", "empty_response" },
                     response_statuses = { 502, 503 } } }
```

- **Set `passive_health` explicitly.** In 0.5.4 a pool without it never ejects a server, although the
  specification describes defaults (observed; fixed in 6.0). Without it a dead backend costs errors until
  `unhealthy_threshold` probes fail.
- **Fast recovery.** `down_interval_ms` of 10000 or less, `fast_interval_ms` 1000. A 60 s down interval
  hides a restarted machine for a minute. `timeout_ms` must not exceed `interval_ms`.
- **Statuses count as passive failures only if the route lists them in `response_statuses`.** Add `500`
  only where it means a sick backend. Workers that answer `503 busy` under load would be ejected under
  `layer7`; use `observe = "layer4"` (connect failures and timeouts only) there.
- **Retry triggers.** `connect_failure` and `connect_timeout` sent no bytes, so any method may retry.
  `empty_response`, `response_timeout`, `junk_response` and statuses replay only what the method safety
  allows. A node's pool-unavailable `503` is redispatched automatically (0.5.4). `max_retries` is at most 3
  and below the server count.
- **Method safety.** The default (`get_head` + `empty`) never replays a `POST` after bytes were sent.
  `method_safety = "all"` + `body_safety = "buffered"` also needs route `request_buffering = true` and a
  positive `max_request_body_bytes`; use it only where every request is idempotent, and never with
  `response_timeout` on routes with side effects: the job runs on two machines.
- **Deadlines and capacity.** Service `upstream_io_timeout_ms` is the longest legitimate job (progress does
  not extend it); give short calls their own service and pool. `max_connections` is concurrent jobs per
  GPU; `queue_timeout_ms` bounds the wait for a slot (`null`: no queue, immediate `503`).

> **Arrives in 6.0** (optional; defaults keep the strict behavior):
>
> ```
> resilience = { fail_open = true, min_members = 1, ejection_jitter_percent = 20,      -- on the pool
>                queue_while_unavailable_ms = 15000, unavailable_retry_after_seconds = 5,
>                unavailable_body = "pool unavailable" }
> policy = { retry = { ..., jitter_percent = 50, idempotency_header = "Idempotency-Key",
>                      budget = { per_second = 20, ratio_percent = 20 } },
>            timeouts = { connect_ms = 2000, header_ms = 30000, idle_ms = 60000, total_ms = 3600000 },
>            rescue = { statuses = { 404 }, target = "next_server" } }
> ```
>
> - `fail_open`: a pool with no selectable server serves the least bad one instead of an empty `503`;
>   `min_members` stops ejection from emptying the pool; `queue_while_unavailable_ms` waits for a
>   recovering member. The unavailable `503` always has a current `Date`.
> - `504` with `x-oxiroute-reason` (`connect_timeout`, `upstream_header_timeout`,
>   `upstream_write_timeout`, `total_deadline`) replaces the dropped connection; attempts are clamped, so a
>   hang leaves retry budget.
> - `rescue`: replay on another server when a backend answers a listed status, for example a `404` from an
>   Ollama node that lacks the model, without ejecting it (`target = "pool"` plus `pool` goes to another pool).
> - `idempotency_header` (needs `method_safety = "all"`): a non-`GET`/`HEAD` request is replayed after bytes
>   were sent only when it carries the header; the worker must honour the key. `budget` caps retries per
>   second and as a share of recent requests, so retries cannot amplify an outage.

## Worker-side health recipes

The probe uses the server's own address and port (no separate check port), so the health path must be
served on the traffic port. Rules for `/__fit`:

- `200` while busy. A `503` while busy makes busy servers unhealthy, leaves the pool with no selectable
  server, and callers get an immediate empty `503` instead of queueing.
- `503` when the worker cannot do useful work: stale NFS mount, or free VRAM below the threshold while the
  engine is not already resident. The worker then needs no self-drain; health removes it after about
  `unhealthy_threshold` x `interval_ms` and restores it after `healthy_threshold` x `fast_interval_ms`.
- Never wake an idle engine (no connection to a socket-activated port); never touch a database.

If the engine cannot serve `/__fit`, put a node-local OxiRoute on the traffic port with two routes: path
`{ kind = "exact", value = "/__fit" }` to a `fit-gate` pool and `{ kind = "segment_prefix", value = "/" }`
to the `engine` pool. The gate (stdlib Python) probes in a thread, so a hung NFS mount or `nvidia-smi`
shows as a stale result instead of blocking the answer.

```python
import os, subprocess, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MOUNT, GPU, MIN_FREE_MIB, UNIT = "/mnt/models", "0", 4800, "engine@0.service"
state = {"ok": False, "why": "starting", "at": 0.0}

def probe():
    os.listdir(MOUNT)                                    # hangs or raises on a stale NFS mount
    if subprocess.run(["systemctl", "is-active", "--quiet", UNIT], timeout=3).returncode == 0:
        return True, "resident"                          # the engine's own VRAM must not fail the gate
    query = ["nvidia-smi", "-i", GPU, "--query-gpu=memory.free", "--format=csv,noheader,nounits"]
    free = int(subprocess.check_output(query, timeout=3))
    return free >= MIN_FREE_MIB, f"free_mib={free}"

def loop():
    while True:
        try:
            ok, why = probe()
        except Exception as error:                       # report it, never answer 200 for a failed probe
            ok, why = False, type(error).__name__
        state.update(ok=ok, why=why, at=time.time())
        time.sleep(2)

class Gate(BaseHTTPRequestHandler):
    def do_GET(self):
        fresh = time.time() - state["at"] < 6            # a stuck probe thread counts as failing
        body = (state["why"] if fresh else "probe stalled").encode()
        self.send_response(200 if state["ok"] and fresh else 503)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

threading.Thread(target=loop, daemon=True).start()
ThreadingHTTPServer(("127.0.0.1", 9100), Gate).serve_forever()
```

Test with `curl -si http://127.0.0.1:9100/__fit` while the NFS server is stopped and while another
process holds the VRAM.

## HA and local-first

- Each worker host runs its own OxiRoute with a node listener in front of its local worker pool (`first`,
  local GPUs in order). The front pool (`gpu-render`, `first`) lists the local node first, then the other
  machines: local capacity is used before the LAN, and a machine receives only overflow once the earlier
  ones are at `max_connections`. Use `least_connections` for an even spread instead.
- A node whose local servers are all drained answers `503` with `x-oxiroute-pool-unavailable: 1` and the
  front redispatches to the next node when `max_retries` is positive (0.5.4). Add `passive_health` to the
  front pool so the node is not asked again on every request.
- Run two front instances (`front-a`, `front-b`) with identical configuration pushed by the file flow:
  canary on `front-b`, verify `oxiroute status` and traffic, then `front-a`. Give clients both addresses,
  or a floating address managed outside OxiRoute (VRRP). Runtime state is per instance in 0.5.4: drain on both.

## Monitoring and alerts

`/metrics` and `/ready` are public on the management port, which is loopback only: scrape from the host
or from a configured statistics bind. Metric names are verified against the exporter.

```yaml
groups:
- name: oxiroute
  rules:
  - {alert: OxirouteDown, expr: 'up{job="oxiroute"} == 0', for: 1m}
  - {alert: PoolHasNoServers, expr: 'oxiroute_pool_available_servers == 0', for: 1m, labels: {severity: page}}
  - {alert: ServerUnhealthy, expr: 'oxiroute_server_health{state="unhealthy"} == 1', for: 10m}
  - {alert: ServerEjectedRepeatedly, expr: 'increase(oxiroute_server_passive_ejections_total[15m]) >= 3'}
  - {alert: PoolQueueBacklog, expr: 'oxiroute_pool_queue_depth > 0', for: 15m}   # tune to job length
  - {alert: PoolQueueTimeouts, expr: 'increase(oxiroute_pool_queue_timeouts_total[10m]) > 0'}
  - alert: HighErrorRatio
    expr: |
      sum by (listener) (rate(oxiroute_http_requests_total{result=~"server_error|upstream_error|timeout"}[5m]))
        / sum by (listener) (rate(oxiroute_http_requests_total[5m])) > 0.05
    for: 5m
  - {alert: GenerationFailing, expr: 'increase(oxiroute_generation_failures_total[15m]) > 0 or oxiroute_generation_degraded == 1'}
```

- The request counter is per listener, not per pool: give each pool its own listener for a per-pool error
  ratio. `oxiroute_http_request_duration_milliseconds` is the latency histogram.
- `oxiroute_pool_available_servers` counts selectable servers (healthy, not drained, not ejected): page
  on zero, and compare against the expected count to catch forgotten drains.
- *Arrives in 6.0:* also alert on `increase(oxiroute_config_rejections_total[15m]) > 0` and
  `increase(oxiroute_pool_fail_open_selections_total[5m]) > 0` (serving degraded); `rescue` replays are
  counted by `oxiroute_http_rescues_total`.
