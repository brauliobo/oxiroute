# Code Reuse, Encapsulation, and Decomposition Plan

Status: repository-wide audit snapshot from 2026-08-10. This document plans changes; it does not
claim that the refactors have been implemented.

## Scope and Method

This audit covers first-party runtime, configuration, import, RTMP, cache, ACME, forward-proxy,
supervision, UI, test-support, release, benchmark, and fuzz code. It excludes `vendor/`, lockfiles,
generated benchmark evidence, binary/media assets, and intentionally independent configuration
fixtures.

The audit combined:

- A fresh GitNexus rebuild after repairing an inconsistent full-text index: 22,994 nodes, 68,742
  relationships, 827 clusters, and 300 execution flows.
- GitNexus hub, caller, file-import, large-symbol, cycle, context, and upstream-impact queries.
- First-party line-count inventory and direct source review of every finding below.
- Parallel subsystem reviews of server/runtime, config/import, supporting Rust crates, UI, tests, and
  tooling.

The MCP process retained its pre-rebuild graph snapshot, which was four commits behind `main`, even
after the on-disk index was refreshed. Graph topology was therefore used as a prioritization aid, and
current source was treated as authoritative for line-level findings. The refreshed index should be
loaded in a new MCP session before implementing any item.

No directed file-import cycles were found. The main graph hubs are:

| Boundary | Graph signal | Planning consequence |
| --- | ---: | --- |
| `ui/src/config.ts` | 39 importing files | Preserve a barrel while splitting model, decoding, and field registry. |
| `ui/src/api.ts` | 25 importing files; `request` has 29 direct callers | Split behind stable endpoint exports; do not combine transport changes with endpoint moves. |
| `validate_config` | CRITICAL; 46 impacted symbols, 4 process groups | Treat a draft/validated type split as a separate design project. |
| `render_lua` | CRITICAL; 91 impacted symbols, 4 process groups | Move domain renderers without changing output or the facade. |
| `runtime_plan` | CRITICAL; 71 impacted symbols, 8 process groups | Extract protocol compilers behind the existing facade. |
| generation `prepare` | CRITICAL; 72 impacted symbols, 5 process groups | Characterize activation and rollback ordering before consolidation. |
| `CanonicalCandidate` | CRITICAL across importer modules | Change finalization in one migration with all product lowerers and report tests. |

## Priority Model

- **P0**: confirmed correctness drift or a broken user-visible path. Fix before structural cleanup.
- **P1**: security/invariant ownership or duplicated safety-critical behavior.
- **P2**: substantial reuse, encapsulation, or decomposition with bounded behavioral risk.
- **P3**: mechanical shared helpers, test organization, and low-risk file moves.

## Implementation Rules

1. Add characterization or failing tests before changing shared behavior.
2. Change the owning abstraction rather than adding call-site guards or alternate paths.
3. Keep behavior changes, public API changes, and move-only file splits in separate changes.
4. Preserve serialized configuration, API JSON, metrics names/labels, CLI output, diagnostic paths,
   audit records, and on-disk formats unless a work item explicitly changes that contract.
5. Do not create a generic `utils` module. Put each helper beside the policy or lifecycle it owns.
6. Do not infer reuse from similar names alone. Shared code must have the same invariants and failure
   contract.
7. Run GitNexus impact analysis on each existing symbol before editing it. CRITICAL boundaries require
   direct-caller review and focused rollback tests.
8. Re-run GitNexus change detection and cycle checks after each implementation slice.

## Phase 0: Correct Confirmed Drift

### P0.1 Replace-in-flight UI requests

**Finding:** Token changes can abort the old request and lose the replacement request because loaders
return while `loading` or `loadRequest` still describes the aborted operation.

**Evidence:**

- `ui/src/EventsWorkspace.vue:102-120,201-215`
- `ui/src/CertificatesWorkspace.vue:212-236,374-385`
- `ui/src/OperationsWorkspace.vue:243-275,411-424`
- `ui/src/ProvenanceWorkspace.vue:214-301`

**Plan:**

- [ ] Add tests that hold token-A requests open, switch to token B, and prove B starts immediately.
- [ ] Introduce a focused `useLatestAbortableTask` composable that owns one generation-tagged task,
  aborts and replaces in-flight work, ignores stale completion, and cancels on unmount.
- [ ] Migrate the four workspaces without changing each workspace's stale-data or error policy.
- [ ] Prove an aborted task cannot clear the replacement task's loading state or emit unauthorized.

### P0.2 Forward-cache diagnostic paths

**Finding:** The model stores cache policy at `forward_proxy_services[].cache`, while the editor and
field registry advertise `forward_proxy_services[].header_policy.cache`. Backend diagnostics cannot
focus the corresponding controls.

**Evidence:**

- Model: `ui/src/config.ts:885-898`
- Registry: `ui/src/config.ts:1906-1943`
- Editor: `ui/src/configuration/ForwardProxyServiceEditor.vue:194-355`

**Plan:**

- [ ] Correct field paths before performing component extraction.
- [ ] Add a diagnostic-navigation test for `forward_proxy_services[0].cache.store`.
- [ ] Generalize `HttpCachePolicyEditor.vue` into a model-value component with `fieldPath` and
  `storeNames`, then use it for HTTP-route and forward-proxy cache policies.

### P0.3 Static-file semantics across HTTP adapters

**Finding:** HTTP/1-2 evaluates `If-Match`, `If-Unmodified-Since`, `If-None-Match`,
`If-Modified-Since`, and `If-Range`; HTTP/3 evaluates ranges directly. HTTP/3 can therefore return a
body or partial body where the other adapters return `304`, `412`, or a full body.

**Evidence:**

