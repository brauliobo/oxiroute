# OxiRoute 6.0.0

OxiRoute 6.0.0 makes a router in front of a farm of uneven machines keep working when machines fail,
slow down, join or leave. The release remains pre-alpha and retains conservative compatibility claims.

## Summary

What an operator sees, by outcome:

- **Fault tolerance.** A pool no longer goes dark because its health checks or passive ejection
  emptied it (`resilience`); a hung backend answers `504` with a reason instead of a dropped
  connection; a `POST` is replayed only when it carries an idempotency key; retries have a budget;
  a backend `404` (a node that lacks a model) can be rescued on another server; a rejected
  configuration file is no longer silent, and drain, maintenance and overrides survive every
  generation change.
- **Balancing.** Waiting requests take whichever server frees first, load follows capacity and live
  weights, a circuit breaker, outlier detection and probe-based re-admission replace the "client pays
  for the probe" recovery, and requests can be routed by capability, pinned to a server or ordered by
  priority class.
- **Registry.** Workers join and leave a pool on their own with a lease; no configuration write, no
  generation, no drain reset, no root token.
- **Lifecycle.** A replaced generation is bounded by a drain deadline and a cap, an idle keep-alive
  connection no longer blocks the next apply, and draining generations are listed.
- **Observability.** Every request carries an id, the access log says where a request went and why it
  failed, stuck requests are listed and counted, and queue, generation and per-server series are
  truthful.
- **Operations.** `oxiroute check` gives monitoring a line and an exit code, `/status.json` judges
  readiness per pool, pools can carry a synthetic canary, and the operator runbook is complete.
- **Structure.** The oversized source files are split into folders by context and their tests moved
  to separate files; behavior is unchanged.
- **Dependencies.** Workspace crates, the UI and the recordings toolchain are updated to their latest
  releases.

Read [Upgrade notes](#upgrade-notes) before installing: the supervising master must be restarted, and
a configuration that uses new fields cannot be rolled back to 0.5.4 until they are removed.

## Fault tolerance

### Pool resilience

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
  management API count the selections made this way. Fail-open applies to HTTP and L4 pools alike;
  alerting on `oxiroute_pool_available_servers == 0` keeps working because the strict count does not
  include failed-open servers.
- The configuration reference now states what the runtime has done since 0.5.x: a pool without a
  `passive_health` block does no passive failure accounting or ejection, and an HTTP status counts
  as a passive failure only when the route's `retry.response_statuses` lists it.

### Timeouts and retries

- A hung backend or an expired request deadline now answers `504` with an `x-oxiroute-reason`
  header (`connect_timeout`, `upstream_header_timeout`, `upstream_write_timeout`, `total_deadline`)
  instead of dropping the connection. Only the listener's own client timeouts still close silently.
  A request that waits in the pool queue past the route deadline also answers `504`.
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

### Configuration safety

- A configuration file that fails to load, validate or prepare is no longer dropped silently while
  the old generation keeps serving. It is logged once as an error with the diagnostic code, the
  failing setting and the line of a syntax error; a `configuration_reload` event with outcome
  `rejected` is emitted; `oxiroute_config_rejections_total` rises; and `generation status` shows
  `lastRejection` and `degraded` until the next acceptable file. `/ready` stays OK. Under the
  supervising master the same status is shown, including the master's own refusals (unsupported
  configuration, unbindable listener, failed replacement worker).
- `422` responses, `config check` and startup failures name the failing setting as a JSON pointer,
  the rule that failed and the syntax-error line; a root without `version 1` says so.
- Administrative state (drain, maintenance, `max_connections` and health overrides, weights) follows
  a server by pool and server name through file writes, reload, apply, rollback and supervised worker
  replacement, and is in place before the new generation accepts. `generation status` shows the live
  generation number.

## Balancing

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
- New `passive_health.outlier_detection` ejects a server whose time to first byte on a route is far
  above its peers (reason `slow`, a new `lastFailure`/`passiveEjectionReason` value in the API).
