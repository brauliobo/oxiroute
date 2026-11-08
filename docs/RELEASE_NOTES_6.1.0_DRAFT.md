# OxiRoute 6.1.0 Release Notes (Draft)

OxiRoute 6.1.0 lets an operator free a machine immediately: `evacuate` is `drain` plus a one-shot move of
the requests running on it. The release remains pre-alpha and retains conservative compatibility claims.

## Highlights

- `oxiroute server evacuate [--force] --pool <pool> [--pool <pool> ...] <server>` and
  `oxiroute pool evacuate [--force] <pool> [<pool> ...]` (`POST /api/v1/servers/evacuate` and
  `POST /api/v1/pools/evacuate`) drain the server or pool and cancel its in-flight HTTP requests, each of
  which is dispatched again to another server of the same pool. The caller gets the response of the second
  server with the same `x-request-id` and sees no error. This replaces killing the worker process to
  reclaim a machine that runs minutes-long renders or speech jobs.
- A request moves only while no response byte was sent and the backend has not answered with its response
  header, and only when the route's `retry` policy allows replaying it (`method_safety`,
  `idempotency_header`, `body_safety`, `request_buffering`). Retries do not need to be enabled and a moved
  request spends neither retry budget nor `max_retries`. A request moves at most 3 times and its total
  deadline keeps running.
- The command prints a report: `cancelledAndRedispatched`, `forced` and `leftRunning` by reason
  (`response_started`, `not_replayable`, `body_not_replayable`, `upgraded`, `limit_reached`,
  `no_destination`). Requests left running finish as in drain. `--force` also cancels them: `503` with
  `x-oxiroute-reason: evacuated`, or a closed connection when the response had started.
- The state is `drain`: it follows the server by pool and server name across generations, `server ready`
  un-drains, and the cancel itself is never replayed by a later generation or a restart.
- Observability: `oxiroute_evacuated_requests_total{pool,server,result}`, the attempt outcome `evacuated` on
  `oxiroute_server_attempts_total`, the `server_evacuation` event and the audit entry.
- Limits: WebSocket and other upgrades, forward proxy and `CONNECT`, HTTP/3, TCP, UDP and RTMP traffic is
  drain only (`--force` closes upgraded connections).

Upgrade note: the cancel needs a small additive hook in the vendored Pingora proxy crate; nothing changes
for deployments that never run `evacuate`. Configuration formats are unchanged. See
[the runbook](user/RUNBOOK.md#evacuate-a-server) and [the design](developer/design/EVACUATE.md).
