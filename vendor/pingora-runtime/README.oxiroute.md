# Vendored pingora-runtime patch

This directory contains the published `pingora-runtime 0.8.1` source from crates.io
(registry checksum `41815a13691a3e7d9ad0e34767d4140284132e31b95a4481f5e73ab6f407f834`)
under its upstream Apache-2.0 license. The benchmark target is dropped.

OxiRoute's delta is limited to `src/lib.rs` and a path dependency on `crates/oxiroute-aged-executor` in `Cargo.toml`:

- `spawn_connection` places a new connection task on the non-stealing worker with the fewest open connections, the
  calling worker winning ties, and gives the slot back when the task ends. Upstream's `current_handle().spawn` picks a
  worker at random, which leaves a few connections unevenly spread for the whole life of long keep-alive clients. On a
  work-stealing runtime, or outside a pool thread, it is a plain `spawn` on the current runtime.
- The worker table of the calling thread is a std `thread_local!`, not the `thread_local` crate (dependency dropped):
  the crate hands the entry of an exited thread to any later thread with the same identifier, so after a generation
  replacement a worker of the new runtime inherited the shut down runtime's table and its listener tasks were spawned
  on a dead runtime (upstream never hit it: only non-stealing runtimes read the table). A test builds and shuts down
  runtimes in a loop.
- The worker table keeps one load counter per worker. Tests cover the even spread, the slot hand-back, the tie rule and
  the work-stealing fallback (`cargo test --manifest-path vendor/pingora-runtime/Cargo.toml --offline`).

- `Runtime::new_no_steal_aged` (`NoStealRuntime::new(.., Some(max_age))`) gives every worker an
  `oxiroute_aged_executor::AgedExecutor` (`crates/oxiroute-aged-executor`, a path dependency) whose runner is spawned
  as a task on the worker's `current_thread` runtime, built with `event_interval(1)`. `spawn_connection` then submits
  the connection to the chosen worker's executor instead of to Tokio's first-in first-out queue, so the worker serves
  the newest ready connection first and the oldest once it waited `max_age`. Tokio itself is not patched: the order
  lives in the executor (a task that is not a connection, and the HTTP/2 stream tasks, still use Tokio's queue).

OxiRoute runs a service on non-stealing runtimes only when the configuration asks for
`connection_scheduling = "aged_per_worker"` (`ServerConf::work_stealing = false` with `connection_age_ms` set); the
default is Pingora's work-stealing runtime. See `docs/developer/PERF_6_3_RUNTIME_MODEL.md` and
`docs/developer/PERF_6_4_CONNECTION_SCHEDULING.md`.
