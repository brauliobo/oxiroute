# Vendored pingora-runtime patch

This directory contains the published `pingora-runtime 0.8.1` source from crates.io
(registry checksum `41815a13691a3e7d9ad0e34767d4140284132e31b95a4481f5e73ab6f407f834`)
under its upstream Apache-2.0 license. The benchmark target is dropped.

OxiRoute's delta is limited to `src/lib.rs`:

- `spawn_connection` places a new connection task on the non-stealing worker with the fewest open connections, the
  calling worker winning ties, and gives the slot back when the task ends. Upstream's `current_handle().spawn` picks a
  worker at random, which leaves a few connections unevenly spread for the whole life of long keep-alive clients. On a
  work-stealing runtime, or outside a pool thread, it is a plain `spawn` on the current runtime.
- The worker table keeps one load counter per worker. Tests cover the even spread, the slot hand-back, the tie rule and
  the work-stealing fallback (`cargo test --manifest-path vendor/pingora-runtime/Cargo.toml --offline`).

OxiRoute runs every service on non-stealing runtimes (`ServerConf::work_stealing = false`):
`docs/developer/PERF_6_3_RUNTIME_MODEL.md`.
