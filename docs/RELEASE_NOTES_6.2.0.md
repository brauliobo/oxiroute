# OxiRoute 6.2.0

OxiRoute 6.2.0 makes the same router cheaper to leave running, to start and reload, to query and to scale: an
idle instance costs about 1.4% of what it did, a farm-size configuration loads about twelve times faster, a
proxied request needs 10% to 20% fewer instructions, and route matching and the common server selection
algorithms no longer grow with the number of routes and servers. The release remains pre-alpha and retains conservative compatibility claims.

## Summary

- **Idle cost.** A minimal idle instance drops from about 696 k to 9.6 k user-space instructions per second and
  from 198 to 2 wake-ups per second. A farm-size instance drops from 3.43 M to 78 k instructions per second.
- **Startup and reload.** Appending to the audit store no longer re-reads it, so a farm that starts with a full
  store reaches quiet in 4.5 s instead of 40 s. Upstream TLS contexts parse the system CA bundle once per
  process instead of about four times per health-checked pool.
- **Configuration reads.** Canonical files load and render about twelve times faster, an unchanged file is not
  parsed again, `GET /api/v1/config` answers `304` to a matching `If-None-Match`, and unrelated files written
  next to the configuration no longer trigger reconciliations.
- **Request path.** A proxied request needs 10% to 20% fewer instructions, the access log costs 85% less, small
  static files are served from memory, and an idle keep-alive connection holds about 65 KB less.
- **Selection scaling.** Route matching costs the same with 10,000 routes as with a few, and server selection
  and the circuit breaker cost the same with 2 or 2,000 servers for round robin, first and power of two choices.
  Sticky sessions are 50x to 330x cheaper at 2,000 servers but still grow with the pool.
- **Queue and registry.** A request with free capacity skips the wait queue, a capacity event no longer wakes
  every waiter, and dynamic-registry operations and `/metrics` stop re-doing whole-pool work.
- **`listener_workers`.** A new root option runs one HTTP or TCP listener on several threads, so a listener is
  no longer capped at one core. The default stays 1.
- **Configuration warnings.** `config check` and imports report retry settings that validate but cannot take
  effect.
- **Fixes that were product bugs.** Connections opened while a replaced generation drains an HTTP/3 listener are
  no longer delayed or lost, the last response of a draining HTTP/3 connection is no longer cut off, RTMP VOD
  playback no longer reports completion before its last audio and video, and an open event stream receives its
  shutdown frame as soon as the process stops.