- HTTP/1-2: `crates/oxiroute-server/src/http_proxy.rs:2130-2365`
- HTTP/3: `crates/oxiroute-server/src/http3.rs:1976-2048,2206-2362`

**Plan:**

- [ ] Add one protocol-parity matrix for GET/HEAD, every precondition, strong/date `If-Range`,
  duplicate/multi-range headers, empty files, and ETag-disabled plans.
- [ ] Move precondition and range evaluation into a protocol-neutral static-request decision owned
  beside `StaticFilesPlan`.
- [ ] Keep file streaming and header serialization in the Pingora and H3 adapters.

### P0.4 Decide HTTP/3 reverse-upstream behavior

**Prior finding:** The runtime rejected routes without an H3 upstream before returning through the H3
transport, which made the subsequent Pingora H1/H2 fallback unreachable.

**Resolved contract:** Reverse HTTP/3 proxy routes require an exact H3 upstream pool and dispatch H3
to H3. Configuration rejects exact H1, flexible H1/H2, and exact H2 pools. Runtime retains a
defensive `502` invariant check when no H3 transport is available; it does not fall back to Pingora
H1/H2 sessions. Validation covers all rejected pool capabilities and exact H3 acceptance, while the
process test proves that the origin negotiates H3.

## Phase 1: Put Invariants at Their Owners

### P1.1 Import candidate finalization

**Finding:** `CanonicalCandidate.config` is independently public and writable, while report assembly
later hides finalized config when blockers exist. Eight product finalizers manually construct related
state, and Squid carries a special parallel shape.

**Evidence:**

- Owner: `crates/oxiroute-import/src/candidate.rs:150-225`
- Defensive report correction: `crates/oxiroute-import/src/evidence.rs:394-415`
- Finalizers: `nginx/lower/report.rs:182-209`, `nginx/rtmp_lower.rs:209-230`,
  `nginx/stream_lower.rs:127-148`, `nginx/root.rs:1123-1151`,
  `haproxy/lower/report.rs:165-193`, `apache/lower.rs:831-865`,
  `squid/importer.rs:218-254`, and `varnish/lower.rs:1671-1687`

**Plan:**

- [ ] Make finalized config private and represent blocked/finalized outcomes explicitly.
- [ ] Require the owning finalizer to receive the product eligibility decision; retain product-specific
  diagnostic policy in each importer.
- [ ] Migrate Squid to the shared candidate/report shape.
- [ ] Remove evidence-layer correction after all products prove that blockers imply no final config.
- [ ] Preserve report JSON ordering and shapes.

### P1.2 Forward-proxy destination capability

**Status:** Complete. `ApprovedDestination` now proves that resolution and policy authorization
succeeded: external callers can inspect its destination and approved socket addresses but cannot
construct or mutate the capability.

**Evidence:**

- Sealed fields and read-only accessors: `crates/oxiroute-forward-proxy/src/policy.rs:58-97`
- Authorized construction: `crates/oxiroute-forward-proxy/src/decision.rs:173-232`
- Trusted use: `crates/oxiroute-server/src/forward_proxy.rs:1397-1474`

**Plan:**

- [x] Make fields private, retain crate-private construction, and expose read-only accessors.
- [x] Review `ForwardDecision` and `TunnelDecision`; their public capability fields remain safe
  because `ApprovedDestination` itself is sealed and read-only.
- [x] Add a Rustdoc compile-fail proof that external code cannot construct an approved destination
  directly.

### P1.3 ACME job lifecycle

**Status:** Complete. `JobState` remains JSON-compatible for historical recovery, while new records
are emitted only through `StateStore`-owned legal lifecycle transitions with private, read-only state.

**Evidence:**

- Lifecycle owner and compatibility tests: `crates/oxiroute-acme/src/state.rs`
- Managed issuance and administrative action transitions: `crates/oxiroute-server/src/tls/acme.rs`

**Plan:**

- [x] Make lifecycle fields private and expose read-only getters.
- [x] Replace unchecked writes with store-owned create, start, challenge wait, finalization, success,
  failure, cancellation, and pause transitions.
- [x] Keep recovery deserialization capable of reading every persisted historical status and weak
  record without changing Serde keys, status spellings, or correlation defaults.
- [x] Enforce immutable creation time, monotonic updates, bounded attempts, terminal states, and
  status-dependent outcome, revision, and next-action fields for every newly emitted record.
- [x] Cover the transition matrix, emitted invariants, historical JSON recovery, redaction, and
  representative server final states.

### P1.4 Exact cache insertion deltas

**Status:** Complete. Memory insertion now returns deterministic exact removals internally while the
public store result remains unchanged. Live persistence and startup recovery reconcile only those
keys, preserve independent disk admission, and fail closed if reconciliation fails after memory
publication.

**Finding:** The memory cache returns only an eviction count. The disk cache compensates by predicting
victims, inserting, snapshotting every resident key, and deleting differences.

**Evidence:**

- `crates/oxiroute-cache/src/cache.rs:68-89,860-878,1018-1059`
- `crates/oxiroute-cache/src/disk.rs:126-157,510-600`
- Exact delta and reconciliation implementation: `crates/oxiroute-cache/src/cache.rs`,
  `crates/oxiroute-cache/src/disk.rs`
- Characterization and private fault coverage: `crates/oxiroute-cache/tests/cache_core.rs`,
  `crates/oxiroute-cache/tests/disk_cache.rs`, `crates/oxiroute-cache/src/disk.rs`

**Plan:**

- [x] Add an internal `InsertionDelta` containing the inserted key and exact removed keys while
  preserving public `StoreOutcome` if required.
- [x] Make disk reconciliation consume the delta instead of scanning all resident keys.
- [x] Fault-test replacement, conflicting `Vary`, eviction pressure, generation loss, every durable
  phase, rollback, and restart recovery.

