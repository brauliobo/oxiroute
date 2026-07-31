# Vendored pingora-proxy

This directory contains the normalized, published `pingora-proxy 0.8.1` source from crates.io
(registry checksum `8a92ee756ecf6ecb6419864da651cad6cecd933b6d420a26877031efa16bef57`)
under its upstream Apache-2.0 license. The crate archive's `.cargo_vcs_info.json` identifies upstream
commit `719ef6cd54e40b530127751bab6c1afc5ae815a8`; the package does not identify a corresponding tag.

OxiRoute has no behavioral delta from the published crate. `Cargo.toml`, `LICENSE`, `src/`, and
`examples/` are byte-for-byte copies from Cargo's normalized registry extraction. The published
crate excludes its upstream `tests/` directory, so the registry source contains only the inline
unit tests under `src/`. Consistently with `vendor/pingora-core`, packaging-only `.cargo-ok`,
`.cargo_vcs_info.json`, `Cargo.lock`, and `Cargo.toml.orig` files are not retained.

## Update procedure

1. Resolve and lock the intended crates.io release, then verify the cached `.crate` archive digest
   against the `Cargo.lock` checksum.
2. Remove the previous vendored directory and recreate it from that exact local registry extraction.
3. Bulk-copy only the normalized build source with `cp -a -- "$source/Cargo.toml" "$source/LICENSE"
   "$source/examples" "$source/src" vendor/pingora-proxy/`.
4. Restore this README with the new version, checksum, and packaged VCS commit, without changing the
   copied manifest or Rust source.
5. Regenerate the workspace lockfile offline and confirm `cargo tree -i pingora-proxy` reports the
   `vendor/pingora-proxy` path.
6. Compare `Cargo.toml`, `LICENSE`, `src/`, and `examples/` recursively against the registry
   extraction before running the verification commands below.

## Vendor tests

Run the crate's packaged unit tests with the feature used by OxiRoute:

```sh
CARGO_TARGET_DIR=target/vendor-pingora-proxy \
  cargo +1.87.0 test --manifest-path vendor/pingora-proxy/Cargo.toml \
    --features openssl --offline --lib
```

The crate-local command may generate `vendor/pingora-proxy/Cargo.lock`; remove that generated file
afterward because the workspace lockfile owns dependency resolution. The published crate contains no
standalone integration tests. Exercise the local proxy integration paths, none of which require an
external OpenResty installation, with:

```sh
cargo +1.87.0 test -p oxiroute \
  --test health_checks \
  --test http_proxy_routing \
  --test websocket_proxy \
  --test wire_tls_interop \
  --locked --offline
```

Then run the full workspace test, strict clippy, formatting, and benchmark-validation gates.
