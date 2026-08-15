# OxiRoute 0.5.1

OxiRoute 0.5.1 reduces idle CPU use and completes the runtime, configuration-proof, supervision, and
control-plane ownership work developed after the published 0.5.0 release. The release remains
pre-alpha and retains conservative compatibility claims.

## Highlights

- Block recorder cleanup, recorder reaping, and child-process reaping threads when they have no work
  instead of waking on a fixed polling interval. Increase supervised master and worker poll intervals
  from 5 ms to 20 ms while retaining bounded lifecycle responsiveness.
- Compile validated configuration into immutable generation blueprints, then acquire TLS, pools,
  caches, listeners, RTMP stores, and runtime resources through generation-owned preparation and
  rollback boundaries.
- Move the public mutable configuration boundary to `ConfigDraft` and require an owned validation
  transition to `ValidatedConfig` before planning, rendering, TLS preparation, listener reservation,
  generation activation, or successful import publication.
- Replace direct RTMP runtime ownership with opaque value plans, prepared/runtime sets, control and
  service handles, and generation-owned media, recording, relay, callback, VOD, fanout, and exec
  resources.
- Unify TCP, forward HTTP/2, RTMP, HTTP/3, and UDP listener admission around an authoritative
  descriptor inventory, including supervised descriptor parity and reload catalog ownership.
- Strengthen supervision with worker process-tree containment, delegated cgroup subtrees, launcher
  metadata validation, contained RTMP exec processes, and monotonic lifecycle snapshots.
- Bound shared backend and generation preparation waits and preserve reverse-order cleanup when
  staged acquisition fails or shutdown reaches its absolute deadline.
- Add registry-owned authenticated read-only management endpoints, typed response DTOs, a checked
  OpenAPI 3.1 contract, generated TypeScript bindings, and UI contract tests aligned with those
  schemas.
- Install `rustfmt` in release verification and build source archives from committed `HEAD`, with a
  deterministic regression test proving dirty tracked files cannot contaminate release artifacts.

## API And Architecture Changes

- `oxiroute-config` replaces the mutable public `Config` and mutating validation facade with
  `ConfigDraft` and `ValidatedConfig`. Restricted Lua loading and typed rendering now belong to
  `oxiroute-config-source`.
- Import candidates expose validated success evidence while blocked canonical drafts remain owned by
  the importer. nginx RTMP directive compatibility APIs move to `oxiroute_import::nginx`.
- `oxiroute-rtmp` removes raw registry, hub, store, recorder-worker, and service-runtime ownership
  exports in favor of opaque plans and handles. Runtime planning APIs now require validated
  configuration proof.
- The management control-plane contract is checked in at
  `contracts/control-plane.openapi.json`; its generated endpoint registry and DTO schemas are the
  source of truth for the generated UI bindings.
- The classified public API ledgers and authenticated generation baseline under `docs/developer`
  remain the exact compatibility evidence for the coordinated 0.5 API boundary.

## Verification Boundary

The low-load change is covered by existing recorder, reaper, supervision, shutdown, and deployment
wakeup verification. A scheduler-timing or `/proc` assertion was intentionally not added because it
would be environment-dependent; the deterministic functional suites verify command delivery,
shutdown, reaping, deadlines, and lifecycle outcomes while deployment evidence verifies reduced idle
wakeups.

Release gates cover Rust 1.97.1 formatting, strict workspace clippy, locked workspace tests, checked
lockfiles and fuzz targets, the OpenAPI and generated TypeScript contract, classified public API
baselines, authenticated generation behavior, release-version alignment, and deterministic archive
policy. CA staging, active production traffic, broad external interoperability, and long-running fuzz
campaigns remain separate evidence gates.