### P1.5 Canonical lexical and HTTP-method ownership

**Status:** Complete. Neutral DNS/path primitives are shared with importers, and route, cache, and
forward validation now use one bounded HTTP-token normalizer. Plain Serde preserves authored method
strings; `validate_config` uppercases accepted tokens, route/cache retain sorting, forward access
retains declaration order, and render validates a clone without mutating its caller.

**Finding:** DNS-label and safe-path rules have import-side copies, and route, cache, and forward
method validators have drifted grammar and normalization behavior.

**Evidence:**

- Canonical lexical owner: `crates/oxiroute-config/src/lexical.rs:48-87,152-364,469-473`
- Import copies: `crates/oxiroute-import/src/canonical.rs:16-75` and
  `haproxy/resolver.rs:3731-3764`
- Method validators: `http_validation.rs:264-293`, `cache_validation.rs:271-292`, and
  `forward_validation.rs:316-340`

**Plan:**

- [x] Expose narrow, neutral lexical primitives from `oxiroute-config` and wrap their errors with
  product-specific diagnostics.
- [x] Add one HTTP-token parser/normalizer; keep list bounds, deduplication, sorting, and GET/HEAD
  restrictions at their current owners.
- [x] Normalize authored forward-proxy methods to uppercase during validation while preserving access
  rule method order.

### P1.6 RTMP media parser and snapshot invariants

**Finding:** HLS and DASH duplicate AVC/AAC parsing with incompatible malformed-input acceptance, and
publisher, relay, and auto-push paths calculate media snapshots differently.

**Evidence:**

- Codec parsing: `crates/oxiroute-rtmp/src/media_segmenter.rs:1012-1076,1314-1321` and
  `dash_segmenter.rs:991-1123`
- Snapshots: `session_publish.rs:106-161`, `relay.rs:1094-1126`, and
  `auto_push.rs:825-880`

**Plan:**

- [x] Build one malformed-input corpus and differential tests before changing acceptance.
- [x] Extract a structural FLV AVC/AAC parser that returns canonical records, then apply explicit HLS
  and DASH capability policies.
- [x] Add one `MediaSnapshotAccumulator` and prove identical traces yield identical observations on
  all three publication paths.

### P1.7 Supervision replacement ownership

**Status:** Complete. `ReplacementSupervisor` owns the authoritative replacement role and lifecycle
state and derives `ReplacementPhase` from it. The master retains boot, shutdown, failure, process,
and reaping state while one action driver executes the pure machine's ordered side effects.

**Finding:** `ReplacementSupervisor` is authoritative, while master `Stage` mirrors candidate adoption,
quiescing, activation, rollback, drain, and termination transitions with repeated action assertions.

**Evidence:**

- `crates/oxiroute-supervision/src/replacement.rs:21-366`
- `crates/oxiroute-supervisor-master/src/master.rs:362-414,1106-1534`
- Derived authoritative phase and bounded model traces:
  `crates/oxiroute-supervision/src/replacement.rs`,
  `crates/oxiroute-supervision/tests/replacement.rs`
- Ordered action driver and master lifecycle coverage:
  `crates/oxiroute-supervisor-master/src/master.rs`,
  `crates/oxiroute-supervisor-master/tests/master.rs`

**Plan:**

- [x] Add model-based event traces against the pure state machine.
- [x] Keep master-only boot, shutdown, failure, process ownership, and reaping state.
- [x] Derive replacement phase from the pure machine and drive its ordered actions through one master
  action executor.
- [x] Do not merge lifecycle enums directly; observations and ownership states have different roles.

## Phase 2: Consolidate Shared Behavior

### P1.8 HTTP policy evaluation

**Finding:** Request mutation, response mutation, redirect expansion, and cache orchestration are
implemented separately across Pingora reverse HTTP, H3 reverse HTTP, and forward HTTP.

**Evidence:**

- Request mutation: `http_proxy.rs:2595-2677`; `http3.rs:1735-1811,2569-2647`
- Response policy: `http_proxy.rs:2824-2982`
- Redirect expansion: `http_proxy.rs:2482-2529`; `http3.rs:2485-2525`
- Reverse cache: `http_proxy.rs:1062-1253,1374-1475`
- Forward cache: `forward_proxy.rs:670-934,1002-1026,1814-1942`

**Plan:**

- [ ] Define protocol-neutral request and response policy decisions.
- [ ] Keep one small header adapter per `ResponseHeader`/`HeaderMap` representation.
- [ ] Centralize redirect template context and host/scheme normalization.
- [ ] Introduce a shared `CacheTransaction` for lookup, collapsed-fill leadership, revalidation, stale
  eligibility, completion, cancellation, and admission; leave body I/O in protocol adapters.
- [ ] Add table-driven parity and leader/follower cancellation tests before migration.

### P1.9 Bearer-token file security

**Status:** Complete. Route access now composes the shared secure bearer-token loader, digest, and
single-header cardinality logic after the loader adopted the route path's full stable-file timestamp
and identity checks.

**Finding:** `http_action.rs` repeats bounds, secure open, stable-file inspection, hashing,
duplicate-header rejection, and constant-time comparison already represented by `secure_bearer.rs`.

**Evidence:**

- Existing primitive: `crates/oxiroute-server/src/secure_bearer.rs:8-93`
- Route copy: `crates/oxiroute-server/src/http_action.rs:47-49,977-981,1396-1465,2152-2161`

**Plan:**

- [x] Strengthen `SecureBearerToken::load` with any stricter timestamp/stability guarantees currently
  supplied by `same_file_snapshot`.
- [x] Make route access compose `SecureBearerToken` and the shared single-header parser.
- [x] Preserve custom header/challenge behavior and constant-time comparison.

