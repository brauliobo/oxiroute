# Runbook: GPU farms and worker pools

Procedures for operators of a LAN farm (GPU render nodes, voice or inference pools) behind OxiRoute.
Commands and settings are those of 6.0; where a 0.5.x install behaves differently a note says so. The
setting names are defined in [CONFIG_SPEC.md](../CONFIG_SPEC.md) and the commands in
[CLI.md](../reference/CLI.md). See also [OPERATING.md](OPERATING.md),
[TROUBLESHOOTING.md](TROUBLESHOOTING.md), [MANAGEMENT_CLI.md](../MANAGEMENT_CLI.md).

Examples use the pool `gpu-render` with servers `node-a` (`192.0.2.11:9000`), `node-b`, `node-c`; config
fragments use the notation of CONFIG_SPEC (the KDL file uses the same names).

## Conventions

- Global flags (`--output json`, `--endpoint`, `--token-file`) go before the subcommand.
- The token comes from the packaged discovery (`/etc/oxiroute/oxiroute.env`, then
  `/etc/oxiroute/management.token`). For another file export its path, never the secret:
  `export OXIROUTE_MANAGEMENT_TOKEN_FILE=/path/to/management.token` (mode `0400`/`0600`; use `sudo` when
  root-owned). Never put a token in a command line, `curl -H` included. `ready`, `metrics` and `check` need
  none. With a `registry` block, `oxiroute --socket /run/oxiroute/status.sock ...` reads status and
  `server list` as any member of the socket's group, without a token and without `sudo`.
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

## Let machines register themselves

