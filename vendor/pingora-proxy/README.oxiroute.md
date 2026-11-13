# Vendored pingora-proxy

This directory contains the normalized, published `pingora-proxy 0.9.1` source from crates.io
(registry checksum `8e3d5670490740405ecc8c6f75c3fcbce4d24f7adaffb355ca44550e7918e358`)
under its upstream Apache-2.0 license. The crate archive's `.cargo_vcs_info.json` identifies upstream
commit `f86ffda5d3ae5c3baf5125682138b33b26c01b08`. The 0.8.1 to 0.9.1 rebase clears RUSTSEC-2026-0390 (client
`Connection` nominations and hop-by-hop headers forwarded to HTTP/1 upstreams) for this crate: upstream's
`HttpUpstreamRequestPolicy` is part of the crate. OxiRoute sets `preserve()` on its peers (see below) and owns
request header sanitation in its own request policy, so the advisory is cleared in this crate by the version while
the product's behaviour is decided by that policy, not by Pingora's.

`LICENSE` remains a byte-for-byte copy from Cargo's normalized registry extraction. OxiRoute's
delta is limited to isolated borrowed HTTP/1 upstream request preparation, inline-storage HTTP/1
task handoffs and response batches, the cancellable upstream attempt, and compatibility with the adjacent patched core:

- `Cargo.toml` resolves its direct `pingora-core 0.9.1` dependency through the adjacent
  `../pingora-core` path. The root workspace patches transitive crates.io references to the same
  path, so workspace builds contain one patched core. `pingora-proxy` is a standard workspace member
  while `pingora-core` remains an excluded path dependency. The adjacent core owns the shared
  `smallvec` task batch. The published dev dependencies are retained, with `reqwest 0.13` (without a bundled TLS
  provider) and `tokio-tungstenite 0.30`, so the published examples remain buildable. There is no `rustls` feature
  (`pingora-rustls` is no longer vendored) and no `shutdown_signal` bench (its source is not published).
- `examples/virtual_l4.rs` initializes its optional connection-lifetime field to `None`, preserving
  the published examples' behavior while keeping all targets buildable.
- `proxy_trait.rs` adds `PreparedUpstreamRequest` and the additive `prepare_upstream_request` hook.
  Its default clones the downstream request and invokes the existing mutable
  `upstream_request_filter`, preserving external implementations and behavior.
- `lib.rs` re-exports the additive preparation result.
- `proxy_trait.rs` adds the additive `upstream_attempt_cancelled` hook (default `None`) and the
  `UpstreamCancellation` future type, re-exported by `lib.rs`. `HttpProxy::proxy_to_upstream` moves the
  attempt after `upstream_peer` (client session, H1/H2/custom proxying, session release) into
  `proxy_attempt` and races it against the cancellation with a biased `tokio::select!`. When the
  cancellation wins, the attempt is dropped (its upstream connection closes without returning to the
  pool) and its error goes through `error_while_proxy` like any attempt error. Inline tests cover the
  dropped connection and the unaffected default.
- `proxy_h1.rs` applies upstream's request policy only when the peer's `http_upstream_request_policy` is not
  `preserve()`: `preserve()` keeps the zero-clone borrowed write (OxiRoute sets it on every peer and sanitizes the
  request itself); any other policy clones the request, sanitizes it, reconciles the authority and framing as
  upstream's owned path does, and returns it owned. A request the preparation hook returned owned gets the policy
  after the hook.
- `proxy_h1.rs` and `proxy_h1_inline.rs` mirror upstream's 0.9 H1 correctness fixes by hand: the upstream connection
  is reused only when the request write did not fail and the response was read to its end (`upstream_can_reuse`,
  `pipe_state`); the pump and the inline relay run the `upstream_modules` hooks; request-target forms and the
  cancel-safe header read live in core. `proxy_h2.rs` keeps the `spsc` queue under upstream's new pipe-state logic
  (`spsc::Sender::closed` added).
- `proxy_h1.rs` uses the preparation hook only for plain HTTP/1 requests with inactive caching.
  HTTP/2-to-HTTP/1 conversion and active cache mutation still clone and use the old mutable filter;
  H2 and custom upstream paths are unchanged. Borrowed headers use pingora-core's borrowed request
  writer, and every retry prepares from the unchanged downstream request. Its first per-batch task
  buffer and filtered output use the core's four-task inline batch with safe spill. Module filtering
  is shared over mutable task slices by the unchanged public `Vec<HttpTask>` writer and a private H1
  batch writer. Cache singleton responses also use inline storage. The two plain HTTP/1 task pumps
  use borrowed, stack-owned, four-slot SPSC channels; H2, custom-message, custom-session, and
  subrequest channels are unchanged.