### P1.10 TLS publication, watcher, and ACME workflows

**Finding:** Certbot and direct-file TLS paths duplicate bounded compare-and-swap publication and
nearly copy the watcher/debounce/supervision engine. ACME protocol code also duplicates polling,
challenge parsing, and bad-nonce retry.

**Evidence:**

- Publication: `tls/certbot_reconcile.rs:198-348`; `tls/file_reconcile.rs:169-258`
- Watchers: `tls/certbot_watcher.rs:22-566`; `tls/file_watcher.rs:24-527`
- ACME polling: `crates/oxiroute-acme/src/protocol.rs:920-1049`
- Challenge parsing/responding: `protocol.rs:553-577,854-897,1620-1692`
- JWS retry: `protocol.rs:1140-1175,1227-1271`

**Plan:**

- [ ] Move the publication transaction beside `ActiveCertificateGeneration`; keep candidate loading and
  outcome classification source-specific.
- [ ] Extract only watcher notify/debounce/reconciliation/supervision mechanics; retain each source's
  path and filesystem semantics.
- [ ] Add a private ACME polling driver that alone owns deadlines, attempts, cancellation,
  `Retry-After`, jitter, sleep, and backoff.
- [ ] Add common challenge endpoint/value parsing and a bad-nonce request driver; keep nested key-change
  JWS construction distinct.

### P1.11 Native-source snapshot kernel

**Finding:** Five import loaders duplicate file/byte budgets, stable bounded reads, source IDs, first
include-stack retention, final rereads, fingerprint comparison, and read-failure translation.

**Evidence:**

- nginx: `crates/oxiroute-import/src/nginx/loader.rs:243-385,731-881`
- Apache: `apache/loader.rs:196-333,643-773`
- Squid: `squid/loader.rs:179-312,624-708`
- Varnish: `varnish/loader.rs:428-560,815-897`
- HAProxy: `haproxy/source_roots.rs:172-414`

**Plan:**

- [ ] Add an internal source catalog/budget in `source.rs` that owns identity, bounds, stable snapshots,
  and rerechecks.
- [ ] Keep include syntax, path bases, glob dialect, ordering, cycle policy, edge statuses, and
  diagnostics product-specific.
- [ ] Do not create a generic include loader.

### P2.1 Import provenance and evidence builders

**Finding:** Product lowerers hand-roll canonical provenance merging, sometimes append duplicates, and
four evidence paths serialize the same source graph separately.

**Evidence:**

- Provenance: `nginx/lower/report.rs:613-631`, `nginx/stream_lower.rs:501-505`,
  `haproxy/lower/provenance.rs:72-85`, `apache/lower.rs:888-901`,
  `squid/importer.rs:561-584`, `varnish/lower.rs:1689-1694`
- Evidence: `crates/oxiroute-import/src/evidence.rs:707-1018`

**Plan:**

- [ ] Add a stable-order indexed `CanonicalProvenanceLedger` with caller-supplied origin identity and
  product-specific empty-origin policy.
- [ ] Add one source-graph evidence builder with small edge/status adapters per product.
- [ ] Preserve serialized ordering and failed-empty edge behavior.

### P2.2 Cache-store common fields

**Finding:** Memory and disk cache variants repeat operational-limit fields, validation extraction,
rendering, and importer defaults.

**Evidence:** `model.rs:510-556`, `cache_validation.rs:83-156`, `render.rs:484-570`, and
`varnish/lower.rs:47-56,1794-1823`.

**Plan:**

- [ ] Preserve the flat serialized enum.
- [ ] Add common views/accessors and canonical memory/disk constructors.
- [ ] Do not introduce a flattened nested serde object under `deny_unknown_fields`.

### P2.3 RTMP segment, bootstrap, and admission workflows

**Finding:** HLS/DASH repeat segment timing; recorder, auto-push, and relay repeat bootstrap cache
classification with semantic drift; publish/playback repeat role admission and release compensation.

**Evidence:**

- Segment timing: `media_segmenter.rs:50-63,470-603`; `dash_segmenter.rs:30-41,137-334`
- Bootstrap: `recording_runtime.rs:119-188`; `auto_push.rs:455-488`; `relay.rs:1236-1275`
- Admission/release: `session_publish.rs:175-206,265-392`; `session_playback.rs:98-265`;
  `session.rs:193-215,678-700`

**Plan:**

- [ ] Share immutable `SegmentWindowConfig` and elapsed/cut calculations, not complete segmenter state
  machines.
- [ ] Define ordering, multi-codec retention, fallback-audio, and queue-fit policies before sharing a
  bootstrap cache core.
- [ ] Add an admission transaction that owns a role until protocol acceptance and map insertion commit.
- [ ] Make explicit close and `Drop` call one idempotent cleanup primitive while preserving explicit
  error reporting.

### P2.4 Forward tunnel coordination

**Finding:** H1, H2, and H3 tunnel paths duplicate deadline, directional progress, half-close, limit,
and outcome coordination. The H2 path repeats its own timeout/upstream-read branch.

**Evidence:** `crates/oxiroute-forward-proxy/src/tunnel.rs:236-595,826-897`.

**Plan:**

- [ ] Extract the duplicated H2 branch first.
- [ ] Introduce a shared budget/deadline/outcome coordinator around protocol-specific pumps.
- [ ] Keep H2/H3 flow control and reset behavior in protocol adapters.
- [ ] Test asymmetric/simultaneous EOF, every limit, timeout races, reset, blocked flow control, and
  exact byte accounting.

### P2.5 Supervision wire primitives

**Finding:** Control/status codecs duplicate bounded slice-consumption code; `Frame` and
`AuthenticatedFrame` duplicate storage/accessors; launcher metadata validates one wire contract twice.

