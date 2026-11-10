# Vendored pingora-runtime patch

This directory contains the published `pingora-runtime 0.9.1` source from crates.io
(registry checksum `09d2c5be0a8fcffa36c23d21b3d6ec5bad113395086f5b4e1a0c50b300c674c0`)
under its upstream Apache-2.0 license. The benchmark target and `benches/` are dropped; the normalized manifest
is kept with `thread_local` removed and a path dependency on `crates/oxiroute-aged-executor` added.

OxiRoute's delta is limited to `src/lib.rs`:

- `spawn_connection` places a new connection task on the non-stealing worker with the fewest open connections, the
  calling worker winning ties, and gives the slot back when the task ends. Upstream's `current_handle().spawn` picks a
  worker at random, which leaves a few connections unevenly spread for the whole life of long keep-alive clients. On a
  work-stealing runtime, or outside a pool thread, it is a plain `spawn` on the current runtime. The worker table keeps
  one load counter per worker (`Pool`).
- `RuntimeOpts::connection_age` (`Some(age)` on a non-stealing runtime) gives every worker an
  `oxiroute_aged_executor::AgedExecutor` whose runner is spawned as a task on the worker's `current_thread` runtime,
  built with `event_interval(1)`. `spawn_connection` submits the connection to the chosen worker's executor instead of
  Tokio's first-in first-out queue, so the worker serves the newest ready connection first and the oldest once it
  waited `age`. Tokio itself is not patched; tasks that are not connections (HTTP/2 streams, watchers) keep Tokio's
  queue.
- `spawn_stream` (used for HTTP/2 stream tasks in place of `current_handle().spawn`) runs a stream on the calling
  worker's aged executor when the runtime has one: upstream spawns it on a random worker, which moved the data of
  every HTTP/2 connection between cores (512 requests in flight on two workers: p99 five times higher than work
  stealing). Without aged queues it behaves as upstream does.
- The worker table of the calling thread is a std `thread_local!`, not the `thread_local` crate: the crate hands the
  entry of an exited thread to any later thread with the same identifier, so after a generation replacement a worker
  of the new runtime inherited the shut down runtime's table and its listener tasks were spawned on a dead runtime
  (upstream never hits it: only non-stealing runtimes read the table, and they are not replaced at runtime). A test
  builds and shuts down runtimes in a loop.
- Tests (`connection_tests`) cover the even spread, the slot hand-back, the tie rule, the work-stealing fallback, the
  aged runtime and the thread-identifier reuse (`scripts/test-vendored.sh runtime`).

OxiRoute runs a service on non-stealing runtimes only when the configuration asks for
`connection_scheduling = "aged_per_worker"` (`ServerConf::work_stealing = false` with `connection_age_ms` set); the
default is Pingora's work-stealing runtime. See `docs/developer/PERF_6_3_RUNTIME_MODEL.md` and
`docs/developer/PERF_6_4_CONNECTION_SCHEDULING.md`.
