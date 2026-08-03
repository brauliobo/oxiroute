# Testing

The project uses red-green-refactor. Start with the smallest failing test at the owning abstraction,
then add the runtime and wire evidence needed to support the user-facing claim.

## Local Gates

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --jobs 4 -- -D warnings
cargo test --workspace --jobs 4
pnpm --dir ui test
pnpm --dir ui build
```

For a locked toolchain check when it is installed:

```sh
cargo +1.87 test --workspace --locked --jobs 4
```

Build tasks should use at most four workers in constrained environments. For make-based tooling, use
`nice make -j4`; Cargo commands above pass `--jobs 4` explicitly.

## Test Layers

| Layer | Proves |
| --- | --- |
| Unit/property | Bounds, normalization, defaults, validation, rendering, state machines, counters, and diagnostics |
| Integration | Loopback listener behavior, generation lifecycle, APIs, health checks, recording, files, and shutdown |
| Import conformance | Include graphs, provenance, decision ledgers, blockers, finalized candidate behavior, and sanitized live fixtures |
| Protocol interoperability | Independent HTTP/TLS/H2/gRPC/WebSocket/RTMP clients and wire behavior |
| UI contract/component | Exact API shapes, stale/conflict behavior, responsive controls, navigation, and redaction |

## Focused Commands

```sh
cargo test -p oxiroute-config --test lua_config
cargo test -p oxiroute-config-source --test resolver
cargo test -p oxiroute-import --test coverage_manifests
cargo test -p oxiroute-import --test nginx_rtmp
cargo test -p oxiroute --test rtmp_api
cargo test -p oxiroute --test wire_tls_interop
cargo test -p oxiroute-rtmp --test recording_store --test recording_worker
pnpm --dir ui test
```

Use the nearest focused test while iterating. Run the workspace gates before describing the change as
complete.

## What A Support Claim Needs

A capability should not move to `implemented` without the relevant:

- success path;
- malformed-input and resource-bound cases;
- failure and rollback behavior;
- observable state and redaction behavior;
- reload/rotation or lifecycle coverage where applicable; and
- independent protocol or interoperability evidence where applicable.

There is currently no checked-in fuzz target or real-browser runner. The Linux workflow in
`.github/workflows/ci.yml` enforces the Rust and UI gates plus coverage-manifest validation, but it
does not imply browser or platform coverage.

Fuzzing remains a documented follow-up. Do not add `cargo-fuzz` or a fuzzing dependency until an
isolated parser/protocol target can compile and run deterministically with Rust 1.87; the release
workflow deliberately does not claim that coverage yet.