**Evidence:**

- Codecs: `oxiroute-supervisor-master/src/protocol.rs:541-649` and `status.rs:511-650`
- Frames: `oxiroute-supervision-unix/src/transport.rs:154-198` and
  `oxiroute-supervisor-process/src/lib.rs:712-756`
- Metadata: `oxiroute-supervisor-process/src/lib.rs:147-218` and `launcher.rs:73-135`

**Plan:**

- [ ] Add bounded wire reader/writer primitives with protocol-specific error mapping.
- [ ] Keep `AuthenticatedFrame` as a proof wrapper around a validated frame; do not alias away the
  authentication boundary.
- [ ] Introduce one `WorkerMetadata` value used by parent encoding and launcher decoding.

### P2.6 UI API and form reuse

**Finding:** API endpoint ownership and decoding are mixed in one file; RTMP callbacks/defaults and
cache forms are duplicated; workspaces repeat presentation helpers.

**Evidence:**

- API transport/decoding: `ui/src/api.ts:1726-1832,1992-2003,2583-2597`
- RTMP callbacks/defaults: `RtmpServiceEditor.vue:70-114,409-452,631-656` and
  `useConfigurationNavigation.ts:259-289`
- Repeated `formatTime`, `shortRevision`, and `errorMessage`: Events, Certificates, Operations,
  Provenance, Audit, and dashboard components.

**Plan:**

- [ ] Make transport return `unknown`; require one decoder per endpoint response contract.
- [ ] Define monitoring endpoint classification once.
- [ ] Add `RtmpCallbackEditor` and one `defaultRtmpApplication` owner.
- [ ] Move shared presentation formatting to `formatters.ts` and add one narrow API-error presenter.

### P2.7 Tooling manifests as sources of truth

**Finding:** Release archive policy, benchmark defaults/implementation sets, and fuzz target limits
have multiple independently maintained definitions.

**Evidence:**

- Release: `scripts/create-release-archive.sh:20-29` and
  `scripts/verify-release-archive.sh:57-190`
- Benchmark: `benchmarks/scripts/preflight.sh:8-29`, `run.sh:26-62`, `environment.sh:8-13`, and
  `benchmarks/lanes.json:3-8`
- Fuzz: `fuzz/targets.sh:3-16`, `fuzz/fuzz_targets/*.rs`, `scripts/verify-fuzz.sh`, and
  `fuzz/campaign.sh:183-203`

**Plan:**

- [ ] Define one declarative release-file policy consumed by archive creation and verification.
- [ ] Make `lanes.json` authoritative and add one benchmark settings loader for defaults/validation.
- [ ] Define one fuzz-target manifest, verify every Rust target limit against it, and use one command
  array for evidence and execution.

## Phase 3: Encapsulate Lifecycles and Split Central Files

File length alone is not a defect. The splits below are justified by distinct ownership boundaries.
Existing facades and public re-exports should remain until callers have migrated.

### Server/runtime split map

| Current file or type | Proposed modules/boundaries | Order and constraints |
| --- | --- | --- |
| `crates/oxiroute-server/src/routing.rs` (4,611 lines) | Move inline tests (`2576-4610`) first; reassess pool endpoint, selection, and health internals after shared selection/observability policy work. | Do not split the production selection contract merely to meet a size target. |
| `crates/oxiroute-server/src/http_proxy.rs` (4,095) | Static adapter, cache adapter, request/response policy adapters, and core Pingora proxy service. | Extract shared decisions first, then move adapters. |
| `crates/oxiroute-server/src/forward_proxy.rs` (3,756) | HTTP/1 service, H2/H3 adapters, cache adapter, destination connection. | Follow cache transaction and tunnel coordination. |
| `crates/oxiroute-server/src/tls/acme.rs` (3,433) | `AcmeOrderEngine`, `DnsChallengeJournal`, `AcmeJobController`, managed-certificate publisher; keep `AcmeManagedReconciler` facade. | Critical lifecycle; follow TLS publication and `JobRecord` work. |
| `crates/oxiroute-server/src/main.rs` (3,289) | Listener/admission adapters, RTMP ingest/access logging, generation-process supervisor, startup/serve composition. | Preserve `main`/`run` as thin composition roots. |
| `crates/oxiroute-server/src/monitoring.rs` (3,177) | Process sampling, listener/pool metrics, certificate health, transport events; keep `RuntimeMetrics` facade. | Preserve snapshot and metric semantics. |
| `crates/oxiroute-server/src/http3.rs` (3,065) | H3 service, reverse dispatch, static adapter, header sanitation/response writer. | Follow P0 static and upstream decisions. |
| `crates/oxiroute-server/src/generation.rs` (2,785) | Shared preparation components, reservation acquisition/adoption, lifecycle manager, `RtmpGenerationRuntime`, cleanup registry. | Consolidate duplicated prepare paths before moving. |
| `crates/oxiroute-server/src/cli.rs` (2,478) | `command`, `offline`, `management`, `output`, and raw `client`. | Preserve Clap schema, redaction, output, framing, and exit categories. |
| `crates/oxiroute-server/src/http_action.rs` (2,456) | Action plan types, static-file plan, access/security plan. | Follow bearer and static-decision extraction. |
| `crates/oxiroute-server/src/service_plan.rs` (2,353) | HTTP, forward, L4, and RTMP compilers behind `runtime_plan`; shared RTMP relay-target and media-store acquisition helpers. | `runtime_plan` is CRITICAL; keep one facade and parity tests. |
| `crates/oxiroute-server/src/operational_event.rs` (1,967) | Durable `audit_store`, in-memory `operational_events`, and explicit event-to-audit bridge. | Move-only first; audit durability is a separate risk. |
| `crates/oxiroute-server/src/rtmp_api/management.rs` (1,586) | Generation, upstream, TLS, process, and audit domain controllers. | Keep one route facade and authorization/audit context. |
| `crates/oxiroute-server/src/rtmp_api/service.rs` (1,223) | Pure route matcher, ordinary response adapter, SSE body, VOD/media body, Pingora transport. | Preserve method restrictions and streaming behavior. |
| `crates/oxiroute-server/src/prometheus.rs` / `crates/oxiroute-server/src/stats.rs` | Metric-family appenders; stats routing, HTML, and status DTO modules. | Preserve names, labels, readiness, and JSON exactly. |