- New proxy `policy.select` routes a request to a pool by the numeric value of a request header
  (capability routing), with optional spill to a larger pool when the chosen one has no selectable
  server; counted in `oxiroute_route_pool_selections_total{route,pool}`.
- New proxy `policy.affinity` (cookie, rendezvous hash or header) pins sessions to a server while it
  stays selectable.
- New pool `queue` policy orders waiting requests by class (weighted or strict, with aging) and
  shares each class fairly between tenants; routes can fix a class with `policy.queue_class`.
  Waiting requests per class appear in `oxiroute_pool_queue_class_depth{pool,class}`.
- New algorithm `power_of_two_choices`. `least_connections` now compares load per unit of capacity
  (`max_connections`, else weight) instead of raw counts, and `weighted_round_robin` is smooth
  (weights 3 and 1 give `a a b a`, not `a a a b`) and follows live weights. The UI offers the new
  algorithm.
- New `oxiroute server weight set|reset --pool P SERVER [WEIGHT]` and `PUT /api/v1/servers/weight`
  change a server weight (1 to 100) at runtime; like the other overrides it follows the server by
  pool and server name through reloads.
- New per-route `policy.queue_headers` (default off) adds `x-oxiroute-queue-position` and
  `x-oxiroute-queue-wait-ms` to the responses of requests that waited for capacity.
- A connection that was queued across a configuration apply and has not sent a request yet is still
  served; only the wait between requests on a reused keep-alive connection ends.

## Registry

- Workers can join and leave a pool on their own. A pool opts in with `registration` (limits,
  TTL bounds, allowed address ranges) and the new process-level `registry` block names the store,
  the scoped tokens file and the endpoints. A registration is a lease: `PUT
  /api/v1/pools/{pool}/servers/{name}`, a heartbeat before it runs out, `DELETE` to leave. A
  worker that goes silent leaves rotation within its TTL and the requests it holds finish. No
  configuration write, no generation, no drain reset, no root token.
- Access without the root token: group-permission unix sockets (`read` for status without `sudo`,
  `register` limited to named pools, access is the file mode) and a TCP endpoint for LAN workers
  with per-pool scoped tokens, a peer address allow list, a 4 KiB body limit and a rate limit.
  Registered addresses are checked against the pool's `allowed_cidrs` and never point at an
  OxiRoute listener.
- Registrations live in a persisted store outside the configuration file. They survive restarts,
  `config apply`, rollback and supervised worker replacement; static servers win over a registration
  of the same name or address; an operator's drain, maintenance, weight and capacity choices for a
  registered server are kept like those of a configured one (the drain is also stored, so it
  survives restarts).
- The same model serves any pool: a voice or LLM pool registers exactly like a render pool, with
  its own tokens and limits.
- Health checks, ejection, the circuit breaker, queues and metrics treat registered servers like
  configured ones. Server inventory, pool view and topology report `origin`; new
  `oxiroute_registry_*` series and `server_update` events report registrations, expiries and
  refusals.
- New CLI: `oxiroute server register|deregister`, `server list --pool P --origin static|dynamic`,
  `--socket PATH` or `OXIROUTE_ENDPOINT=unix:/path`, and `oxiroute registry token create`.
  `server register --heartbeat` is a complete agent for a systemd unit.
- Evaluated and not built: merging an overlay or an include directory into each candidate
  configuration, which costs one generation (0.5 to 2.5 s, a new worker process, reset drain state
  and counters) per membership change. The registry updates live pools in place instead, and the
  configuration file, its revisions and `config apply` never see registered servers.
- Contracts: `contracts/registry.openapi.json` (new) and `contracts/control-plane.openapi.json`
  (`origin` on the server inventory).

## Lifecycle

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

## Observability

- Every HTTP request carries `x-request-id`: a well formed incoming value is kept, otherwise one is
  generated. It is sent to the backend on every attempt and returned on every response, including
  the router's own `404`, `502`, `503` and `504` and compressed responses. A backend's own id is
  kept as `x-upstream-request-id`.