Read [Behaviour changes to know before upgrading](#behaviour-changes-to-know-before-upgrading) before
installing.

## Measured effect

User-space instructions (`perf stat -e instructions:u`) from the committed benchmark harnesses, release builds,
before and after on the same tree. Absolute values depend on the profile and host; wall times and rates on a
shared host are noisy and are quoted only where the developer note says they held. Each row is explained, with
its method and caveats, in the linked note.

| Area | Measure | 6.1 | 6.2 | Note |
| --- | --- | --- | --- | --- |
| Idle, minimal config | worker instr/s | 622 k | 9.6 k | [idle audit](developer/PERF_6_2_IDLE_AUDIT.md) |
| Idle, minimal config | wake-ups per second | 198 | 2 | [idle audit](developer/PERF_6_2_IDLE_AUDIT.md) |
| Idle, farm config | worker instr/s | 3.43 M | 78 k | [idle audit](developer/PERF_6_2_IDLE_AUDIT.md) |
| Audit append | instr per record, store at its cap | 931 M | 33 k | [idle audit](developer/PERF_6_2_IDLE_AUDIT.md) |
| Farm startup to quiet, store at its cap | wall | 40.0 s | 4.5 s | [idle audit](developer/PERF_6_2_IDLE_AUDIT.md) |
| Farm reload to quiet | instr | 91.6 G | 53.0 G | [idle audit](developer/PERF_6_2_IDLE_AUDIT.md) |
| Reload, old worker prepare | instr | about 27 G | about 1.4 G | [TLS client context](developer/TLS_CLIENT_CONTEXT.md) |
| `config compose --format kdl`, farm (341 KB) | instr | 1.321 G | 106.6 M | [config load](developer/CONFIG_LOAD_PERFORMANCE.md) |
| Master 30 s reconcile, farm | instr per spike | 1.25 G | about 1 M | [config load](developer/CONFIG_LOAD_PERFORMANCE.md) |
| `config get`, farm | worker instr | 1.362 G | 14.4 M | [config load](developer/CONFIG_LOAD_PERFORMANCE.md) |
| Registrar poll cycle | instr | 1.947 G | 54.2 M | [config load](developer/CONFIG_LOAD_PERFORMANCE.md) |
| Proxied request, no feature | instr per request | 88,719 | 79,820 | [request path](developer/design/REQUEST_PATH_COST.md) |
| Proxied request, access log on | instr per request | 177,761 | 93,600 | [request path](developer/design/REQUEST_PATH_COST.md) |
| Access-log cost per request | instr | 89,042 | 13,780 | [request path](developer/design/REQUEST_PATH_COST.md) |
| Static file, 1 KiB | instr per request | 66,291 | 43,201 | [request residual](developer/PERF_6_2_REQUEST_RESIDUAL.md) |
| Static file, 1 KiB | syscalls per request | 32.4 | 2.1 | [request residual](developer/PERF_6_2_REQUEST_RESIDUAL.md) |
| 8 KiB of custom request headers | instr per request | 223,265 | 194,439 | [request residual](developer/PERF_6_2_REQUEST_RESIDUAL.md) |
| Gzip, 64 KiB HTML | bytes allocated per request | 581,530 | 198,204 | [request residual](developer/PERF_6_2_REQUEST_RESIDUAL.md) |
| New connection per request | syscalls per request | 25.45 | 11.31 | [connection layer](developer/CONNECTION_LAYER_PERFORMANCE.md) |
| Idle keep-alive connection, HTTP | live heap | 94,330 B | 28,781 B | [connection layer](developer/CONNECTION_LAYER_PERFORMANCE.md) |
| 4,096 live connections | process RSS | 427 MB | 165 MB | [connection layer](developer/CONNECTION_LAYER_PERFORMANCE.md) |
| Route match, 1,000 normalized-host routes | instr | 1,398,601 | 1,301 | [routing and selection](developer/ROUTING_AND_SELECTION_PERFORMANCE.md) |
| Round robin, 2,000 servers | instr per selection | 377,044 | 1,340 | [routing and selection](developer/ROUTING_AND_SELECTION_PERFORMANCE.md) |
| Least connections, 2,000 servers | instr per selection | 393,494 | 237,276 | [routing and selection](developer/ROUTING_AND_SELECTION_PERFORMANCE.md) |
| Hash affinity, 2,000 servers | instr per request | 6,924,724 | 59,872 | [routing and selection](developer/ROUTING_AND_SELECTION_PERFORMANCE.md) |
| Cookie affinity, 2,000 servers | instr per request | 11,851,181 | 36,167 | [routing and selection](developer/ROUTING_AND_SELECTION_PERFORMANCE.md) |
| Breaker success, 2,000 servers | instr | 94,392 | 319 | [routing and selection](developer/ROUTING_AND_SELECTION_PERFORMANCE.md) |
| 4 listeners with queueing | instr per request | 309,874 | 100,454 | [pool, queue, registry](developer/POOL_QUEUE_REGISTRY_PERFORMANCE.md) |
| `on_connect` DNS lane | instr per request | 142,117 | 99,779 | [pool, queue, registry](developer/POOL_QUEUE_REGISTRY_PERFORMANCE.md) |
| One capacity event, 1,000 weighted waiters | instr | 347,644 | 9,724 | [pool, queue, registry](developer/POOL_QUEUE_REGISTRY_PERFORMANCE.md) |
| Membership update, 2,000 servers | instr | 10,108,078 | 14,383 | [pool, queue, registry](developer/POOL_QUEUE_REGISTRY_PERFORMANCE.md) |
| Registry expire all, 1,000 records | instr | 9,855,167,931 | 37,071,922 | [pool, queue, registry](developer/POOL_QUEUE_REGISTRY_PERFORMANCE.md) |
| `/metrics`, 500 pools x 20 servers | instr | 667,881,894 | 186,985,160 | [pool, queue, registry](developer/POOL_QUEUE_REGISTRY_PERFORMANCE.md) |
| Select + release, 20 servers, 4 threads | instr | 18.4 M | 9.5 M | [multi-core scaling](developer/MULTI_CORE_SCALING.md) |

Notes on reading the table:

- Rows that name a size (servers, routes, records) show the cost that used to grow with that size. Least
  connections and weighted round robin still make one pass over the selectable servers and improved by 1.7x to
  3.9x, not by orders of magnitude.
- The same row can differ between notes because each note measured against the tree at its own merge point.
  Compare a before with its own after. The combined comparison of v6.1.0 with the final 6.2 tree
  ([performance summary](developer/PERF_6_2_SUMMARY.md)) is the reference for the whole release; it shows
  larger request-path gains (about 17% fewer instructions per keep-alive HTTP/1.1 request, 54% fewer with the
  access log on), a farm reload that needs 469 M instructions until quiet instead of 119 G, and a farm that
  reaches ready in 70 ms instead of 2.3 s.
- A few rows got slightly worse by design: the scratch copy of the lazy read buffer costs about 1% on keep-alive
  requests, TCP passthrough costs about 3% more per request, and plain FIFO queue bookkeeping costs 1% to 24%
  more in exchange for cheap removal and cancellation (see the known limits). Adding and removing one pool
  member also got more expensive in small pools (see the known limits).

## Idle cost, startup and reload

Details: [idle audit](developer/PERF_6_2_IDLE_AUDIT.md), [shared TLS client context](developer/TLS_CLIENT_CONTEXT.md).

- **Audit store.** Records live in an in-memory ring. An append serializes one line and no longer re-reads or
  re-parses the store, and exceeding the record limit no longer rewrites files (they are trimmed to the limit
  when the store opens). The reload and startup cost that used to scale with the store size is gone.
- **Idle loops.** The master sleeps until an event or a deadline instead of polling every 20 ms. The worker
  reports its status only when it changed, and builds it from counters instead of a full runtime snapshot
  (`/proc` reads, descriptor scan, every pool and certificate). The shared timer thread no longer wakes 100
  times a second.
- **System CA bundle.** Upstream health checks, canaries, probes and the HTTP data path share one TLS client
  context for the system trust store, built once per process. RTMPS connects, VOD origin fetches over HTTPS,
  RTMP callbacks and ACME requests share one connector. Trust policy, verification and hostname checks are
  unchanged. A custom CA file, client certificate or key log still gets its own context.
- **Failing workers.** A worker prints why it is failing before it rejects a phase, because the master now
  kills it as soon as it sees that.

## Configuration reads

Details: [configuration load performance](developer/CONFIG_LOAD_PERFORMANCE.md).

- Canonical KDL files load and render about 12x faster; any other KDL falls back to the generic parser with the
  same diagnostics as before.
- The resolved document is reused while the SHA-256 of the file bytes is unchanged, so the 30-second master
  reconciliation costs one file read and hash. Sources with native references (`nginx_server`,
  `haproxy_server`, ...) are still resolved at every reconciliation, and rejections are never cached.
- `GET /api/v1/config` carries a strong `ETag` built from the disk and active revisions and answers a matching
  `If-None-Match` with `304`. The read no longer runs on the management I/O thread. A `304` still reads and
  hashes the file.
- The configuration, listener, pool, server, generation, readiness and monitoring responses are serialized
  straight to bytes.
- The watcher reacts only to events naming the configuration file, its directory or a recorded native
  dependency.

## Request path

Details: [request path cost](developer/design/REQUEST_PATH_COST.md),
[per-request residual](developer/PERF_6_2_REQUEST_RESIDUAL.md),
[connection layer](developer/CONNECTION_LAYER_PERFORMANCE.md).

- **Access log.** A typed record is rendered directly into a reused buffer and written in batches. Keys, order
  and values are unchanged.
- **Metrics and counters.** Latency samples take two atomic updates instead of about 17, per-server counters
  are resolved once per pool member, and the in-flight registry takes its start time from the request context.
- **Static files.** Files of up to 128 KiB are read once and kept in a per-route memory cache for one second;
  larger files stream in 64 KiB chunks read from the page cache without a thread hop when the pages are
  resident. HTTP/1, HTTP/2 and HTTP/3 share the reader. Path safety checks are unchanged.
- **Gzip and headers.** Gzip responses reuse a pooled deflate state instead of allocating about 380 KiB, and
  repeated custom header names are parsed without allocating.
- **Clock reads and allocations.** A proxied request makes 13 fewer clock reads and several fewer allocations.
- **Connections.** An idle keep-alive connection no longer holds a 64 KiB read buffer, closing a connection no
  longer probes the socket, accepting and closing no longer issue a futex wake-up nobody waits for, and TLS
  reads take what the socket holds in one call. Connection-per-request traffic runs about 1.8x faster in the
  benchmark.
- **Client `Connection: close`.** It is no longer forwarded to the origin (see the behaviour changes), so
  HTTP/1.0 clients, simple scripts and probes reuse pooled upstream connections.
- **TLS handshakes.** Nothing in OxiRoute's handshake path was large enough to change; resumption stays off by
  default. The note records the trade-offs of `session_cache` and `session_tickets`.

## Selection scaling

Details: [route matching and server selection](developer/ROUTING_AND_SELECTION_PERFORMANCE.md).

- **Routes.** An index by host class and a byte trie by path replace the scan of every route. Precedence is
  unchanged and a randomized test compares the index with the original scan.
- **Servers.** Each pool publishes a view of its selectable servers that is rebuilt only when eligibility
  changes (administrative state, health, ejection, breaker, weight). Round robin, first and power of two
  choices no longer allocate or scan the pool. A pool with almost all servers drained stays cheap.
- **Circuit breaker and passive ejection.** The "enough other servers remain" check runs only when an outcome
  would open the breaker or eject the server.
- **Sticky sessions.** Hash affinity hashes the request key once and each server contributes a stored hash.
  Cookie affinity caches the per-server cookie ids.
- **Remaining limits.** Least connections, weighted round robin and hash affinity are still linear in the
  selectable servers, and one pool-wide selection lock remains.

## Queue, registry and metrics

Details: [pool queue, DNS, registry and metrics](developer/POOL_QUEUE_REGISTRY_PERFORMANCE.md).

- A request first checks an atomic waiter count and selects directly when nobody is queued; it enters the queue
  only when the pool is saturated. Fairness (FIFO, class weights, tenant turns, aging) is unchanged.
- A lease release notifies waiters only when a request waits, and pools without a queue timeout skip the queue
  entirely. The wait queue uses ordered sets, so removal and cancellation are logarithmic.
- Dynamic-registry membership changes republish a pool in linear time, heartbeats that change nothing do
  nothing, registrations are served from memory, and an expiry sweep is one write. Every membership change is
  still written and fsynced before the call returns, and followers in other processes detect a replaced file by
  its stat identity.
- Outlier judging sorts once per route instead of once per server, and `/metrics` renders about three times
  faster on large farms.

## Multi-core scaling: `listener_workers`

Details: [multi-core scaling](developer/MULTI_CORE_SCALING.md), [CONFIG_SPEC](CONFIG_SPEC.md).

```lua
return { version = 1, listener_workers = 4, --[[ ... ]] }
```

The root option `listener_workers` (1 to 256, default 1) sets how many threads serve each `http`,
`forward_http1`, `forward_http2` and `tcp` listener. The threads share the listener socket, the pools, the
queues and the limits, so `max_connections`, queue depth, drain, retirement and evacuation behave as with one
thread. The management, statistics, RTMP, `udp`, `http3` and `forward_http3` listeners keep one thread each.
Changing the value is an ordinary reload.

- **The default stays 1.** Extra workers add cross-thread wake-ups at low load and only pay off for a listener
  that is CPU-bound on one core (about 60,000 small requests per second). Raise it only for such a listener, to
  at most the cores you can give it.
- In the benchmark one listener went from 63 k requests per second with one worker to 192 k with four, with
  instructions per request flat and about zero futex calls per request.
- Making that scale needed the contention fixes listed in the note: jemalloc as the global allocator, no
  wake-up lock on lease release, a selection lock that no longer covers building the lease, and per-thread
  slots for per-server counters and the in-flight registry.

## Configuration warnings for retry settings

`oxiroute config check` prints one `warning: <code>: <message>` line per setting that validates but cannot take
effect, after the verdict, and the exit code does not change. HAProxy imports carry the same warnings in their
report. Warnings never block finalization or change the effective policy.

- `W_RETRY_NEVER_REPLAYS`: a route that retries after the request was sent (`response_statuses`,
  `empty_response`, `response_timeout`, `junk_response`, or a `rescue`) but accepts only methods that the replay
  safety policy excludes.
- `W_PASSIVE_HEALTH_MISSING`: a pool that serves a route with `response_statuses` and has no `passive_health`,
  so a server that only returns those statuses is never ejected.

See [CONFIG_SPEC](CONFIG_SPEC.md) and [IMPORT_SPEC](IMPORT_SPEC.md).

## Fixes

These started as flaky tests and were traced to product defects.

- **HTTP/3 reload.** A reload that kept a UDP listener address left two QUIC endpoints reading one socket, the
  draining generation and the new one. Each took a share of the datagrams, so new clients waited for
  retransmissions (a second or more) or timed out, and the draining generation's own clients lost packets. The
  listener reservation now owns the single endpoint, and every generation accepts from it under its own TLS
  configuration only while its accept gate is open.
- **HTTP/3 drain.** A connection that had sent GOAWAY was closed the moment its last request task ended, which
  discarded response data the client had not acknowledged yet and surfaced as `H3_NO_ERROR` before the response.
  The connection now gives the client up to one second, bounded by the drain deadline, to receive the response
  and close first.
- **RTMP VOD.** Playback decided in two separate steps whether the queue was empty and whether the reader had
  finished, so a player could see the completion status before its last audio and video. Both are now decided
  under one lock.
- **Event stream shutdown.** Stopping the process waited for in-flight work before telling HTTP/1 connections to
  end, and an event stream ends only on that signal, so it held the whole drain and its final frame was
  delivered or lost depending on thread timing. The process now marks its generations retired as soon as it
  starts stopping: streams send their frame and finish at once, and a stop with an open stream is no longer
  delayed by the drain deadline.

## Behaviour changes to know before upgrading

- **Hash-affinity keys remap once.** The rendezvous score changed from SHA-256 of key and server name to a fast
  mix hash, so sessions pinned by a `hash` affinity key move to another server once on upgrade. Still true:
  the mapping is stateless, identical on every worker and across reloads, honors weights, and a leaving server
  remaps only its own keys. Cookie affinity is unchanged: the cookie value, and so existing cookies, stay valid.
- **Access-log lines can reach the file about 2 ms after the request ends.** Lines are batched; the writer
  waits 2 ms for concurrent requests and writes the batch with one call. Content, keys and order are unchanged.
  Tailers see the same lines slightly later.
- **Small static files are cached for 1 s.** Files of up to 128 KiB are cached in memory per route, so an edit,
  replacement, removal or permission change can take up to 1 s to show. Larger files and error documents are
  read on every request. The cache holds at most 512 files and 16 MiB per static route per generation, and a
  draining generation keeps its cache. The window is fixed and has no setting.
- **`on_connect` DNS lookups are shared for 1 s.** One lookup per endpoint per second serves all requests, so a
  changed answer is seen up to one second late, plus the OS resolver's own caching. A connect failure or
  timeout and every health probe re-resolve. `startup` endpoints keep their pinned addresses.
- **Health and RTMP audit records are batched.** A crash can lose the last half second of these records.
  Operator actions are still written and synced before they are reported, and a graceful shutdown flushes the
  batch. Between restarts the audit files can hold more than `OXIROUTE_AUDIT_MAX_RECORDS` records; readers
  always see at most the limit. The counters `oxiroute_audit_operations_total` still count every event.
- **Consecutive duplicate health-event audit records collapse.** Pending records of the same kind merge into
  one, so the first observations of a generation with many servers are a handful of records instead of one per
  server.
- **jemalloc is the global allocator on Linux.** Workers no longer queue on glibc's shared arenas, and every
  request costs about 5% fewer instructions.
- **A client's `Connection: close` no longer closes the pooled upstream connection.** The `close` token is
  removed from the upstream request (`Upgrade` and other tokens are kept), and the client connection is still
  closed. Under per-server `max_connections` caps such clients now follow the reusable-connection path, so a
  burst queued behind one long request can concentrate on the server whose idle connection frees first.
- **`GET /api/v1/config` has an `ETag` and answers `304`.** Pollers may send `If-None-Match`. Clients that
  ignore the header are unaffected.
- **JSON key order follows declaration order in some management responses.** The configuration, listener,
  pool, server, generation, readiness and monitoring responses are serialized directly from their types
  instead of through an intermediate value tree. Consumers that parse JSON objects are unaffected; scripts that
  compare response text byte for byte may see a different key order.
- **Other timing.** Timers armed through the vendored timer fire up to 1 ms later than before, never earlier.
  The master sees the counters of an idle worker up to one second late (250 ms while they change).
- **`listener_workers` defaults to 1.** Nothing changes unless you set it. A 6.1 configuration loads unchanged.
- **Retry warnings appear on existing configurations.** `config check` may now print warnings for
  configurations that passed silently. Exit codes and effective policies are unchanged.

## Upgrade notes

- **Restart the services after the package upgrade.** The package does not restart them, so running
  processes keep the previous binary. Check that no long job is running first, because a restart ends
  in-flight requests.
- Configuration formats are unchanged except for the new optional root field `listener_workers`.
- Memory: a static route can hold up to 16 MiB of cached files per generation, and each worker thread can keep
  up to four pooled gzip states (about 1.5 MiB) and a bounded header-name cache. Idle connections hold much
  less than before.
- The audit store adds one parked writer thread.

### Pingora rebase

The vendored Pingora crates carry additional deltas, which the next Pingora upgrade must reapply and review.
They are recorded in `vendor/pingora-core/README.oxiroute.md` and `vendor/pingora-timeout/README.oxiroute.md`.
The vendored version is the same as in 6.1 (0.8.1).

- `pingora-core`: pooled gzip deflate state (a per-thread pool of at most four, same stream header and trailer
  as before), a per-thread HTTP/1 header-name cache (at most 512 names of up to 64 bytes) and a lazily
  allocated accepted-stream read buffer (`protocols/l4/lazy_read.rs`). Smaller changes are the accepted-stream
  drop without a socket probe, the accept gate notifying only while a thread waits, and the shared system-trust
  TLS client context.
- `pingora-timeout` is now vendored: its clock thread parks until the earliest armed timer (at most one second)
  instead of waking every 10 ms, and `DELAYS_SEC` is raised from 2 to 3.
- `pingora-load-balancing`: health checks hold an `Arc` connector and default to one process-wide connector.

## Known limits

- The numbers above were measured on a shared host: instructions are reliable, wall times and rates are
  indicative, and tail-latency claims need a quiet machine. Each note says what it could not conclude.
- A full TLS 1.3 handshake still costs about 1.83 M instructions, the same as 23 keep-alive requests, almost all
  inside OpenSSL.
- Least connections, weighted round robin, hash affinity and cookie affinity remain linear in the selectable
  servers.
- **Late binding with reusable upstream connections.** With `connection_reuse safe`, a burst behind
  capacity-limited servers is not always spread over every server. With two servers of capacity 1, one 800 ms
  request and then 24 requests of 50 ms, the ideal makespan is 1.0 s, and a bad run serves `[1, 24]` in 1.34 s
  while the first server idles for about 0.5 s. It is not a 6.2 regression: it was measured the same on 6.1.0.
  It was hidden for clients that sent `Connection: close`, which 6.1 forwarded upstream so that every request
  opened a new connection; 6.2 no longer forwards it, so such clients now take the reusable-connection path.
  A request that is bound to a server waits for that server and is not selected again. `connection_reuse never`
  gives the ideal makespan. See [the pool note](developer/POOL_QUEUE_REGISTRY_PERFORMANCE.md#known-limitation-late-binding-with-reusable-upstream-connections).
- **Adding or removing a pool member costs more in small pools.** Each membership change now republishes the
  selectable view. One server added and removed costs about 7 k instructions in a 20-server pool on 6.1 and
  about 38 k on 6.2 (+443%), +75% at 200 servers, and 81% less at 2,000 servers (20.2 M to 3.8 M). Updates that
  change nothing are free. Only registrations that join or leave a pool pay the added cost, once per change.
- **Plain FIFO queue bookkeeping costs more.** Waiters without class weights pay for the ordered sets that make
  removal and cancellation logarithmic: draining plain queues is 1% to 7% slower, a wake-up storm of plain
  waiters 19% slower, and a capacity event on a pool nobody waits on 24% slower (about 80 instructions).
  Weighted queues are much cheaper (up to 97%).

See the developer notes linked in each section, [CONFIG_SPEC](CONFIG_SPEC.md) for `listener_workers`, and
[the runbook](user/RUNBOOK.md) for `config check`.

## Audit status

The advisories against the vendored Pingora crates remain open in 6.2.0. They are cleared only by the rebase
onto Pingora 0.9.1, which is not part of this release (the vendored version is still 0.8.1). Treat the
`cargo audit` and `cargo deny` advisory findings for the Pingora dependency paths as known until that rebase,
and see [the security audit](developer/SECURITY_AUDIT.md) for the current record.