Additional server lifecycle work:

- [ ] Consolidate duplicated generation preparation at `generation.rs:81-212` into reservation
  acquisition plus one `prepare_generation_components` path.
- [ ] Create an explicit `RtmpGenerationRuntime` instead of storing RTMP resources in the generic
  generation lifecycle (`generation.rs:220-566`).
- [ ] Add small admission-layer composition rather than manually rebuilding generation, process,
  listener, and service limits in `main.rs:292-707`.
- [ ] Split `ManagementState` by domain rather than exposing one object spanning listeners, pools,
  generations, TLS/ACME, process lifecycle, audit, and events.

### Configuration/import split map

| Current file | Proposed modules/boundaries | Constraints |
| --- | --- | --- |
| `crates/oxiroute-config/src/model.rs` (3,263) | Core/top-level, HTTP (`869-1595`), RTMP (`1596-2383`), forward (`2384-2776`), errors (`2822-3263`). | Preserve public re-exports and serde shape. |
| `crates/oxiroute-config/src/validation.rs` (3,915) | TLS/cert (`235-737`), management/listeners (`738-1502`), RTMP (`1503-3214`), upstream/L4 (`3215-3915`). | Keep one top-level orchestration function. |
| `crates/oxiroute-config/src/render.rs` (2,749) | Cache/common, RTMP (`572-1125`), upstream (`1126-1334`), HTTP (`1335-2120`), forward (`2121-2406`), writer (`2442-2651`). | Golden round-trip output must remain byte-identical. |
| `crates/oxiroute-import/src/haproxy/resolver.rs` (4,008) | Effective model (`43-692`), resolution engine (`778-2833`), directive/bind parsing (`2913-3321`), certificate loading (`3323-3764`). | Extract source snapshot and lexical owners first. |
| `crates/oxiroute-import/src/nginx/lower/listener.rs` (2,799) | Bind policy (`297-1068`), routes/static (`1075-1611`), proxy lowering (`1612-2366`), static auxiliaries (`2367-2660`). | Preserve lowerer facade and diagnostics. |
| `crates/oxiroute-import/src/varnish/parser.rs` / `crates/oxiroute-import/src/varnish/semantic.rs` | AST vs parser; semantic model (`25-580`) vs analyzer (`602-1956`). | Do not unify nginx/Varnish ASTs or parsers. |
| `crates/oxiroute-import/src/apache/semantic.rs` | Semantic model (`21-160`) and resolver (`163-1565`). | Move-only after shared source kernel. |
| `crates/oxiroute-config-source/src/uci.rs` (1,153) | AST/parser/renderer (`16-248`), native sections (`249-571`), JSON records (`572-1046`), tokenizer (`1047-1153`). | Do not create one generic source-adapter trait. |
| `crates/oxiroute-config-source/src/resolver.rs` | Product import workflows and small dependency adapters. | Share only a source-dependency view where path semantics align. |

Separate design project, not an incidental cleanup:

- [ ] Evaluate `ConfigDraft` and `ValidatedConfig` proof types so runtime planning cannot accept
  unvalidated mutable configuration. The current mutable `Config`, mutating validator, render-time
  clone/revalidation, and `CanonicalDraft` duplication justify the design, but `validate_config` has
  CRITICAL impact and must not be changed during module splitting.

### RTMP/cache/ACME/forward/supervision split map

| Current file | Proposed modules/boundaries | Constraints |
| --- | --- | --- |
| `crates/oxiroute-rtmp/src/recording_runtime.rs` (2,495) | Controller/bootstrap, restart policy, reaper queue, cleanup coordinator. | Follow bootstrap/admission ownership. |
| `crates/oxiroute-rtmp/src/recording_worker.rs` (2,336) | Handle/queue, segment engine, finalization, notifications/status. | Preserve finalization and queue-release ordering. |
| `crates/oxiroute-rtmp/src/recording_store.rs` (1,922) | Pinned root, quota, publication transaction, finalizer, FLV inspection. | Wrap raw atomic commit states in typed `CommitPhase`. |
| `crates/oxiroute-rtmp/src/relay.rs` (1,913) | Pull, push, shared client session, bootstrap queue, executor. | Share bounded executors only after preserving result types. |
| `crates/oxiroute-rtmp/src/auto_push.rs` (1,815) | Discovery, authentication, wire protocol, peer queue, remote publication. | Preserve proof and queue boundaries. |
| `crates/oxiroute-rtmp/src/media_segmenter.rs` / `crates/oxiroute-rtmp/src/dash_segmenter.rs` | Shared parser/timeline; separate TS and fMP4 builders/state machines. | Do not unify complete segmenters. |
| `crates/oxiroute-cache/src/disk.rs` (1,811) | Secure root, transaction, record codec (`1172-1582`), recovery/index. | Codec is the safest first move; preserve disk format. |
| `crates/oxiroute-acme/src/protocol.rs` (2,625) | Models, client operations, JWS/nonce, polling, response parsing, CSR. | Keep public client facade. |
| `crates/oxiroute-forward-proxy/src/tunnel.rs` (1,124) | Coordinator, byte-stream adapter, H2 adapter, H3 adapter. | Follow shared coordinator tests. |
| `crates/oxiroute-supervisor-master/src/master.rs` (1,904) | Transition driver, request dispatch, observation/status, termination. | Follow model-based replacement work. |
| `crates/oxiroute-supervisor-process/src/lib.rs` (1,348) | Metadata, handshake, authenticated channel, process owner, reaper. | Preserve launcher fail-closed behavior. |