- The access log now records `requestId`, `pool`, `server`, `servers`, `attempts`, `rescued`,
  `queueWaitMs`, `connectMs`, `headerMs`, `wasStuck` and a bounded `reason` for failed requests.
  `Idempotency-Key` values are never logged, only a short hash (`idemKey`).
- New `oxiroute requests list|show` and `GET /api/v1/requests` list the requests inside the router;
  a request quiet for a minute is logged and counted once as stuck, and
  `oxiroute_http_inflight_overdue` (should be 0) flags requests that outlived their deadline.
- `oxiroute_pool_queue_depth` now counts requests waiting for a free connection on the default
  (reusing) path, which it read as 0 before. New: `oxiroute_pool_queue_oldest_wait_seconds`,
  `oxiroute_pool_queue_wait_milliseconds`, per-server response, attempt, latency, byte and
  connection limit series, and `oxiroute_generation_info` (the active generation next to its age).
- Active health check transitions are events (`upstream_endpoint_health`), with the previous and next
  state and the failure reason; they replace the bare log line.

## Operations and monitoring

- New `oxiroute check`: one line and a monitoring-plugin exit code for cron, systemd timers, Nagios,
  Icinga and NRPE (`0` OK, `1` WARNING, `2` CRITICAL, `3` UNKNOWN), with `--json`. It judges readiness,
  the available servers of each pool (`--warn-available-below`, `--crit-available-below`), queue wait
  (`--max-queue-wait`), the oldest request (`--max-inflight-age`), overdue and stuck requests, a refused
  configuration file, retired generations that do not drain (`--max-retired-age`), registry leases and
  the pool canaries; `--canary --url URL` also requests a URL through the listener. These codes replace
  the exit categories 2 to 9 of every other command for `check` only, and a usage error of `check` exits
  `3`, not `2`.
- `check` needs no token. It reads the public `/status.json`, over the management endpoint or over
  `--socket` on a `read` registry socket, so a monitoring user in the socket's group runs it without
  `sudo`. The `read` socket serves `/status.json` beside `/ready`.
- New public `GET /status.json` with an overall and per-pool `ok`/`warning`/`critical` state:
  `ready`, `generation` (`id`, `retired`, `retiredOldestAgeSeconds`, `lastRejection`), per pool
  `canary`, and `registry` (`leases`, `lateLeases`, `shadowed`, `orphaned`, `degraded`). One
  implementation judges it for the page and for `check`.
- New pool `canary` (`path`, `host`, `expected_status`, `interval_ms`, `timeout_ms`, `max_latency_ms`):
  a `GET` every interval through the pool's own server selection, as a client reaches it. It never
  waits for capacity and never takes a slot from a client (a saturated pool is skipped and counted as
  `busy`), and it does not eject servers. Results are `oxiroute_canary_success`,
  `oxiroute_canary_duration_milliseconds`, `oxiroute_canary_last_success_timestamp_seconds`,
  `oxiroute_canary_consecutive_failures` and `oxiroute_canary_runs_total{result}` per pool, and appear
  in `check`: one failed run warns, two in a row are critical. Plain HTTP only, like `health_check`.
- The operator runbook (`docs/user/RUNBOOK.md`) is complete for 6.0: adding and removing machines with
  and without the registry, drain, rollback, an outage playbook, the NFS-aware worker health gate,
  local-first and two-front setups, and `check` with cron and systemd examples.

## Structure

No behavior change is intended. The large source files were split into modules by context and their
inline tests moved to separate files, so reviews and future changes touch smaller units:

- `oxiroute-server` is grouped into `config/`, `generation/`, `http/`, `http3/`, `l4/`, `listener/`,
  `observability/`, `registry/`, `rtmp/`, `service_plan/` and `supervised/`.
- The supervisor master, its status and wire model, the descriptor handling and the RTMP crate are
  split the same way.
- Routing addresses pool servers by handle so membership can change under health checks.

## Dependencies

