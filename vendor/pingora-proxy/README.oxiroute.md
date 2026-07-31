# Vendored pingora-proxy

This directory contains the normalized, published `pingora-proxy 0.8.1` source from crates.io
(registry checksum `8a92ee756ecf6ecb6419864da651cad6cecd933b6d420a26877031efa16bef57`)
under its upstream Apache-2.0 license. The crate archive's `.cargo_vcs_info.json` identifies upstream
commit `719ef6cd54e40b530127751bab6c1afc5ae815a8`; the package does not identify a corresponding tag.

`LICENSE` remains a byte-for-byte copy from Cargo's normalized registry extraction. OxiRoute's
delta is limited to isolated borrowed HTTP/1 upstream request preparation, inline-storage HTTP/1
task handoffs and response batches, and compatibility with the adjacent patched core:

- `Cargo.toml` resolves its direct `pingora-core 0.8.1` dependency through the adjacent
  `../pingora-core` path. The root workspace patches transitive crates.io references to the same
  path, so workspace builds contain one patched core. `pingora-proxy` is a standard workspace member
  while `pingora-core` remains an excluded path dependency. The adjacent core owns the shared
  `smallvec` task batch. Every published dev dependency is retained unchanged so the published
  examples remain buildable.
- `examples/connection_filter.rs` accepts the patched core's optional peer address, and
  `examples/virtual_l4.rs` initializes its optional connection-lifetime field to `None`, preserving
  the published examples' behavior while keeping all targets buildable.
- `proxy_trait.rs` adds `PreparedUpstreamRequest` and the additive `prepare_upstream_request` hook.
  Its default clones the downstream request and invokes the existing mutable
  `upstream_request_filter`, preserving external implementations and behavior.
- `lib.rs` re-exports the additive preparation result.
- `proxy_h1.rs` uses the preparation hook only for plain HTTP/1 requests with inactive caching.
  HTTP/2-to-HTTP/1 conversion and active cache mutation still clone and use the old mutable filter;
  H2 and custom upstream paths are unchanged. Borrowed headers use pingora-core's borrowed request
  writer, and every retry prepares from the unchanged downstream request. Its first per-batch task
  buffer and filtered output use the core's four-task inline batch with safe spill. Module filtering
  is shared over mutable task slices by the unchanged public `Vec<HttpTask>` writer and a private H1
  batch writer. Cache singleton responses also use inline storage. The two plain HTTP/1 task pumps
  use borrowed, stack-owned, four-slot SPSC channels; H2, custom-message, custom-session, and
  subrequest channels are unchanged.
- `spsc.rs` implements the safe fixed-capacity handoff with inline queue storage, single sender and
  receiver ownership, cancellation-safe reservation, close/drop wakeups, and panic-safe queued-value
  destruction.
- Inline tests cover SPSC capacity, FIFO ordering, cancellation and wakeup interleavings, closure,
  panic recovery, and randomized schedules; request/response pump saturation, ordering, failure, and
  disconnect behavior; task-buffer inline capacity and spill; Vec/batch response equivalence; module
filters and H1 output; default request ownership; HTTP/2 conversion; and active cache mutation.
The cache preparation tests also prove parsed H1 requests and parsed upstream responses enter active
cache handling without acquiring an original-case map.

The published crate excludes its upstream `tests/` directory, so the registry source contains only
the inline unit tests under `src/`. Consistently with `vendor/pingora-core`, packaging-only
`.cargo-ok`, `.cargo_vcs_info.json`, `Cargo.lock`, and `Cargo.toml.orig` files are not retained; the
workspace lockfile owns dependency resolution.

## Update procedure

1. Resolve and lock the intended crates.io release, then verify the cached `.crate` archive digest
   against the `Cargo.lock` checksum.
2. Remove the previous vendored directory and recreate it from that exact local registry extraction.
3. Bulk-copy only the normalized build source with `cp -a -- "$source/Cargo.toml" "$source/LICENSE"
   "$source/examples" "$source/src" vendor/pingora-proxy/`.
4. Reapply and review only the adjacent pingora-core dependency, connection-filter and virtual-L4
   example compatibility changes, additive preparation API, isolated HTTP/1 borrowed path, inline
   HTTP/1 task handoff and batch paths, and their focused tests. Restore every published dev
   dependency exactly, then restore this README with the new provenance.
5. Keep `vendor/pingora-proxy` in the root workspace members and `vendor/pingora-core` in its
   excludes. Regenerate the workspace lockfile offline only when Cargo reports that it is stale, then
   confirm root metadata lists the proxy exactly once and `cargo tree -i pingora-core` reports one
   `vendor/pingora-core` package for direct and transitive proxy edges.
6. Compare `LICENSE`, examples except the documented compatibility changes, and all `Cargo.toml`
   entries except the documented pingora-core path against the registry extraction before running
   the verification below.

## Vendor tests

The proxy is a standard workspace member, so the repository's normal locked, offline gate runs its
inline unit tests and compiles its published examples:

```sh
cargo test --workspace --all-targets --locked --offline
```

The SPSC and H1 pump tests can be inspected directly, and examples can be checked independently,
without creating a nested lockfile:

```sh
cargo test -p pingora-proxy --lib --locked --offline
```

```sh
cargo check -p pingora-proxy --examples --features connection_filter --locked --offline
```

The published crate contains no standalone integration tests. The retained dev dependencies are
resolved by the root lockfile; do not prune them to bypass an incomplete offline cache. Run strict
workspace clippy with:

```sh
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
```

Then run formatting and diff checks.