Encapsulation tasks within these splits:

- [ ] Review public RTMP recording re-exports in `rtmp/src/lib.rs:74-83`; keep policy/controller APIs
  public and make low-level leases, commits, files, and workers crate-private only after a semver review.
- [ ] Keep secure pinned-directory behavior separate across disk cache, recording store, and ACME state
  until their symlink, locking, recovery, and durability threat models are documented. Do not create a
  shared filesystem crate merely because the code looks similar.
- [ ] Preserve `AuthenticatedFrame` as evidence of authentication, not a type alias for raw transport.

### UI split map

| Current file | Proposed modules/boundaries | Constraints |
| --- | --- | --- |
| `ui/src/api.ts` (2,695) | `transport`/SSE, monitoring, management, TLS, configuration, import-report; stable barrel exports. | Split decoders and transport before endpoint moves; graph impact is CRITICAL. |
| `ui/src/config.ts` (2,255) | `config/model.ts` (`11-993`), `config/decode.ts` (`995-1616`), `config/fieldRegistry.ts` (`1618-2255`); stable barrel. | Keep every imported name and diagnostic path stable. |
| `ui/src/configuration/RtmpServiceEditor.vue` (900) | Callback editor, application editor, HLS/DASH editor, exec-profile editor. | Establish default owner first; preserve nested update semantics. |
| `ui/src/ConfigurationWorkspace.vue` (1,738) | Keep as workspace facade; move only responsibilities not already owned by lifecycle/navigation composables. | Avoid a generic form framework. Reassess after cache and RTMP subeditors. |
| `ui/src/App.vue` (2,001) | Keep navigation/composition root; consider a resource-loader primitive only after latest-task ownership is established. | Different resources intentionally have different stale/error fallbacks. |

## Phase 4: Mechanical Reuse and Test Organization

These are low-risk after the owning abstractions above are settled.

### P3.1 Small exact production duplication

- [ ] Correlation IDs: make `operational_event.rs:45-78` use `logging.rs:55-67`.
- [ ] HTML escaping: share one server-owned writer for `http_action.rs:2137-2149` and
  `stats.rs:865-876`.
- [ ] Generation health DTO: replace duplicate JSON construction in `stats.rs:286-300` and
  `rtmp_api/observability.rs:110-122`.
- [ ] RTMP relay wire mappings: share `rtmp_api/streams.rs:210-239` and
  `rtmp_api/observability.rs:449-479`; keep operational event codes distinct.
- [ ] Four-segment management routes: share a prefix-parameterized parser for
  `rtmp_api/media.rs:1-24` and `rtmp_api/vod.rs:1-24`.
- [ ] Shutdown watch loop: share `proxy_protocol.rs:641-650` and `udp_relay.rs:901-909` at the
  shutdown abstraction owner.
- [ ] OWS trimming: share cache-owned logic from `cache/src/key.rs:313-327` and
  `cache/src/policy.rs:789-803`.
- [ ] RTMP wall-clock milliseconds: centralize `relay.rs:1128-1134`, `auto_push.rs:1614-1620`,
  `media_segmenter.rs:927-933`, and `dash_segmenter.rs:1191-1197` behind an injectable clock where
  behavior is tested.
- [ ] RTMP pull/push bounded executors: consolidate `relay.rs:809-847,1277-1316`.
- [ ] Publisher/playback identity accessors: move shared identity behavior from
  `session_publish.rs:94-104` and `session_playback.rs:49-59` to the identity owner.
- [ ] Stream delegation wrappers: share the mechanical forwarding in `client.rs:317-338`,
  `vod.rs:322-343`, and `callback.rs:372-393` without obscuring distinct stream types.
- [ ] Validated-envelope deserialization: share bounded envelope mechanics in
  `supervision/src/protocol.rs:112-139` and `snapshot.rs:78-101`.

### P3.2 Test-support reuse and suite splits

- [ ] Split `crates/oxiroute-server/tests/support/mod.rs` into certificates (`89-482`), proxy harness
  (`483-756`), TLS clients (`757-1234`), H1 origins (`1235-1523`), H2 origins/clients (`1524-1983`),
  and counters/loading (`1984-2065`), re-exported from `mod.rs`.
- [ ] Add `tests/support/http.rs` helpers for response heads, chunked streaming, authenticated SSE,
  active revision, and bounded revision waits. Replace copies in `process_active_drain.rs`,
  `process_rtmp_runtime.rs`, `process_udp_drain.rs`, `reverse_http3.rs`, and `rtmp_api.rs`.
- [ ] Add parameterized `tests/support/h3.rs` endpoint, TLS, and retry setup for
  `forward_proxy_h3.rs`, `reverse_http3.rs`, `http3_client_auth.rs`, and `supervised.rs`.
- [ ] Move large inline test modules out of `routing.rs`, `udp_relay.rs`, TLS ACME, generation,
  catalog, and recording modules where private visibility can be preserved with child test modules.
- [ ] Split integration suites by behavior after support extraction:
  `http_proxy_routing.rs` (routing/cache/retries/policies/logging), `supervised.rs`
  (core/UDP/H3/replacement), `rtmp_api.rs` (runtime/auth-events/config-import), and
  `config/tests/lua_config.rs` (statistics/core/binds/TLS/upstream/routes/sandbox).
