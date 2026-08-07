# Testing

The project uses red-green-refactor. Start with the smallest failing test at the owning abstraction,
then add the runtime and wire evidence needed to support the user-facing claim.

## Local Gates

```sh
cargo +1.97.1 fmt --all --check
cargo +1.97.1 clippy --workspace --all-targets --locked --jobs 4 -- -D warnings
cargo +1.97.1 test --workspace --locked --jobs 4
pnpm --dir ui test
pnpm --dir ui build
pnpm --dir ui test:browser -- --workers=2
```

For a locked toolchain check when it is installed:

```sh
cargo +1.97.1 test --workspace --locked --jobs 4
```

The checked-in locked gate is Rust `1.97.1`, matching the workspace manifests and CI.

Security and release dependency gates can be run directly when the pinned tools are installed:

```sh
./scripts/verify-lockfiles.sh
cargo audit -D warnings
pnpm --dir ui audit --audit-level high
pnpm --dir remotion audit --audit-level high
```

The RustSec commands are intentionally fail-closed for the three current findings; see
[SECURITY_AUDIT.md](SECURITY_AUDIT.md) for their dependency paths and replacement policy.

The current worktree passes the Rust format, clippy, and locked workspace-test gates plus the UI
unit, browser type-check, build, and browser gates. The browser matrix has 21 passed tests and one
existing desktop-only skip. The bounded fuzz workspace format/check gates pass; the optional smoke
command exits without execution when `cargo-fuzz` is unavailable. None of these local checks is
production traffic, CA-staging, or process-level FFmpeg/OBS evidence.

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
| Real browser | Built dashboard desktop/mobile layout, in-memory token lifecycle, config writes/conflicts/external edits, SSE reconnect, controls, redaction, and provenance boundaries |

## Focused Commands

```sh
cargo test -p oxiroute-config --test lua_config
cargo test -p oxiroute-config-source --test resolver
cargo test -p oxiroute-import --test coverage_manifests
cargo test -p oxiroute-import --test nginx_rtmp
cargo +1.97.1 test -p oxiroute-import --test import_report --test varnish_foundation --locked --jobs 4
cargo +1.97.1 test -p oxiroute --test process_udp_drain --locked --jobs 4
cargo +1.97.1 test -p oxiroute-rtmp --test amf_hardening --test chunk_interop --locked --jobs 4
cargo test -p oxiroute --test rtmp_api
cargo test -p oxiroute --test wire_tls_interop
cargo test -p oxiroute-rtmp --test recording_store --test recording_worker
pnpm --dir ui test
```

Use the nearest focused test while iterating. Run the workspace gates before describing the change as
complete.

## What A Support Claim Needs

A capability should not move to `stable` without the relevant:

- success path;
- malformed-input and resource-bound cases;
- failure and rollback behavior;
- observable state and redaction behavior;
- reload/rotation or lifecycle coverage where applicable; and
- independent protocol or interoperability evidence where applicable.

Checked-in bounded parser fuzz scaffolding lives under `fuzz/`; the separate optional
`.github/workflows/fuzz-smoke.yml` workflow does not claim fuzz coverage and skips execution when
cargo-fuzz or nightly Rust is unavailable. The Linux workflow in `.github/workflows/ci.yml` enforces
the Rust and UI gates plus coverage-manifest and localhost-only browser validation; it does not run
the fuzz smoke. `.github/workflows/platform.yml` runs locked metadata on every
listed platform and explicitly gates full builds to Linux until the platform boundary changes.

The browser suite runs against the built static UI with Playwright route interception. Unexpected API
or non-local requests fail the test, SSE responses are scripted locally, and no daemon, public
endpoint, or ACME server is contacted. Install Chromium once with
`pnpm --dir ui test:browser:install` when running the suite locally.

The current browser coverage includes authenticated GET-only native import report browsing, redacted
source/provenance/preview rendering, read-only boundaries, and desktop/mobile layout. It does not
claim daemon, CA-staging, production, or FFmpeg/OBS interoperability.

Dependency and release gates live in `.github/workflows/audit.yml` and
`.github/workflows/release.yml`. `deny.toml` denies unknown dependency sources, unapproved licenses,
and unmaintained advisories; RustSec, cargo-deny, all lockfiles, both JavaScript dependency roots,
archive contents, and build provenance are separate checks rather than silent fallbacks. See
[SECURITY_AUDIT.md](SECURITY_AUDIT.md) for the current advisory policy.

The dependency audit remains open in this worktree: `cargo audit -D warnings` currently denies three
unmaintained-dependency warnings. Do not describe the audit as passing until those findings are
resolved. Direct OxiRoute PEM parsing has migrated to `rustls-pki-types`; the remaining
`rustls-pemfile` finding is transitive through Pingora, alongside Pingora's `daemonize` and
`derivative` findings.

Read [`fuzz/README.md`](../../fuzz/README.md) before running a target. Keep parser harnesses
isolated, deterministic, resource-bounded, and honest about unsupported protocol boundaries.
