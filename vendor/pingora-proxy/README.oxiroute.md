# Vendored pingora-proxy

This directory contains the normalized, published `pingora-proxy 0.8.1` source from crates.io
(registry checksum `8a92ee756ecf6ecb6419864da651cad6cecd933b6d420a26877031efa16bef57`)
under its upstream Apache-2.0 license. The crate archive's `.cargo_vcs_info.json` identifies upstream
commit `719ef6cd54e40b530127751bab6c1afc5ae815a8`; the package does not identify a corresponding tag.

`LICENSE` remains a byte-for-byte copy from Cargo's normalized registry extraction. OxiRoute's
delta is limited to isolated borrowed HTTP/1 upstream request preparation and compatibility with
the adjacent patched core:

- `Cargo.toml` resolves its direct `pingora-core 0.8.1` dependency through the adjacent
  `../pingora-core` path and patches transitive crates.io references to the same path, so standalone
  and root builds contain one patched core. Every published dev dependency is retained unchanged so
  the published examples remain buildable in a complete Cargo environment.
- `examples/virtual_l4.rs` initializes the patched core's optional connection-lifetime field to
  `None`, preserving the published example's behavior while keeping all targets buildable.
- `proxy_trait.rs` adds `PreparedUpstreamRequest` and the additive `prepare_upstream_request` hook.
  Its default clones the downstream request and invokes the existing mutable
  `upstream_request_filter`, preserving external implementations and behavior.
- `lib.rs` re-exports the additive preparation result.
- `proxy_h1.rs` uses the preparation hook only for plain HTTP/1 requests with inactive caching.
  HTTP/2-to-HTTP/1 conversion and active cache mutation still clone and use the old mutable filter;
  H2 and custom upstream paths are unchanged. Borrowed headers use pingora-core's borrowed request
  writer, and every retry prepares from the unchanged downstream request.
- Inline clone-probe tests cover default ownership, HTTP/2 conversion, and active cache mutation.

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
4. Reapply and review only the adjacent pingora-core dependency and transitive patch, virtual-L4
   example compatibility initializer, additive preparation API, isolated HTTP/1 borrowed path, and
   clone-probe tests. Restore every published dev dependency exactly, then restore this README with
   the new provenance.
5. Regenerate the workspace lockfile offline and confirm root `cargo tree -i pingora-core` reports
   one `vendor/pingora-core` package for direct and transitive proxy edges.
6. Compare `LICENSE`, examples except the documented virtual-L4 initializer, and all `Cargo.toml`
   entries except the documented pingora-core path and patch against the registry extraction before
   running the verification below.

## Vendor tests

Direct crate-local tests retain and resolve the complete published dev-dependency graph. They can be
run in a complete Cargo environment with:

```sh
CARGO_TARGET_DIR=target/vendor-pingora-proxy \
  cargo +1.87.0 test --manifest-path vendor/pingora-proxy/Cargo.toml \
    --features openssl --lib
```

This standalone command may generate `vendor/pingora-proxy/Cargo.lock`; remove it afterward. An
offline checkout may be unable to resolve an uncached published dev dependency even when `--lib`
does not use it. Do not prune the manifest or install dependencies solely to bypass that condition.

The repository's offline verification exercises OxiRoute's clone probes and the local proxy wire
paths through the workspace lockfile:

```sh
cargo +1.87.0 test -p oxiroute --lib http_proxy::tests:: --locked --offline
```

```sh
cargo +1.87.0 test -p oxiroute \
  --test health_checks \
  --test http_proxy_routing \
  --test websocket_proxy \
  --test wire_tls_interop \
  --locked --offline
```

The published crate contains no standalone integration tests. Then run the full workspace tests,
strict clippy, formatting, and diff checks.