- [ ] Add `ui/src/test/async.ts` for `deferred`, `ui/src/test/sse.ts` for controllable streams, and
  focused wrapper-query helpers.
- [ ] Split `ui/tests/browser/dashboard.spec.ts` by workspace while retaining common browser support.

## Recommended Delivery Order

Keep each numbered slice independently reviewable and reversible.

1. **Characterization:** Add P0 protocol/UI tests, importer all-product invariants, media differential
   tests, cache transaction fault tests, and supervision model traces.
2. **P0 fixes:** UI replace-in-flight ownership, forward-cache diagnostic path, HTTP static parity, and
   the explicit H3 transport decision.
3. **Proof types:** `ApprovedDestination`, candidate finalization, ACME `JobRecord`, cache
   `InsertionDelta`, and RTMP admission/cleanup guards.
4. **Leaf shared policy:** lexical/method parsing, provenance ledger, evidence builder, media parser and
   snapshot accumulator, correlation/HTML/DTO/route helpers.
5. **HTTP/cache/tunnel:** protocol-neutral mutation/static/cache decisions and tunnel coordinator.
6. **TLS/ACME:** publication transaction, watcher engine, polling/JWS/challenge helpers, then split the
   managed reconciler facade.
7. **Generation/control plane:** common preparation, generation resources, service compilers,
   admission composition, management domains, observability, audit/event bridge, and CLI.
8. **Supervision:** shared wire primitives and metadata, then replacement driver and master split.
9. **UI forms/contracts:** API/config barrels, cache/RTMP editors, formatters, then reassess large
   workspace roots.
10. **Tooling/tests/moves:** release/benchmark/fuzz manifests, test support, integration suite splits,
    and behavior-neutral module moves.
11. **Separate proposal:** draft/validated configuration lifecycle and any public RTMP visibility
    reduction requiring semver decisions.

Parallel work is safe only where ownership does not overlap. Good independent lanes are:

- UI latest-task and diagnostic-path corrections.
- Import provenance/evidence after candidate API shape is agreed.
- RTMP media parser/snapshot work.
- Release, benchmark, and fuzz manifests.
- Test-support extraction that does not move production code under active refactor.

## Verification Gates

Every slice must run the smallest focused tests first, then the relevant crate/application gates.

### Contract-specific gates

- HTTP policy/static work: H1/H2/H3 parity, malformed headers, cancellation, retries, cache
  leader/follower races, and metric parity.
- TLS/ACME: fake-CA protocol tests, stable-file replacement, CAS conflicts, watcher storms/debounce,
  restart cleanup, pause/cancel/revoke/delete, and publication rollback.
- Import/config: all-product deterministic reports, provenance order, snapshot rerechecks, canonical
  round trips, Lua/KDL rendering, and blocker/finalization invariants.
- RTMP: malformed media corpus, differential segmenter tests, snapshot parity, queue bounds,
  admission compensation, recording finalization, and relay/bootstrap codec changes.
- Cache: replacement, `Vary`, eviction pressure, disk fault injection, restart recovery, and exact
  insertion/removal deltas.
- Supervision: model event traces, codec truncation at every field, descriptor ownership, stale
  acknowledgements, timeout boundaries, rollback, reaping, and authenticated-frame failures.
- UI: typecheck, endpoint decoder rejection, token replacement races, diagnostic focus, form
  enable/disable round trips, component tests, browser workspace tests, and production build.
- Tooling: malicious/omitted archive cases, benchmark preflight/run parity, and fuzz manifest vs target
  limits and logged/executed command equality.

### Repository gates

Run builds with at most four workers:

```sh
cargo fmt --all --check
nice cargo clippy --workspace --all-targets --jobs 4 -- -D warnings
nice cargo test --workspace --jobs 4
nice pnpm --dir ui test -- --maxWorkers=4
nice pnpm --dir ui build
```

Also run `git diff --check`, GitNexus `detect_changes`, and GitNexus cycle checks. Move-only changes
must have no changed serialized fixtures, API shapes, metric snapshots, CLI snapshots, or disk-format
fixtures.

## Explicit Non-Goals

- Do not share native-product lexers, parsers, ASTs, glob dialects, or one five-product lowering trait.
- Do not fully unify HLS and DASH segmenter state machines; share structural parsing and timing only.
- Do not merge all supervision lifecycle enums; authoritative state, observed state, and process
  ownership are different concepts.
- Do not replace master pending state with the generic correlation table until time and cardinality
  contracts align.
- Do not generate Lua from generic serde values; field-specific omitted `nil` versus explicit `null`
  behavior must remain intact.
- Do not flatten cache-store serde models merely to remove repeated fields.
- Do not extract a generic secure-directory crate before each subsystem's threat model is documented.
- Do not split cohesive modules such as `server/src/topology.rs` or production `udp_relay.rs` solely by
  line count. Moving their tests is sufficient unless responsibilities later diverge.
- Do not add compatibility wrappers unless a shipped external or persisted contract requires one.

## Completion Criteria

The plan is complete when:

- Every P0 item has a regression test and no protocol/user-visible drift remains.
- Security and lifecycle proof types cannot be forged or moved to illegal states through public APIs.
- Shared HTTP, TLS, import, media, cache, and supervision workflows each have one policy owner with
  thin protocol/product adapters.
- Central files act as composition facades rather than owning unrelated policy, I/O, state machines,
  presentation, and tests together.
- UI transport, endpoint decoding, configuration model, and diagnostic registry have distinct owners
  with stable barrel exports.
- Release, benchmark, and fuzz behavior each derives from one declarative source of truth.
- All focused and repository gates pass, GitNexus reports only expected process impact, and no new
  import cycle is introduced.
