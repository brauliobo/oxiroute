# Developer guide

Start here to build, test, and change OxiRoute. The Rust workspace owns the proxy and control plane;
the Vue/Pug dashboard consumes the management API. A successful build alone does not establish
protocol support. The [compatibility matrix](../COMPATIBILITY.md) records that boundary.

## Set up a checkout

Run commands from the repository root. Use Rust `1.97.1`, Node.js `22`, and pnpm `11.3.0`, matching
CI. The workspace declares `rust-version = "1.97"`; the tested patch release is `1.97.1`.
Native dependencies need a C/C++ toolchain, CMake, pkg-config, and OpenSSL development files.
See the [local walkthrough](../user/GETTING_STARTED.md) for runtime prerequisites and verification.

```sh
rustup toolchain install 1.97.1 --component rustfmt --component clippy
pnpm --dir ui install --frozen-lockfile
nice pnpm --dir ui build
nice cargo +1.97.1 build --locked --jobs 4 -p oxiroute
./target/debug/oxiroute version
```

Use the binary at `target/debug/oxiroute`; a source build does not put `oxiroute` on your PATH or
install a service. Linux has the full-build CI gate. Platform metadata checks are a narrower claim;
read [testing](TESTING.md) before treating another platform as supported.

## Find the owner

Read [architecture](ARCHITECTURE.md) for the source-to-generation lifecycle. This table groups the
workspace by responsibility rather than listing every internal type.

| Crate or directory | Owns |
| --- | --- |
| `crates/oxiroute-server` | CLI, daemon, planning, generations, listeners, management APIs, monitoring, topology |
| `crates/oxiroute-config` | Canonical typed model, defaults, validation, restricted Lua |
| `crates/oxiroute-config-source` | Source adapters, templates, native references, composition, rendering |
| `crates/oxiroute-import` | Native parsers, provenance, diagnostics, conservative lowering |
| `crates/oxiroute-forward-proxy` | Target parsing, authentication, destination policy, tunnel primitives |
| `crates/oxiroute-h1-head`, `oxiroute-h2`, `oxiroute-h3` | HTTP protocol components |
| `crates/oxiroute-cache` | Bounded memory and persistent cache core |
| `crates/oxiroute-rtmp` | RTMP sessions, catalog, fanout, recording, FLV, relays |
| `crates/oxiroute-acme`, `oxiroute-tls-client`, `oxiroute-tls-rustls` | Certificate lifecycle and TLS components |
| `crates/oxiroute-supervision*`, `oxiroute-supervisor-*` | Replacement state machines, Unix transport, Linux master/worker processes |
| `crates/oxiroute-aged-executor`, `oxiroute-cpu`, `oxiroute-worker-scaling` | Execution, CPU discovery, worker scaling |
| `crates/oxiroute-allocator`, `vendor/` | Allocator selection, upstream integrations, documented local patches |
| `ui/` | Dashboard, client contracts, component and browser tests |
| `contracts/` | Checked control-plane API artifact |
| `website/` | Static public documentation site |
| `remotion/` | Reproducible documentation media |

A component crate's existence does not mean every listener uses it. Trace the active daemon path
before describing an implementation detail as a feature.

## Make a change

1. Check the worktree and leave unrelated changes alone. Read the workspace instructions.
2. Read the relevant contract and the nearest tests. Reproduce a reported failure at its owning layer.
3. Run GitNexus impact analysis before editing an existing function, class, or method; inspect callers
   and execution flows. Investigate stale or degraded results before relying on them.
4. Correct the owning abstraction and add focused behavior coverage. Verify failure, bounds,
   redaction, and lifecycle behavior when relevant.
5. Update the affected guide/reference and compatibility status together. Planned behavior stays
   labeled planned until runtime evidence exists.
6. Run the focused checks, then the applicable repository gates in [testing](TESTING.md).

For example, configuration and resolver changes can start with:

```sh
nice cargo +1.97.1 test --locked --jobs 4 -p oxiroute-config --test lua_config
nice cargo +1.97.1 test --locked --jobs 4 -p oxiroute-config-source --test resolver
```

The core Rust and dashboard checks are:

```sh
cargo +1.97.1 fmt --all --check
nice cargo +1.97.1 clippy --workspace --all-targets --locked --jobs 4 -- -D warnings
nice ./scripts/test/all.sh
pnpm --dir ui test
nice pnpm --dir ui build
```

These commands are a starting set. [Testing](TESTING.md) defines additional required contract,
fuzz, browser, dependency, and release gates; a focused test is not a substitute for them.

## Contracts and evidence

- [Configuration](../CONFIG_SPEC.md) and [source formats](../CONFIG_FORMATS.md): accepted objects and validation.
- [API/UI](../API_UI_SPEC.md) and [CLI](../reference/CLI.md): external behavior and revisions.
- [Operations](../OPERATIONS.md): activation, readiness, reload, drain, and shutdown.
- [Import](../IMPORT_SPEC.md) and [RTMP](../RTMP_SPEC.md): feature-specific contracts.
- [Benchmarks](../BENCHMARKS.md): historical measurements, scope, and reproducibility.
- [Documentation maintenance](DOCUMENTATION.md): audience, language, evidence, and source ownership.
- [Releasing](RELEASING.md): release artifacts and maintainer checks.
- [Shared TLS context](TLS_CLIENT_CONTEXT.md): outbound trust ownership and reload cost.