A pool with a `registration` block (see [CONFIG_SPEC](../CONFIG_SPEC.md#dynamic-server-registration))
accepts workers that announce themselves: no file edit, no new generation, no root token, no drain
reset. Registrations are leases: a worker that stops renewing leaves rotation within its TTL, and
everything it was serving finishes first.

```sh
# once, on the oxiroute host: a token for the workers of one pool
oxiroute registry token create --tokens-file /etc/oxiroute/registry.tokens --id gpu-render --pool gpu-render

# on a worker on the same host: the register socket needs no token
oxiroute --socket /run/oxiroute/register.sock server register --pool gpu-render node-c \
  --address 192.0.2.13:9000 --max-connections 1 --ttl 15s --heartbeat

# on a LAN worker: the TCP endpoint with that token (mode 0600 file with the printed secret)
oxiroute --endpoint http://192.0.2.5:9911 --token-file /etc/oxiroute/worker.token \
  server register --pool gpu-render node-d --address 192.0.2.14:9000 --heartbeat

# anyone in the status group: read-only, no sudo
oxiroute --socket /run/oxiroute/status.sock server list --pool gpu-render
```

`--heartbeat` is the whole agent: a systemd unit running it registers at start, renews at a third
of the TTL, registers again if the lease was lost, and on `SIGTERM` announces `draining` and
deregisters. Without `--heartbeat` the command registers once and the lease runs out after its TTL.
`oxiroute server deregister --pool gpu-render node-c [--missing-ok]` removes a registration at once.
`server list --origin dynamic` shows only registered machines, with their lease and `status`: `active`,
`shadowed` (a configured server of the same name or address wins) or `orphaned` (the pool stopped opting in). Operators drain a registered worker like any other (`server drain --pool P NAME`); the
decision is stored with the registration, so it survives re-registration, reloads and restarts. A
server in the configuration file wins over a registration of the same name or address.

## Remove a machine

A registered machine: stop its agent (`systemctl stop`), which drains and deregisters it; its requests finish.
From the host, `oxiroute server drain --pool gpu-render node-c` then `server deregister` does the same
by hand, and a dead machine simply leaves when its lease runs out. A machine in the configuration file:

```sh
oxiroute server drain --pool gpu-render node-c
oxiroute server show --pool gpu-render node-c          # wait for 0 active leases
oxiroute --output json config get > "$work/cfg.json"
jq '.config.upstream_pools |= map(if .name=="gpu-render"
      then .servers |= map(select(.name!="node-c")) else . end)' "$work/cfg.json" > "$work/new.json"
oxiroute config validate "$work/new.json" && oxiroute config apply "$work/new.json"
```

A dead machine needs no drain. A pool cannot have zero servers (`422`) unless it has a `registration`
block: remove the pool and its routes together. Or keep known machines listed while off: health takes
them out of rotation and their return needs no apply.

## Drain and un-drain

```sh
oxiroute server drain --pool gpu-render node-a        # no new work; leases finish; checks continue
oxiroute server ready --pool gpu-render node-a
oxiroute server maintenance --pool gpu-render node-a  # drain plus checks suspended
oxiroute pool drain gpu-render                         # every server of the pool
oxiroute server drain --pool gpu-render --pool voice-tts node-a   # same name in several pools
```

Drain is runtime state; nothing is written to the file. `server list` shows it next to observed health.
Drain, maintenance, health override, check switch, `max-connections` and `weight` overrides follow a
server by pool and server name across every generation change (file write, `generation reload`,
`config apply`, rollback, supervised worker replacement) and are in place before the new generation
takes traffic. A removed server loses its state and a new one starts `ready`. A restart resets them,
except the drain of a registered machine, which is stored with its registration.

Drain waits for running requests to end; to free the machine now, [evacuate it](#evacuate-a-server).

> **0.5.4:** drain resets on every generation change and on restart. After any change run
> `oxiroute --output json server list` and re-drain what must stay drained (for example a machine in use
> by its owner).

## Evacuate a server

Drain waits for running requests. When the owner needs the machine now (a render that takes twenty minutes,
a speech job on a workstation), `evacuate` is drain plus a one-shot action: the requests running on the
server are cancelled and each one is dispatched again to another server of the same pool. The caller sees
no error; it gets the response of the second server, with the same `x-request-id`.

```sh
oxiroute server evacuate --pool gpu-render node-a             # moves what can move
oxiroute server evacuate --pool gpu-render --pool voice-tts node-a
oxiroute pool evacuate gpu-render voice-tts                   # every server of the pools
oxiroute server evacuate --force --pool gpu-render node-a     # also kills what cannot move
```

The state is `drain`: `server list` shows it, it follows the server by pool and server name across
generations exactly like drain, and `oxiroute server ready --pool gpu-render node-a` hands the machine back.
Only the cancel is one-shot; a later generation or restart never repeats it.

**What moves.** An HTTP request (HTTP/1.1 or HTTP/2 from the client, to an HTTP/1.1 or HTTP/2 backend, on a
reverse proxy route) is moved while no response byte reached the caller and the backend has not answered
with its response header. It must also be replayable under the route's `retry` policy, because the new
backend receives the whole request again:

- a `GET` or `HEAD` without a body always qualifies;
- any other method needs `method_safety = "all"` and `body_safety = "buffered"` with route
  `request_buffering = true` and a positive `max_request_body_bytes`, and, when the route sets
  `idempotency_header`, that header present and non-empty on the request;
- a body still arriving is replayed only while what arrived fits the retry buffer.

The route needs no retries to be moved: `max_retries` can be `0`, and a moved request spends neither retry
budget nor attempt count. It does need those method and body rules. A farm that submits jobs with `POST`
should have the clients send an idempotency key and the route accept it:

```
policy = { retry = { max_retries = 0, method_safety = "all", body_safety = "buffered",
                     idempotency_header = "Idempotency-Key" },
           timeouts = { total_ms = 3600000 } }
-- and on the route: request_buffering = true, max_request_body_bytes = 1048576
```

The backend must honour the key: a job moved after the first server started it may run a second time if the
first server did not actually stop. The destination is chosen like for any request (never a server already
tried, the pool queue and `queue_while_unavailable_ms` apply), and a request may be evacuated at most 3 times.

**What does not move** stays running and finishes as in drain. The command prints one count per reason:

```sh
oxiroute --output json server evacuate --pool gpu-render node-a
{ "outcome": "evacuated", "force": false, "cancelledAndRedispatched": 3, "forced": 0,
  "leftRunning": { "response_started": 1, "not_replayable": 1 } }
```

| `leftRunning` reason | Meaning |
| --- | --- |
| `response_started` | the backend answered (a streaming response or a download in progress) |
| `not_replayable` | method, idempotency header or body policy of the route does not allow a replay |
| `body_not_replayable` | the request body received so far outgrew the retry buffer |
| `upgraded` | WebSocket or another upgraded connection |
| `limit_reached` | already evacuated 3 times |
| `no_destination` | no other server can take it now (all drained, down or at capacity, and the pool has no queue) |

Counts are per request, summed over every server and pool of the call. Read them as: `cancelledAndRedispatched`
left the machine; every `leftRunning` request is still using it, so the machine is free only when they end
(`oxiroute requests list --pool gpu-render` lists them with their server and age). Repeat the command after
fixing the cause (for example once capacity returns for `no_destination`) or add `--force`.

`--force` also cancels what cannot move, without a second attempt: a caller whose response did not start gets
`503` with `x-oxiroute-reason: evacuated`; when the response had started the connection is closed. Movable
requests are still moved, not killed. Use it when the machine must be free regardless of the callers.

**Limits.** Only HTTP requests of reverse proxy routes are moved. WebSocket and other upgrades,
forward proxy and `CONNECT` tunnels, HTTP/3 reverse proxy requests, TCP and UDP relays and RTMP are
drain only: the server leaves rotation and the requests finish (`--force` closes upgraded connections, and
does not affect the other kinds). The report counts HTTP requests only. The route's total deadline keeps running
during the move; a request that cannot be served before it ends gets the normal `504`, so long jobs already
need `total_ms` above their runtime.

**What to watch.** `oxiroute_evacuated_requests_total{pool,server,result}` (`redispatched`, `left_running`,
`forced`, `failed`; `failed` is a cancelled request that then could not be served and ended with an error),
`oxiroute_server_attempts_total{outcome="evacuated"}` on the machine that was freed, and the `server_evacuation`
event in `/api/v2/events` (pool, server and the counts). Evacuation is audited like drain.

### Worked example: a machine needed by its owner

`gpu-render` runs Blender jobs and `voice-tts` runs speech synthesis, both on `node-a` (192.0.2.11) and
`node-b` (192.0.2.12). The owner of `node-a` wants it back.

```sh
oxiroute --output json server evacuate --pool gpu-render --pool voice-tts node-a
# { "outcome": "evacuated", "force": false, "cancelledAndRedispatched": 4, "forced": 0,
#   "leftRunning": { "response_started": 2 } }
```

Four jobs went to `node-b` and their callers keep waiting for one answer. Two responses were already
streaming from `node-a`; they finish normally. Check `oxiroute requests list --pool gpu-render`, wait for them
or run the command with `--force` to cut them, then tell the owner. Later `oxiroute server ready --pool
gpu-render --pool voice-tts node-a` returns the machine to the pools.

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

**A rejected file write.** The old generation keeps serving and `/ready` stays OK, but the refusal is not
silent: it is logged once at error level (`oxiroute::config`) with the failing setting and, for KDL syntax,
the line; `events list` shows `configuration_reload` / `rejected`; `oxiroute_config_rejections_total` rises;
`generation status` shows `lastRejection` and `degraded: true` until the next acceptable file; and
`oxiroute check` exits `1` with `configuration refused N s ago`. `422` and `config check` print
`<code>: <path>: <detail>`. (0.5.4 gave no signal: compare `activeRevision` with the disk revision about 5 s
after every write.)

## Roll back a generation

1. Restore the backup by atomic rename: `cp -p "$cfg.bak-STAMP" "$cfg.new" && mv "$cfg.new" "$cfg"`, then
   confirm with `oxiroute status` (`activeRevision` equals the restored file) and `oxiroute check`. Drain
   and the other server overrides stay as they were.
2. `oxiroute generation rollback` re-activates the retained previous generation in the direct runtime. Under
   the supervising master (the packaged service) it is unavailable and `previousRevision` in
   `generation status` is informational only: the file restore is the rollback. Registrations are not part
   of a generation, so a rollback neither loses nor restores them.
3. The replaced generation drains like any other: its requests finish on it, and `generation status` lists
   it under `retiredGenerations` until they do. If the next apply stays pending, see
   [a reload stays pending](#outage-playbook).

Never restart to roll back: it cuts in-flight requests and resets runtime state.

## Outage playbook

```sh
oxiroute check                                           # one line: which pool, what is wrong; exit 0/1/2/3
oxiroute ready; oxiroute status; oxiroute generation status
oxiroute pool list; oxiroute --output json server list    # health, lastFailure, passive state
oxiroute requests list --older-than 30s                  # requests stuck inside the router, oldest first
curl -s http://127.0.0.1:9900/metrics | grep -E 'oxiroute_pool_(available|queue)|passive_ejected|canary'
```

Start with `check`: its findings name the pool and the symptom (`pool gpu-render 0/4 available`,
`2 overdue requests`, `configuration refused 40 s ago`, `pool gpu-render canary failed
(unexpected_status)`), and the sections below say what to do about each.

**All backends down.** Immediate empty `503` with `x-oxiroute-pool-unavailable: 1`. A pool never queues
when no server is selectable; the queue only waits while servers exist but are at capacity. Read
`lastFailure` (`connect_failed`, `timeout`, `unexpected_status`), fix the workers, and expect recovery
after `down_interval_ms` plus `healthy_threshold` x `fast_interval_ms`. The `503` carries a current
`Date`, and `Retry-After` and a short body when `resilience.unavailable_retry_after_seconds` and
`unavailable_body` are set. With `resilience.queue_while_unavailable_ms` callers wait for a recovering
server instead. `check` exits `2`.

**Only backend unhealthy.** The app is up but its health path fails (for example a pending migration), so
the single-server pool fails closed with an empty `503`. Verify the origin directly, then
`oxiroute server set-health --pool P S up` so the app serves its own error page; undo with `auto`.
Permanent fix: a `tcp` check or a liveness path that never touches the database, or
`resilience.fail_open = true`, which makes the pool serve its least bad server (the application's own error
page) instead of an empty `503`; `oxiroute_pool_fail_open_selections_total` and `failOpen` in `/status.json`
tell that it is happening.

**Saturation.** `oxiroute_pool_queue_depth` above 0, active connections at the caps,
`oxiroute_pool_queue_timeouts_total` rising (those callers get `503` after `queue_timeout_ms`). Add
machines, lengthen `queue_timeout_ms` to the callers' patience, or for an emergency
`oxiroute server max-connections set --pool P S 2` then `max-connections reset`. Never exceed what the
GPU's VRAM holds.

**Hung backend.** It accepts and never answers; its leases stay held. Run `oxiroute server evacuate --pool P S`:
the requests that sent no response and are replayable move to another server and the hung machine is drained,
so there is no need to kill the worker process unless the report shows requests left running
(see [Evacuate a server](#evacuate-a-server)); `--force` cancels those too. `oxiroute requests list --pool P
--older-than 60s` shows which requests are waiting and in which phase (`awaiting_headers` means the worker
took the request and said nothing). The route's `policy.timeouts` (`connect_ms`, `header_ms`, `idle_ms`,
`total_ms`) bound each phase; when one fires the client gets `504` with `x-oxiroute-reason`
(`connect_timeout`, `upstream_header_timeout`, `upstream_write_timeout`, `total_deadline`) and a retry still
has budget. A request past its total deadline plus 5 s is a router bug: `check` exits `2` with `N overdue
requests` and `oxiroute_http_inflight_overdue` is above 0. Keep short-call and long-job pools apart.
(0.5.4 dropped the connection instead of answering.)

**Bad config.** Nothing changed after a write: see the rejected-write detection. Applied but wrong:
[roll back](#roll-back-a-generation).

**A reload stays pending behind a retired generation.** Since 6.0 an idle keep-alive connection no
longer holds a replaced generation: it is closed when the next revision is applied. What can still
hold one is work in progress (a long request, a WebSocket, a tunnel, a relay). Only one replaced
worker drains at a time under the supervising master, so the next reload stays pending
(`generation status` shows a candidate; the journal has `ReplacementCommitted` without
`ReplacementCompleted`) until that work ends or the `retirement.drain_deadline_ms` of the replaced
configuration passes, when the worker is stopped. Check by applying twice during a long request:
the second apply activates when the request ends. `generation status` lists the draining generations as
`retiredGenerations`, with their age and in-flight counts, and a full `max_retired_generations` shows
`pendingApply`. `check` warns when one drains longer than `--max-retired-age` (default 1 h).

**A canary fails.** `check` reports `pool P canary failed (reason)` with reason `connect_failed`, `timeout`,
`unexpected_status` or `protocol_error`: one failed run is a WARNING, two in a row a CRITICAL. The canary
asks the pool the client's question, so a failing canary with all servers `healthy` means the health path is
shallower than the work: the status page of the server it hit is in `canary.server`
(`oxiroute check --json`, `details.pools[].canary`). Test the same path against that worker by hand, then
fix the worker or deepen its health path. A canary that is `busy` was skipped because the pool was at
capacity, which is not a failure.

**A registered worker vanished.** Its lease ran out: it left rotation within its TTL, and `oxiroute server
list --pool P` no longer lists it (`check` warns `late for a heartbeat` while a lease has under a third of
its TTL left). Restart the worker's agent; it registers again by itself.

## Hardening recipe

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

- **Set `passive_health` explicitly.** A pool without it does no passive accounting and never ejects a
  server. Without it a dead backend costs errors until `unhealthy_threshold` probes fail. A successful
  response clears the failure run, so unrelated errors spread over time no longer eject a healthy server.
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

Optional hardening (defaults keep the strict behavior):

```
resilience = { fail_open = true, min_members = 1, ejection_jitter_percent = 20,      -- on the pool
               queue_while_unavailable_ms = 15000, unavailable_retry_after_seconds = 5,
               unavailable_body = "pool unavailable" }
canary = { path = "/__fit", interval_ms = 30000, timeout_ms = 5000, max_latency_ms = 2000 }  -- on the pool
-- proxy action of the route:
policy = { retry = { ..., jitter_percent = 50, idempotency_header = "Idempotency-Key",
                     budget = { per_second = 20, ratio_percent = 20 } },
           timeouts = { connect_ms = 2000, header_ms = 30000, idle_ms = 60000, total_ms = 3600000 },
           rescue = { statuses = { 404 }, target = "next_server" } }
```

- `fail_open`: a pool with no selectable server serves the least bad one instead of an empty `503`;
  `min_members` stops ejection from emptying the pool; `queue_while_unavailable_ms` waits for a
  recovering member. The unavailable `503` always has a current `Date`.
- `504` with `x-oxiroute-reason` (`connect_timeout`, `upstream_header_timeout`,
  `upstream_write_timeout`, `total_deadline`) replaces the dropped connection; attempts are clamped, so a
  hang leaves retry budget.
- `rescue`: replay on another server when a backend answers a listed status, for example a `404` from an
  Ollama node that lacks the model, without ejecting it (`target = "pool"` plus `pool` goes to another pool).
- `idempotency_header` (needs `method_safety = "all"`): a non-`GET`/`HEAD` request is replayed after bytes
  were sent only when it carries the header; the worker must honour the key. `budget` caps retries per
  second and as a share of recent requests, so retries cannot amplify an outage.
- `canary`: a request every `interval_ms` through the pool's own server selection, so it proves the path a
  client takes, not only that each server answers its probe. It skips (`busy`) instead of taking a slot
  from a client, and a failing run shows in `oxiroute check` and `oxiroute_canary_*`. Point it at the same
  worker-side health path or at a deeper one; plain HTTP only.

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

MOUNT, GPU, MIN_FREE_MIB, UNIT = "/path/to/models", "0", 4800, "engine@0.service"
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
process holds the VRAM. On the front OxiRoute the stale mount then shows as `pool gpu-render 3/4
available` in `oxiroute check` within `unhealthy_threshold` x `interval_ms`, and a `canary` with
`path = "/__fit"` proves the gate is reachable through the pool as a client would reach it.

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
  or a floating address managed outside OxiRoute (VRRP). Runtime state (drain, overrides, registrations) is per
  instance: drain on both. Workers that register themselves (`server register --heartbeat`) can announce to
  both fronts, one agent per front.
- Monitor each front with its own `oxiroute check` and a `canary` on the front pool; a front is healthy when
  `check` exits `0`, whatever the other front does.

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
- Also alert on `increase(oxiroute_config_rejections_total[15m]) > 0` (a refused file) and
  `increase(oxiroute_pool_fail_open_selections_total[5m]) > 0` (serving degraded); `rescue` replays are
  counted by `oxiroute_http_rescues_total`.
- `increase(oxiroute_evacuated_requests_total{result="failed"}[15m]) > 0` means a cancelled request could not
  be served by another server; `result="left_running"` shows what an evacuation did not free.
- `oxiroute_http_inflight_overdue > 0` is a router bug and should page. `oxiroute_canary_success == 0` and
  `time() - oxiroute_canary_last_success_timestamp_seconds > 120` mean a pool's client-side path fails.
  `oxiroute_generation_retired_age_seconds` shows a replaced generation that does not drain.

## Monitor with `oxiroute check`

`oxiroute check` is the one command for cron and monitoring systems. It prints one line and exits with the
plugin codes: `0` OK, `1` WARNING, `2` CRITICAL, `3` UNKNOWN (it could not reach or understand the
endpoint, or an option is wrong). These codes replace the exit categories 2 to 9 of the other commands;
see [CLI.md](../reference/CLI.md#check-monitoring-exit-codes). It reads the public `/status.json`, so it
needs no token: over `--endpoint` on the host, or over `--socket` as any member of the status socket's
group, without `sudo`.

```sh
oxiroute check
# OXIROUTE OK - 2 pools, 8/8 servers available | gpu-render_available=4;4;1 gpu-render_queued=0 ...
oxiroute check --pool gpu-render --warn-available-below 3 --crit-available-below 2
# OXIROUTE WARNING - pool gpu-render 2/4 available | gpu-render_available=2;3;2 ...
oxiroute --socket /run/oxiroute/status.sock check --max-inflight-age 2h --json
```

What it judges: the process is ready; every pool has enough available servers (`--warn-available-below`,
default any missing; `--crit-available-below`, default 1); no queued request waited longer than
`--max-queue-wait` (5 s); no request is older than `--max-inflight-age` (off by default; size it to the
longest legitimate job); no overdue or stuck requests; the last configuration file was accepted; no
replaced generation drains longer than `--max-retired-age` (1 h); registered leases are renewed and the
registry store is healthy; and the pool canaries answer. `--canary --url http://host:port/path
[--host H]` additionally requests the URL through the listener and requires `--expect-status`.
`--json` prints `status`, `exitCode`, `summary`, `findings` and the whole page as `details`.

A systemd timer that fails its unit, and so triggers `OnFailure=`, whenever the farm is not OK:

```ini
# /etc/systemd/system/oxiroute-check.service
[Unit]
Description=OxiRoute farm check
OnFailure=oxiroute-alert@%n.service

[Service]
Type=oneshot
ExecStart=/usr/bin/oxiroute --socket /run/oxiroute/status.sock check --pool gpu-render --warn-available-below 3

# /etc/systemd/system/oxiroute-check.timer
[Timer]
OnCalendar=minutely
AccuracySec=10s

[Install]
WantedBy=timers.target
```

`journalctl -u oxiroute-check` keeps the line of every run. Plain cron mails only when something is wrong:

```cron
*/5 * * * * out=$(oxiroute --socket /run/oxiroute/status.sock check); rc=$?; [ $rc -ne 0 ] && echo "$out" | mail -s "oxiroute check rc=$rc" ops@example.com
```

Nagios, Icinga and NRPE take the line and the code as they are: `command[check_oxiroute]=/usr/bin/oxiroute
--socket /run/oxiroute/status.sock check --warn-available-below 3`. Treat `3` as "cannot tell" and alert on
it too: a router that cannot be asked is not known to be healthy.