- Workspace crates are updated to their latest releases (among them tokio 1.54, kdl 6.7,
  mlua 0.12, hocon-parser 1.14, bcrypt 0.19, sha2 0.11, rcgen 0.14, base64 0.23, signal-hook 0.4 and
  `h3` at upstream master), the vendored Pingora and `rml_rtmp` manifests are adapted, and the fuzz
  crate follows.
- The UI moves to Vite 8, Vitest 5 and jsdom 31 (TypeScript stays at 6.x for `vue-tsc`); the
  recordings toolchain moves to Remotion 4.0.541 and React 19.3.
- Pingora stays at 0.8.1. See Known limits for the advisories this leaves open.

## Upgrade notes

- **Restart the service to upgrade.** The supervising master and its workers exchange the activation
  hand-over (revision and administrative state) and notices (configuration status, retired
  generations) as bounded payloads on the control channel. A 0.5.4 worker rejects those frames and a
  6.0 worker under a 0.5.4 master receives no hand-over, so a package upgrade followed by a reload is
  not supported: restart the master. A restart ends in-flight requests and clears runtime-only
  state that is not persisted (drains set before the restart are not carried over; drain with the
  registry or reapply them afterwards). Check that no long job is running first.
- **Rollback needs the new fields removed.** The previous release rejects unknown fields and
  algorithm names. Before rolling back, remove `resilience`, `policy.timeouts`, `policy.rescue`,
  `policy.select`, `policy.affinity`, `policy.queue_class`, `policy.queue_headers`, the new `retry`
  fields (`idempotency_header`, `budget`, `jitter_percent`), `passive_health.readmission`,
  `passive_health.circuit_breaker`, `passive_health.outlier_detection`, a pool `queue`, `canary`,
  `registration`, the `registry` and `retirement` blocks, and the `power_of_two_choices` algorithm.
  Registrations stay in `state_dir` and return when the pool opts in again. `oxiroute check` does not
  exist before 6.0. Then install the previous package and restart the service.
- The canonical form of every configuration now includes an empty `registry` entry, so effective
  revisions change once on upgrade.
- Requests that used to end with `502` because an upstream read, write or connect timeout fired now
  end with `504`.
- Weighted round-robin selection order changed (smooth instead of clumped); the share per server is
  the same. `least_connections` now compares load per unit of capacity.
- `oxiroute_pool_queue_depth` can now read above 0 on pools where it used to read 0.
- `OXIROUTE_RELOAD_DRAIN_SECONDS` is deprecated in favor of `retirement.drain_deadline_ms`.
- Rescue and the retry budget are rejected for HTTP/3 upstream pools.
- `delay_ms` still applies only before same-server retries; a next-server retry is immediate.

## Known limits

- Under the supervising master one replaced worker drains at a time, so a second apply waits while a
  long request keeps the first alive. The retired worker is listed with its connection total instead
  of work by protocol. A retired worker closes itself at the deadline; the master kills it 10 seconds
  later if it has not left. Stopping the service still ends long jobs after `TimeoutStopSec=15s`.
- There is no operator command to close a retired generation early.
- Not built: switching the request id off, `Server-Timing`, `Retry-After` from the queue, and
  coalescing of health events. The canary has no per-server mode (use `health_check`) and emits no
  events.
- Pingora is at 0.8.1, which `cargo audit` flags with RUSTSEC-2026-0388 (HTTP/2 `:authority` and
  `Host` mismatch that enables cache poisoning and host-based access control bypass) and
  RUSTSEC-2026-0390 (client-nominated `Connection` headers forwarded to HTTP/1 upstreams). Both are
  fixed in Pingora 0.9, which this release does not adopt because the vendored Pingora carries an
  OxiRoute patch that has to be rebased first. Review the advisories if an upstream relies on
  host-based access control or OxiRoute sits in a proxy chain. `unsafe-libyaml` (unmaintained,
  reached through Pingora's configuration parser) and `lru` 0.16.4 (RUSTSEC-2026-0253, unsound
  `LruCache::pop`) remain as well.
