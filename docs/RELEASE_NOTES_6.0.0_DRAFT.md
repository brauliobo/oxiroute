# OxiRoute 6.0.0 (draft)

OxiRoute 6.0.0 makes upstream pools resilient and upstream timeouts and retries predictable. The
release remains pre-alpha and retains conservative compatibility claims.

## Pool resilience

A pool keeps serving when health checks or passive ejection would otherwise take it to zero servers.

- A new per-pool `resilience` object groups availability options; every option defaults to the
  previous behavior, so existing configurations are unchanged.
- `resilience.fail_open = true`: when no server of the pool is selectable because of health checks
  or passive ejection, the pool selects the least bad server instead of answering an empty `503`.
  Administrative drain, maintenance and a forced-down health override are still honoured, and
  `max_connections` and the queue still apply. A single-backend pool whose application also fails
  its own health path now returns the application's error response.
- `resilience.min_members` (default 1 with `fail_open`, otherwise 0) keeps that many servers
  selectable: passive ejection no longer takes the last selectable server out of rotation, so a
  short burst of application errors cannot turn into a whole backoff of `503` answers.
- `resilience.ejection_jitter_percent` (0 through 50) randomizes each passive ejection duration so
  servers ejected together do not return together; the maximum backoff still bounds it.
- `resilience.queue_while_unavailable_ms` makes requests wait up to that long for a recovering
  server before the pool answers `503`, so a rolling restart or a node reboot no longer shows up as
  client errors.
- The `503` OxiRoute answers when a pool has no server that can take the request now carries a
  correct `Date` header (it previously carried Pingora's placeholder from 1994 when automatic
  response headers were off). `resilience.unavailable_retry_after_seconds` adds `Retry-After` and
  `resilience.unavailable_body` sets a short `text/plain` body.
- `oxiroute_pool_fail_open_selections_total` and the pool `failOpenSelections` field of the
  management API count the selections made this way.
- The configuration reference now states what the runtime has done since 0.5.x: a pool without a
  `passive_health` block does no passive failure accounting or ejection, and an HTTP status counts
  as a passive failure only when the route's `retry.response_statuses` lists it.

## Timeouts and retries

- A hung backend or an expired request deadline now answers `504` with an `x-oxiroute-reason`
  header (`connect_timeout`, `upstream_header_timeout`, `upstream_write_timeout`, `total_deadline`)
  instead of dropping the connection. Only the listener's own client timeouts still close silently.
- New per-route `policy.timeouts` with optional `connect_ms`, `header_ms` (time to the first response
  byte), `idle_ms` (between response body reads) and `total_ms`. They take precedence over the pool
  `server_timeout_ms` and the route `read_timeout_ms`, and a per-attempt `header_ms` leaves retry
  budget inside the total deadline.
- `retry.response_statuses` accepts any status between 100 and 599.
- New per-route `policy.rescue`: when a backend answers a listed status (for example a `404` from a
  node that lacks a model), the request is replayed on another server of the pool or on a named
  rescue pool, each server at most once. If every candidate answers a rescue status, the client
  receives the last backend response unchanged. Rescues are counted in
  `oxiroute_http_rescues_total{route,status}` and are not passive health failures.
- New `retry.idempotency_header`: with `method_safety = "all"`, a `POST` (or any method but `GET` and
  `HEAD`) is replayed after the request was sent only when it carries that header, such as
  `Idempotency-Key`. Without the header a response timeout or a retried status no longer runs the
  work twice.
- New `retry.budget` (`per_second`, `ratio_percent`) caps the retries of a route so that they cannot
  amplify an outage, and `retry.jitter_percent` spreads the same-server retry delay.

## Generation retirement

- An idle HTTP/1 keep-alive connection no longer holds a replaced generation, so it can no longer
  delay the next apply until an operator resets the socket. It is closed when the next revision is
  applied; a request in progress finishes with `Connection: close`.
- New `retirement` block: `drain_deadline_ms` (1 s to 24 h) bounds how long a replaced generation
  serves work already in progress before it is closed, and `max_retired_generations` (1 to 16,
  default 4) bounds how many may drain at once. An HTTP, forward proxy or L4 service may set its own
  `drain_deadline_ms`, which only raises the deadline. The default is the larger of 3 hours and the
  longest HTTP `upstream_io_timeout_ms`, so a request is never cut earlier than its own deadline.
  `OXIROUTE_RELOAD_DRAIN_SECONDS` still works and is deprecated.
- The HTTP/3 drain of a replaced generation follows that deadline instead of a fixed 5 seconds.
- `generation status` and `GET /api/v1/generations` list draining generations (`retiredGenerations`,
  `maxRetiredGenerations`, `pendingApply`); new metrics `oxiroute_generation_retired`,
  `oxiroute_generation_retired_age_seconds`, `oxiroute_generation_in_flight` and
  `oxiroute_generation_forced_close_total`, and a `generation_retire` event.
