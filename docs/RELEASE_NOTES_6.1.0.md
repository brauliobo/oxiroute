# OxiRoute 6.1.0

OxiRoute 6.1.0 lets an operator free a machine immediately: `evacuate` is `drain` plus a one-shot move of
the requests running on it. The release remains pre-alpha and retains conservative compatibility claims.

## Summary

- **Evacuate.** `oxiroute server evacuate` and `oxiroute pool evacuate` drain a server or a pool, and
  `server evacuate` also moves its in-flight HTTP requests to other servers of the same pool. This replaces
  killing the worker process to reclaim a machine that runs minutes-long renders or speech jobs.
- **Fixes.** Failed listener binds report the address and the cause, clients that reconnect after a closed
  connection are no longer rejected by `max_connections`, and an RTMP recording keeps its recoverable partial
  segment after a late shutdown timeout and reports a finished segment without waiting for the next media event.
- **Packaging.** Release binaries no longer embed the builder's cargo paths. Dependency audit findings in the
  recordings toolchain and the load generator are resolved.

Read [Upgrade notes](#upgrade-notes) before installing.

## Evacuate a server or a pool

```sh
oxiroute server evacuate [--force] --pool <pool> [--pool <pool> ...] <server>
oxiroute pool evacuate [--force] <pool> [<pool> ...]
```

The management API offers the same operations as `POST /api/v1/servers/evacuate` and
`POST /api/v1/pools/evacuate`.

- `server evacuate` drains the server and cancels its in-flight HTTP requests. Each one is dispatched again to
  another server of the same pool; the caller gets the response of the second server, with the same
  `x-request-id`, and sees no error.
- `pool evacuate` drains every server of the pool. Its requests have nowhere to move, so by default they keep
  running and are reported as `no_destination`. To move requests to other servers, evacuate the servers one
  by one. `--force` cuts the requests that keep running.
- A request moves only while no response byte was sent and the backend has not answered with its response
  header, and only when the route's `retry` policy allows replaying it (`method_safety`,
  `idempotency_header`, `body_safety`, `request_buffering`). Retries do not need to be enabled; a moved
  request spends neither retry budget nor `max_retries`. A request moves at most 3 times and the route's total
  deadline keeps running.
- The command prints a report: `cancelledAndRedispatched`, `forced` and `leftRunning` by reason
  (`response_started`, `not_replayable`, `body_not_replayable`, `upgraded`, `limit_reached`,
  `no_destination`). Requests left running finish as in drain. `--force` also cancels them: callers get `503`
  with `x-oxiroute-reason: evacuated`, or a closed connection when the response had started.
- The state is `drain`: it follows the server by pool and server name across generations, and
  `oxiroute server ready` hands the machine back. The cancel itself is one-shot and is never replayed by a
  later generation or a restart.

### Observability

- `oxiroute_evacuated_requests_total{pool,server,result}` counts `redispatched`, `left_running`, `forced`
  and `failed` requests.
- `oxiroute_server_attempts_total` has a new attempt outcome, `evacuated`, on the machine that was freed.
- The `server_evacuation` event (one per server, or one per pool for `pool evacuate`) appears in the event
  log, and the dashboard events view lists it. Evacuation is audited as `server_evacuate` and `pool_evacuate`.

## Known limits

- Only HTTP requests of reverse proxy routes are moved. WebSocket and other upgrades, forward proxy and
  `CONNECT` tunnels, HTTP/3 reverse proxy requests, TCP, UDP and RTMP traffic are drain only (`--force`
  closes upgraded connections). The report counts HTTP requests only.
- A request that was started on the evacuated server may run a second time on the new one if the first server
  did not actually stop. Backends of routes that replay `POST` requests must honour the idempotency key.
- A moved request still has to finish before the route's total deadline; long jobs already need `total_ms`
  above their runtime.

## Upgrade notes

- **Restart the services after the package upgrade.** The package does not restart them, so running
  processes keep the previous binary. The evacuate commands and API need the new master and workers: restart
  the service, after checking that no long job is running, because a restart ends in-flight requests.
- Configuration formats are unchanged: a 6.0 configuration loads unchanged and no field was added or removed.
  The `evacuate` actions need no configuration, but moving non-`GET` requests needs the route `retry` policy
  described in the [runbook](user/RUNBOOK.md#evacuate-a-server).
- The vendored Pingora proxy crate carries a small additive hook that lets a cancelled upstream attempt be
  dispatched again. Nothing changes for deployments that never run `evacuate`, and the vendored Pingora
  version is the same as in 6.0. The advisories listed in the 6.0.0 known limits still apply.

See [the runbook](user/RUNBOOK.md#evacuate-a-server) and [the design](developer/design/EVACUATE.md).
