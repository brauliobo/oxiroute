# Testing

The project uses red-green-refactor. Start with the smallest failing test at the owning abstraction,
then add the runtime and wire evidence needed to support the user-facing claim.

## Local Gates

```sh
cargo +1.97.1 fmt --all --check
cargo +1.97.1 clippy --workspace --all-targets --locked --jobs 4 -- -D warnings
./scripts/ci/tiers.sh check
./scripts/test/all.sh
./scripts/verify/control-plane-contract.sh
./scripts/verify/fuzz.sh
./scripts/verify/line_limit.sh
./scripts/verify/name_prefixes.sh
pnpm --dir ui test
pnpm --dir ui build
pnpm --dir ui verify:bundle
pnpm --dir ui test:browser -- --workers=2
```

The checked-in locked gate is Rust `1.97.1`, matching the workspace manifests and CI. `cargo test --workspace` still
runs everything on the default plane chain, one binary after the other, which is several times slower than the lanes.

## Test Tiers And Lanes

[`scripts/ci/tiers.txt`](../../scripts/ci/tiers.txt) lists every test binary once with the tiers it runs in, and
`./scripts/ci/tiers.sh check` (a lint step in CI) fails when a new `tests/*.rs` target, a `tests/*/main.rs` target or a
unit-test binary is in no tier. A lane runs the binaries of its tiers with
[cargo-nextest](https://nexte.st) (`cargo install cargo-nextest --locked`; CI pins version 0.9.150), so the lanes, local
and in CI, cannot skip a binary that another lane leaves unrun.

| Lane | Command | Tiers | What it proves |
| --- | --- | --- | --- |
| Core | `scripts/test/core.sh` | `core` | Plane-independent behavior, once, on the default plane chain |
| Planes | `scripts/test/planes.sh tokio\|epoll\|io_uring\|all` | `planes`, plus `engine`, `epoll` or `uring` for the driver | The plane-sensitive suites with `OXIROUTE_TEST_HTTP_DATA_PLANE` set, and the engine suites of the plane's driver (`EPOLL_H1_TRIGGER=level` on epoll, `EPOLL_H1_DRIVER=io_uring` on io_uring) |
| TLS | `scripts/test/tls.sh openssl\|rustls\|all` | `tls` | The downstream TLS suites once per backend (`OXIROUTE_INTERNAL_TEST_TLS_BACKEND`) |
| Documentation | `scripts/test/doc.sh` | | Doc tests, which nextest does not run |
| Slow | `scripts/test/slow.sh` | `slow` | Tests that wait out a real default duration; nightly and on demand, not on pull requests |
| Everything | `scripts/test/all.sh` | all but `slow` | What the pull request gate runs |

Before a plane lane runs, `scripts/ci/probe.sh <plane>` starts the real binary on that plane and checks that it
announces the plane; the binary refuses a plane the machine cannot run (a kernel with `kernel.io_uring_disabled`, a
seccomp policy that blocks `io_uring`), so an `io_uring` lane fails instead of running on epoll. CI assumes the
`ubuntu-24.04` hosted runner image (kernel 6.8 or newer, which has `io_uring` enabled) and prints the image and kernel
of every plane leg. Only Linux runs the plane matrix: `.github/workflows/platform.yml` checks nothing but metadata on
macOS and Windows, which have no epoll or `io_uring` plane and serve HTTP on tokio.

A binary is plane-sensitive when it reaches the plane switch of `tests/support/process.rs`: its configurations have a
plain HTTP listener with the fast lane, and the harness puts them on the suite's plane. Suites that pick their planes
themselves (`epoll_safety`, `epoll_responses`) run all of them in every lane and stay in `core`. To decide a new
binary, run it with `OXIROUTE_TEST_HTTP_DATA_PLANE=epoll`; when a suite starts the real binary on plain HTTP it is
`planes`, when it uses the TLS harness of `tests/support/proxy.rs` it is also `tls`.

CI (`.github/workflows/ci.yml`) builds every test binary once (`cargo nextest archive`), shares the archive, and runs
`core`, the three `planes` legs and the two `tls` legs from it in parallel; `.github/workflows/nightly.yml` runs `slow`.
The documentation tests (`doc`) and the UI, browser and lint jobs run beside the archive job and need nothing from it;
only `ui-api` takes a product of it (the debug `oxiroute` binary, shared as the small `oxiroute-debug` artifact, not the
1.3 GB archive). The io_uring leg raises `RLIMIT_MEMLOCK` to unlimited (`OXIROUTE_RAISE_MEMLOCK`, checked by
`planes.sh`) and runs as many tests at once as the other legs; only the locked-memory accounting test runs alone.

The CI Rust caches count against the 10 GB repository cap and are saved from `main` only. The `archive` job saves the
`tests` cache (restored read-only by `doc`), `lint` saves its own `lint` cache (a `cargo clippy` build is a check build
that shares no dependency artifacts with the test build, so it cannot restore `tests`), and `cpu-targets` saves one cache
per runner CPU (release objects built with `-C target-cpu`). No other job of `ci.yml` saves Rust build output.
A test whose wait is the behavior under test (a default read limit, a drain deadline) moves to the `slow` tier instead
of being deleted; every other wait is a short configurable one or an event.

The dev profile builds line tables only, and the CPU-bound scanners (`oxiroute-cpu`, `oxiroute-h1-head`) with
optimization, because their corpus tests run for minutes unoptimized; CI builds without debug information, which keeps
the archive small.

Security and release dependency gates can be run directly when the pinned tools are installed:

```sh
./scripts/verify/lockfiles.sh
cargo audit -D warnings
pnpm --dir ui audit --audit-level high
pnpm --dir remotion audit --audit-level high
```

The RustSec commands remain intentionally fail-closed. The current dependency graph passes both
advisory checks after replacing the affected Pingora dependency paths.

The bounded fuzz contract is required and validates the target/corpus set before the isolated compile;
the optional smoke command reports and skips execution when `cargo-fuzz` is unavailable. None of these local checks is
production traffic, CA-staging, or process-level FFmpeg/OBS evidence.

## Real CA-Staging Gate

The opt-in staging harness requires explicit `OXIROUTE_ACME_STAGING_*` environment variables and
never uses CI or local test endpoints. Run its non-mutating prerequisite check first:

```sh
bash scripts/acme-staging-evidence.sh --preflight
```

The preflight validates the loopback management boundary, local config and trust/token files,
tooling, DNS resolution, and Let's Encrypt staging reachability without starting OxiRoute or
requesting a certificate. The live invocation starts only the configured harness process and emits
redacted certificate evidence; it must not be used with production directory settings.

The deterministic harness checks remain local and do not contact a CA:

```sh
bash scripts/test/acme-staging-evidence.sh
```

Build tasks should use at most four workers in constrained environments. For make-based tooling, use
`nice make -j4`; Cargo commands above pass `--jobs 4` explicitly.

## Test Layers

| Layer | Proves |
| --- | --- |
| Unit/property | Bounds, normalization, defaults, validation, rendering, state machines, counters, and diagnostics |
| Integration | Loopback listener behavior, generation lifecycle, APIs, health checks, recording, files, and shutdown |
| Import conformance | Include graphs, provenance, decision ledgers, blockers, finalized candidate behavior, and synthetic fixtures |
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
cargo test -p oxiroute --test reverse_http3_golden
cargo +1.97.1 test -p oxiroute --lib contract:: --locked --jobs 4
cargo test -p oxiroute --test wire_tls_interop
cargo test -p oxiroute-rtmp --test recording_store --test recording_worker
pnpm --dir ui test
```

The `reverse_http3_golden` test replays a corpus of reverse HTTP/3 requests (routes, static files, proxying, cache,
limits, protocol violations) against one daemon and compares the normalised observations with the per-area files in
`crates/oxiroute-server/tests/fixtures/reverse_http3_golden/`, printing a per-case diff on any difference.
`OXIROUTE_INTERNAL_TEST_RECORD_GOLDEN=1 cargo test -p oxiroute --test reverse_http3_golden` re-records every area
file; `OXIROUTE_INTERNAL_TEST_RECORD_CASES=id1,id2,stats` re-records only the listed cases (and the counters), so a
behaviour change is a deliberate fixture change. Cases marked `tolerance` are compared without their access log
lines (their content depends on timing); cases marked `deviation` record a difference from the original `h3` crate
path with its RFC reason.

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

Checked-in bounded parser fuzz scaffolding lives under `fuzz/`; `scripts/verify/fuzz.sh` is a required
manifest, harness, corpus, format, and locked-compile gate. The separate optional
`.github/workflows/fuzz-smoke.yml` workflow does not claim fuzz coverage and reports/skips execution
when cargo-fuzz or nightly Rust is unavailable; detected but broken optional tooling fails closed. The
Linux workflow in `.github/workflows/ci.yml` enforces the fuzz contract alongside the Rust and UI gates
plus coverage-manifest and localhost-only browser validation. `.github/workflows/platform.yml` runs locked metadata on every
listed platform and explicitly gates full builds to Linux until the platform boundary changes.

The browser suite runs against the built static UI with Playwright route interception. Unexpected API
or non-local requests fail the test, SSE responses are scripted locally, and no daemon, public
endpoint, or ACME server is contacted. Install Chromium once with
`pnpm --dir ui test:browser:install` when running the suite locally.

### UI stack notes

- The UI is Vue 3.6 in Vapor mode on an exact-pinned release candidate (`ui/package.json`). Vapor
  components cannot be mounted by `@vue/test-utils`, so the unit tests import `mount` and
  `flushPromises` from `ui/src/test/mount.ts`: it mounts with `createVaporApp` and wraps the result
  in the VTU `DOMWrapper`. Setup state is closed in Vapor; a test that needs it reads what the
  component publishes with `defineExpose` through `wrapper.vm`.
- `ui/vitest.config.ts` aliases `vue` and the `@vue/*` runtime packages to their esm-bundler builds
  (Node would otherwise load the CommonJS builds, which have no Vapor runtime).
- `ui/src/sfcPolicy.test.ts` fails when a template uses `$emit`, `$props`, `$attrs`, `$slots`, inline
  styles, or other syntax that Vapor's production build or a strict CSP rejects. Unit tests compile
  in development mode, so only a production build (`pnpm --dir ui build` plus the browser suite)
  proves the bundle runs; `pnpm --dir ui verify:bundle` checks that no VDOM renderer was bundled.
- `vue-tsc` does not type-check `lang="pug"` templates. `@vue/language-plugin-pug` would, but it
  currently reports 21 template errors that are not fixed yet: 13 `:inert` bindings typed as
  `'' | null` instead of a boolean, 2 `:aria-busy` bindings typed `string | null`, an unchecked
  `origins[0]`, `HeaderMutation<T>` generics, the bearer-only access policy inputs on the union that
  also contains the htpasswd policy, and a `stream_name` field passed to a setter that does not
  accept it. Fix them before adding the plugin and `vueCompilerOptions.plugins`.
- The local Playwright run needs Chromium; when the Playwright download is unavailable, point a
  config that extends `ui/playwright.config.ts` at a system Chromium with
  `use.launchOptions.executablePath`.

The current browser coverage includes authenticated GET-only native import report browsing, redacted
source/provenance/preview rendering, read-only boundaries, and desktop/mobile layout. It does not
claim daemon, CA-staging, production, or FFmpeg/OBS interoperability.

Dependency and release gates live in `.github/workflows/audit.yml` and
`.github/workflows/release.yml`. `deny.toml` denies unknown dependency sources, unapproved licenses,
and unmaintained advisories; RustSec, cargo-deny, all lockfiles, both JavaScript dependency roots,
archive contents, and build provenance are separate checks rather than silent fallbacks.

The dependency audit passes locally: `cargo audit -D warnings` and the pinned cargo-deny policy
report no advisory findings after the Pingora dependency paths were replaced with maintained
implementations. Direct and vendored Pingora PEM parsing use `rustls-pki-types`; no advisory
ignore entries were added.

Read [`fuzz/README.md`](../../fuzz/README.md) before running a target. Keep parser harnesses
isolated, deterministic, resource-bounded, and honest about unsupported protocol boundaries.