- Limits: under the supervising master one replaced worker drains at a time, so a second apply
  waits while a long request keeps the first alive, and the retired list is filled by the direct
  runtime only. A retired worker closes itself at the deadline; the master kills it 10 seconds
  later if it has not left. Stopping the service still ends long jobs after `TimeoutStopSec=15s`.

## Observability

- Every HTTP request carries `x-request-id`: a well formed incoming value is kept, otherwise one is
  generated. It is sent to the backend on every attempt and returned on every response, including
  the router's own `404`, `502`, `503` and `504`. A backend's own id is kept as
  `x-upstream-request-id`.
- The access log now records `requestId`, `pool`, `server`, `servers`, `attempts`, `rescued`,
  `queueWaitMs`, `connectMs`, `headerMs`, `wasStuck` and a bounded `reason` for failed requests.
  `Idempotency-Key` values are never logged, only a short hash (`idemKey`).
- New `oxiroute requests list|show` and `GET /api/v1/requests` list the requests inside the router;
  a request quiet for a minute is logged and counted once as stuck, and
  `oxiroute_http_inflight_overdue` (should be 0) flags requests that outlived their deadline.
- `oxiroute_pool_queue_depth` now counts requests waiting for a free connection on the default
  (reusing) path, which it read as 0 before. New: `oxiroute_pool_queue_oldest_wait_seconds`,
  `oxiroute_pool_queue_wait_milliseconds`, per-server response, attempt, latency, byte and
  connection limit series, and `oxiroute_generation_info`.
- New public `GET /status.json` with an overall and per-pool `ok`/`warning`/`critical` state.
- New per-route `policy.queue_headers` (default off) adds `x-oxiroute-queue-position` and
  `x-oxiroute-queue-wait-ms` to the responses of requests that waited for capacity.
- Active health check transitions are events (`upstream_endpoint_health`), with the previous and next
  state and the failure reason; they replace the bare log line.
- Not yet in this draft: switching the request id off and `Server-Timing`.

## Load balancing and admission

- A request that waits for a server no longer stays bound to the server the algorithm picked on
  arrival. On every path (reusable HTTP included) the pool queue hands the oldest waiter whichever
  server frees a slot, or has an idle pooled connection, first. With `algorithm = "first"` or
  `round_robin`, mixed short and long jobs no longer pile up behind one busy worker while another
  sits idle. On a scratch instance with one 3 s job and 24 short jobs on two single-slot workers,
  the makespan went from 5.3 s (`first`) and 4.2 s (`round_robin`) to 3.0 s for the ideal 2.7 s.
- Waiting requests are now counted in the pool queue depth, total, timeout and cancellation metrics
  on the default reusable path too; `oxiroute_pool_queue_depth` no longer reads 0 while requests
  wait.
- `passive_health` counts consecutive failures: a successful response now clears the run, so
  unrelated failures spread over time no longer eject a healthy server.
- New `passive_health.readmission` returns an ejected server only after an active probe (HTTP path
  or TCP connect) passes `successes` times in a row; a failed probe re-ejects it for the next
  backoff step. Recovery no longer costs `error_limit` client-visible failures.
- New `passive_health.circuit_breaker` takes a server out of rotation when its failure ratio over a
  sliding window crosses a threshold, then admits limited trial requests before closing again.
- New algorithm `power_of_two_choices`. `least_connections` now compares load per unit of capacity
  (`max_connections`, else weight) instead of raw counts, and `weighted_round_robin` is smooth
  (weights 3 and 1 give `a a b a`, not `a a a b`) and follows live weights.
- New `oxiroute server weight set|reset --pool P SERVER [WEIGHT]` and `PUT /api/v1/servers/weight`
  change a server weight (1 to 100) at runtime; like the other overrides it follows the server by
  pool and server name through reloads.

## Operational Notes

Fail-open applies to HTTP and L4 pools alike. Alerting on `oxiroute_pool_available_servers == 0`
keeps working: the strict count does not include failed-open servers.

## Upgrade notes

- Weighted round-robin selection order changed (smooth instead of clumped); the share per server is
  the same.
- A configuration that uses `passive_health.readmission`, `passive_health.circuit_breaker` or `power_of_two_choices` must drop them
  before rolling back, because the previous release rejects unknown fields and algorithm names.

- Requests that used to end with `502` because an upstream read, write or connect timeout fired now
  end with `504`.
- Rescue and the retry budget are rejected for HTTP/3 upstream pools.
- `delay_ms` still applies only before same-server retries; a next-server retry is immediate.

Rollback: install the previous package and restart the service. A configuration that uses
`resilience`, `policy.timeouts`, `policy.rescue` or the new `retry` fields must drop them first,
because the previous release rejects unknown fields.