- `proxy_trait.rs` declares the hooks as `fn ... -> impl Future<Output = ...> + Send` (return-position
  `impl Trait` in traits) instead of `#[async_trait]`: an implementation still writes `async fn`, and a hook is
  awaited inside the future of the request instead of being boxed (and its state copied to the heap) for every call.
  A hook default with no body of its own is an `async move` block that returns at once.
- `lib.rs` `HttpProxy::process_request` borrows the request context (`&mut CTX`) from a `Box` made by its callers instead
  of taking it by value: an async fn stores a by-value parameter and the local it is moved into, so the 1.9 KiB OxiRoute
  context was in every suspend state of the request future twice.
- Cold or alternative awaitees are boxed so they stay out of the request future: `handle_error` (now a plain fn that
  returns the boxed `handle_error_unboxed`), the HTTP/2 and custom upstream arms of `proxy_attempt`, and
  `proxy_1to1_pumped` (the two-pump relay that `proxy_1to1` falls back to when the inline relay does not apply).
- `lib.rs` `HttpProxy` implements `HttpServerApp::process_new_http_inline` (see `pingora-core`); `process_new_http`
  delegates to it.
- `lib.rs` `handle_new_request` waits for one shutdown signal while it reads a request head: the watch its owner
  shares for a connection that is between requests (`Option<&ShutdownWatch>`), upstream's `await_shutdown()` for the
  first request. `process_new_http_inline` also carries the keepalive-context handoff. `filter_response_tasks` loops
  over upstream's per-task `downstream_response_task_filter`, so the 101 rules hold for batches.
- `proxy_trait.rs` `adjust_upstream_modules` is declared `-> impl Future + Send`; upstream's other new hooks are
  synchronous with default bodies and unchanged.
- `proxy_h1_inline.rs` relays an HTTP/1 request in one loop (`HttpProxy::relays_inline`: an HTTP/1 or HTTP/2 client
  and no cache involvement): the loop waits for the next response task of the upstream session or the next request
  body chunk of the client. A chunk goes through the two request body filters and is written to the upstream where it
  was read; up to four response tasks that are ready are filtered by `h1_response_filter` and written to the client as
  one batch; a request without a body sends its end first, a retried request sends the buffered body first, and a
  client that closes before the response ends the request (the idle read of `proxy_handle_downstream`). Upgrades
  switch the loop to the raw byte mode where either side ending ends the exchange. `proxy_1to1` uses it before
  creating the channels; only requests with a cache and HTTP/3 clients keep the two-future pump. The result, the
  filters and their order, the error mapping (a failed upstream read or request end becomes a `HttpTask::Failed`, a
  failed request write explains a later failed response read) and the downstream reuse decision are the pump's. The
  one difference is that a blocked write on one side no longer lets the other side's task run ahead by up to a
  channel of tasks: neither half could make progress while the other was blocked in a write before either.
  When the upstream refuses the rest of the request body, what the client still uploads is read and dropped until the
  upstream's answer is relayed, so data that arrives before the answer is visible does not end the exchange.
- `proxy_h2.rs` carries the response tasks of an HTTP/2 origin from the response reader to the downstream loop through
  the `spsc` queue (four slots, as the HTTP/1 pump) instead of a `tokio::sync::mpsc` channel; its batches are drained
  with `try_recv`. The two-future structure of the HTTP/2 upstream path is unchanged.
- `lib.rs` `handle_new_request` takes the downstream request header with `req_header_mut_keeping_facts()` when no
  downstream module is installed (nothing then changes its framing), so the facts noted at parse still hold; and
  `proxy_h1.rs` hands them to `write_request_header_noted` (a request the preparation hook replaced has its own
  facts computed).
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
   example compatibility changes, additive preparation API, cancellable upstream attempt, isolated HTTP/1 borrowed path, inline
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
- `src/lib.rs` `HttpProxy::h2_options` returns the `Copy` `oxiroute_h2::server::Config` (the downstream HTTP/2 options of `pingora-core`) without a clone.
- `src/lib.rs` `HttpProxy::process_request` honors `FailToProxy::can_reuse_downstream` for the final error of the
  upstream attempts too (upstream's only honors it for errors before peer selection): a hook that wrote a complete
  response and ends the request through an error, as OxiRoute's cache does after a revalidation, keeps the keep-alive
  connection. The default hook still returns `false`. `proxy_trait.rs` documents the changed contract.
- `src/lib.rs` `HttpProxy::proxy_attempt` watches the downstream session (`Session::closed`) while the upstream session
  is being obtained, which can queue for a connection of a capped server: a client that leaves meanwhile ends the
  attempt with a downstream `ConnectionClosed` error before anything goes upstream. The watch is built only when the
  connection is not ready on the first poll.
