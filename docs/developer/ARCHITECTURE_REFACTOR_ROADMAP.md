# Architecture Refactor Roadmap

Status: authorized and in progress since 2026-08-11. Phase 0 establishes the contract baseline and
decisions; later production-code slices remain subject to the delivery rules and per-slice review.

## Intended Outcomes

The next architecture program should make these properties explicit in types and ownership:

1. Unvalidated authored configuration cannot reach rendering, planning, or activation.
2. Pure runtime decisions are distinguishable from acquired resources and started workers.
3. One canonical listener inventory owns identity, descriptor roles, readiness, and reload
   compatibility.
4. Direct and supervised runtimes expose one lifecycle-control contract while retaining different
   authorities.
5. RTMP construction is behind value plans and opaque handles rather than a broad mutable registry.
6. Control-plane JSON is an API projection, not an incidental serialization of runtime state.
7. Rust DTOs, routes, TypeScript types, and runtime validators derive from one checked contract.

The goal is not smaller files or more crates. The goal is to make invalid state and split authority
unrepresentable at the owning boundary.

## Evidence Baseline

The analysis was performed at `ac0eb9c905804dba8ca64c02b534e0685f53d418` after rebuilding
GitNexus with PDG data:

- 931 indexed files, 106,309 nodes, 254,100 edges, 879 communities, and 300 execution flows.
- No directed file-import cycles.
- CDG construction was unavailable for 140 functions with unreachable exits. Those paths have
  UNKNOWN control-dependence risk and require source, CFG, and behavior-test review.
- Source is authoritative where graph extraction misses field access, receiver dispatch, manual HTTP
  routing, or private lifecycle relationships.

The principal graph signals are:

| Boundary | Upstream impact | Risk | Planning consequence |
| --- | ---: | --- | --- |
| `validate_config` | 46 symbols, 5 processes, 8 modules | CRITICAL | Migrate through a proof type; do not rename and repair callers in one change. |
| `runtime_plan` | 71 symbols, 11 processes, 8 modules | CRITICAL | Preserve the validated-only planning entry point while planning stages migrate. |
| `PreparedGeneration::prepare` | 72 symbols, 43 direct dependants, 8 processes | CRITICAL | Consolidate acquisition before moving lifecycle ownership. |
| `RuntimeSnapshot` | 32 symbols, 5 processes, 8 modules | CRITICAL | Project API DTOs from snapshots; do not make snapshots wire contracts. |
| `LiveHub::attach_publisher` | 33 symbols, 2 processes, 10 modules | CRITICAL | Keep publisher admission and fanout transactions cohesive. |
| `Master::poll` | 12 symbols across 5 modules | CRITICAL | Add observations around existing ordering; do not rewrite the master loop. |
| `descriptor_plan` | 13 symbols, 4 direct dependants | HIGH | Make it the seed of one listener inventory. |
| UI `request` | 45 symbols, 29 direct dependants | HIGH | Keep transport stable while endpoint contracts migrate. |

File-import hubs support stable facades rather than more direct imports:

| File | Direct importers | Decision |
| --- | ---: | --- |
| `ui/src/config.ts` | 48 | Keep as a barrel over model, decoder, and registry owners. |
| `ui/src/api.ts` | 25 | Keep as a barrel while generated operation contracts migrate underneath. |
| `crates/oxiroute-server/src/config_coordinator/mod.rs` | 8 | Preserve one storage facade while document and revision types change. |

## Current Ownership Problem

The documented lifecycle is correct but is represented by too many authorities:

```text
source bytes
  -> ConfigDraft
  -> owned validation and normalization
  -> resource-free RuntimePlan decisions
  -> empty GenerationAcquisition owning each completed stage with reverse rollback
  -> provisional preparation transaction owning listeners, metrics, and RTMP preparation
  -> PreparedGeneration containing sole-owned data-plane GenerationResources
  -> GenerationManager candidate/publication state
  -> GenerationProcess thread and readiness state in main.rs
  -> direct or master/worker retirement and drain
```

Specific mechanisms that the current abstractions cannot express cleanly are:

- `RuntimePlan` previously mixed immutable decisions with acquired services, TLS, pools, logs, caches,
  and RTMP stores.
- Validation previously used activation constructors for access logs, persistent caches, and media
  roots, causing temporary workers, registry insertion, and filesystem mutation.
- Acquisition previously relied on aggregate drop rather than explicit stage and preparation
  transactions with reverse-order rollback.
- Publication state is in `GenerationManager`, but worker start, readiness, process handles, and
  retirement are owned in `main.rs`.
- Reservation keys, descriptor slots, and supervised listener manifests independently represent the
  same listener identity.
- `RuntimeMetrics` owns traffic admission and lifecycle state in addition to exporting observations.
- A supervised worker exposes direct-runtime reload, rollback, and shutdown operations even though
  the master owns global process lifecycle.
- The server constructs RTMP hubs, stores, workers, resolvers, and application runtimes rather than
  passing value plans to one RTMP composition root.
- Domain snapshots and handwritten `serde_json::Value` objects double as API response types.
- Rust route recognition, authentication, method policy, audit classification, TypeScript types, and
  runtime decoders have separate authorities.

## Target Shape

The intended configuration and runtime pipeline is:

```text
authored bytes / API JSON / import lowering
                 |
             ConfigDraft
                 |
    compose + normalize + validate exactly once
                 |
          ValidatedConfig
             /          \
 deterministic render   GenerationCompiler
             |                 |
  authored/effective      GenerationBlueprint
  revisions                     |
             \          GenerationPreparer
              \                |
               -> PreparedGeneration
                          |
                  GenerationRuntimeHost
                          |
                  ReadyGeneration handle
                          |
                 lifecycle publication
                          |
                Running -> Retired
```

The intended control-plane pipeline is:

```text
domain/runtime snapshot
        -> API projection DTO
        -> EndpointSpec registry
        -> OpenAPI 3.1 artifact
        -> generated TypeScript types + runtime validators
        -> explicit frontend projection
        -> Vue consumers
```

## Architecture Decisions

These decisions constrain every workstream below:

- Keep `oxiroute-config` as the source-format-independent model and validation leaf.
- Keep source syntax, restricted Lua, native-reference resolution, composition orchestration, and
  deterministic rendering in `oxiroute-config-source`.
- Keep product parsers, semantics, provenance, diagnostics, and report policy in `oxiroute-import`.
- Do not introduce a generic source-adapter or product-importer trait.
- Do not add a server-runtime crate yet. Generation blueprints, manifests, admission, and hosting use
  server-specific Pingora, TLS, RTMP, monitoring, and listener concerns and have no second consumer.
- Keep the complete RTMP runtime in `oxiroute-rtmp`. Its session, fanout, media, recording, relay, and
  auto-push state machines share publisher identity and shutdown semantics too closely for useful
  crate separation.
- Move nginx-RTMP compatibility metadata to `oxiroute-import`; it is import syntax policy, not runtime
  behavior.
- Keep the existing pure supervision, Unix transport, master, and process crate boundaries. Refactor
  server integration instead of recombining their lifecycle models.
- Treat public Rust configuration and RTMP API narrowing as a coordinated `0.5` change. Do not retain
  broad compatibility wrappers unless an external consumer is identified. The `0.4.1` public
  surfaces remain unchanged until that boundary lands as one reviewed release change.
- Preserve management API schema v1 field names and semantics. A v1 response may gain an optional
  field only after every in-repository decoder is shown to tolerate it. Removing or renaming a field,
  changing its type, casing, omission/null behavior, status or error semantics, narrowing an accepted
  request, or adding a value to a closed enum is schema v2. Do not content-negotiate two meanings for
  one v1 route or silently repurpose a v1 field; publish an explicit v2 route/artifact and retain v1
  until its separately documented removal boundary.
- Preserve serialized configuration, canonical rendering, effective hashes, management JSON,
  Prometheus families, CLI output, operational/audit event contracts, descriptor/wire formats, and
  disk formats unless a slice explicitly versions that contract.

## Authorized Phase 0 Baseline

### Coordinated Rust `0.5` Boundary

Configuration proof types and RTMP visibility narrowing are one release boundary, not independent
compatibility events. The configuration side replaces the broad mutable/deserializable `Config`
capability with draft and validated capabilities. The RTMP side retains value policy, content,
session/service, control-handle, snapshot, identifier, and error APIs while making raw composition and
mutable ownership internals crate-private. The reviewed removal candidates are `RtmpRegistry`, raw
publisher/subscriber registrations and leases, `LiveHub`, media and recording stores, recorder workers,
and raw `RtmpServiceRuntime` construction. nginx-RTMP parsing and compatibility metadata move to
`oxiroute-import`; they do not remain duplicated in the runtime crate.

The inventory method is:

1. Use the exact `v0.4.1` release source and the candidate `0.5` source with Rust 1.97.1 and locked
   dependencies.
2. Inventory every public item reachable from `oxiroute-config`, `oxiroute-config-source`,
   `oxiroute-import`, `oxiroute-server`, and `oxiroute-rtmp`, including public modules, re-exports,
   inherent methods, trait implementations, constants, fields, and feature-gated items. The current
   root export groups in `oxiroute-rtmp` are
   auto-push, callbacks, catalog/registry, client policy, DASH/HLS/media, nginx directives, exec,
   FLV/live, recording path/runtime/store/worker, relay, session/control, VOD, and handshake helpers.
   The checked Phase 0 RTMP baseline contains 199 root-reachable exports.
3. Run `scripts/verify-public-api.sh`. The repository-owned mechanism uses Rust 1.97.1 rustdoc JSON
   for the current Rust host with all features and compares each candidate crate with its checked
   textual `2d9c5fe` baseline and classified `0.5` delta. `scripts/verify-rtmp-public-api.sh` defaults
   to that classified gate; use `scripts/verify-rtmp-public-api.sh --baseline-equality` only to prove
   the immutable RTMP Phase 0 snapshot. The inventory
   records functions and inherent methods, fields, traits with
   required-versus-provided associated functions, and non-blanket trait impl targets, trait arguments,
   generic bounds, polarity/safety, and associated bindings, plus constants, variants, re-exports, and
   associated types. It excludes private items, implementation method bodies, documentation, source
   paths/spans, dependency bodies, and other environment-specific noise. Mutation self-tests require
   trait requirement and public impl additions/removals to change the snapshot while those excluded
   inputs remain invariant.
4. For the eventual release diff, pin `cargo-public-api` 0.52.0 (crate checksum
   `80f55cea4022db86641f1ece5098c9df2f13d79624e61a461d5379dc5d4b7511`) and generate both
   `v0.4.1` and candidate `0.5` inventories with the same executable. Do not install an unpinned tool
   or substitute a broad category list for the mechanical snapshot.
5. Classify every removal or signature/visibility change in the `0.5` release notes. An item absent
   from the reviewed list blocks release; broad wrappers are added only for an identified external
   consumer.

### Runtime And Package Default

`oxiroute serve` is mode-selecting on Linux. It starts the supervised master when the fixed packaged
launcher exists and the typed descriptor topology is eligible; otherwise it uses the direct runtime.
The Arch package installs that launcher, so eligible packaged configurations are supervised by
default. Unsupported packaged topologies, unpackaged/development installations without the launcher,
non-Linux builds, and the internal direct-runtime test override use the direct runtime. Documentation
must describe both the package default and these explicit fallbacks rather than calling either mode
the universal `serve` default.

### Supervisor Protocol Decision

The currently shipped protocol is control protocol version 2 with descriptor manifest version 1. It
carries master-to-worker lifecycle commands and worker-to-master status observations, but not
worker-to-master lifecycle requests. Master-owned bidirectional lifecycle administration will be one
coordinated control protocol version 3 change. A v3 endpoint must reject v2 identities and payloads
exactly, and a v2 endpoint must reject v3; no negotiation, dual decoder, downgrade, or mixed-version
master/worker support is planned. Descriptor manifest version 1 remains unchanged unless listener
metadata itself requires a separately reviewed manifest version.

### Authoritative Characterization Gates

| Contract | Authoritative Phase 0 gate |
| --- | --- |
| Canonical rendering and effective revision | Exact KDL rendering tests plus `canonical_minimal_config_bytes_and_effective_revision_are_stable` pin canonical bytes and their SHA-256. |
| Descriptor manifest | Supervisor protocol tests pin manifest version rejection, control prefix/ack bytes, and the byte-exact manifest-v1 payload. Unix transport tests retain typed TCP/Unix/UDP/QUIC validation, ordering, descriptor count, and CLOEXEC ownership. |
| Lifecycle trace | `oxiroute-supervision/tests/replacement.rs` pins every successful ordered action/state, every pre/post-quiesce failure, rollback orderings, and all 57 reachable states. Do not add a duplicate snapshot trace. |
| API enums | Rust operational-event serialization tests and UI `api.contract`, `api.events`, and real-process tests enumerate accepted relay, recorder, monitoring, event-name/outcome, and listener-protocol values and reject malformed/unknown values. |
| Public RTMP API | `scripts/verify-rtmp-public-api.sh` defaults to the classified all-crate candidate gate. `scripts/verify-rtmp-public-api.sh --baseline-equality` checks the complete all-features Rust 1.97.1 rustdoc JSON graph against `docs/developer/fixtures/rtmp-public-api-phase0.snapshot`; pinned `cargo-public-api` 0.52.0 produces the eventual `v0.4.1` to `0.5` release diff. |

## Delivery Rules

1. Characterize the existing contract before changing an owner.
2. Run GitNexus upstream impact for every existing symbol before editing it. Warn and review direct
   callers for HIGH and CRITICAL results.
3. Change interfaces before implementations, then migrate callers, tests, and visibility.
4. Keep proof-type changes, behavior changes, public API removals, and move-only module splits in
   separate changes.
5. Keep old facades only while a concrete migration is active. Remove them in the planned `0.5`
   boundary rather than creating indefinite dual paths.
6. Re-run GitNexus change detection and cycle checks after every slice.
7. Stop a slice if API JSON, canonical rendering, effective revisions, metrics, descriptor fixtures,
   or lifecycle traces change without an approved contract change.

## Workstream A: Canonical Configuration Proof

### Goal

Replace one mutable `Config` state with an owned transition from deserializable draft to immutable,
normalized, validated configuration.

### Target Boundary

`oxiroute-config` should own:

```rust
pub struct ConfigDraft {
    // Exact current serialized fields and serde behavior.
}

pub struct ValidatedConfig(ConfigDraft);

impl ConfigDraft {
    pub fn validate(self) -> Result<ValidatedConfig, ConfigError>;
}

impl ValidatedConfig {
    pub fn as_draft(&self) -> &ConfigDraft;
    pub fn to_draft(&self) -> ConfigDraft;
}
```

`ValidatedConfig` may be cloned and serialized, but it must not implement `Deserialize`, `DerefMut`,
or expose a mutable inner value. Normalization occurs only during the owned validation transition.

### Migration Slices

- [x] Add `ValidatedConfig` around the current model and an owned validation transition; remove the
  temporary `validate_config` compatibility facade at the coordinated `0.5` boundary.
- [x] Add idempotence and failure-atomicity tests: revalidating `validated.to_draft()` is equal, and a
  failed validation cannot expose partially normalized state.
- [x] Make composition consume complete drafts and return one `ValidatedConfig`; do not validate
  fragments independently before namespace composition.
- [x] Migrate planners, topology, listeners, TLS, supervision, rendering, and generation storage to
  `&ValidatedConfig` in bounded caller groups.
- [x] Rename the remaining serde/input type to `ConfigDraft` only after valid-only consumers have
  migrated.
- [x] Make the mutating validator private and remove the old facade at the `0.5` boundary.
- [x] Add external compile-fail proofs that `ConfigDraft` cannot reach runtime planning, rendering,
  TLS preparation, listener reservation, or stable-listener preparation, and that `ValidatedConfig`
  cannot be constructed, deserialized, or mutated externally.

### Import And Source Follow-On

- [x] Replace importer `CanonicalDraft` duplication with `ConfigDraft` and represent candidate state
  as blocked draft or validated configuration, never both.
- [x] Preserve product-specific report types, diagnostics, source graphs, ledgers, and evidence
  policy. Do not create one generic import report or lowerer.
- [x] Make config-source renderers accept only `&ValidatedConfig` and remove render-time validation.
- [x] Move restricted Lua parsing/rendering and `mlua` ownership from `oxiroute-config` to
  `oxiroute-config-source` without changing sandbox or output behavior.
- [x] Keep `oxiroute-config-source -> oxiroute-import -> oxiroute-config` dependency direction and
  prove the graph remains acyclic.

### Document And Revision Follow-On

The server coordinator should distinguish authored and effective identity internally:

```rust
pub struct AuthoredRevision(Sha256Hex);
pub struct EffectiveRevision(Sha256Hex);

pub struct ResolvedConfigDocument {
    pub authored_revision: AuthoredRevision,
    pub effective_revision: EffectiveRevision,
    pub config: ValidatedConfig,
    // Existing format, dependency, composition, and preview data.
}

pub struct PersistableConfigCandidate {
    pub effective_revision: EffectiveRevision,
    pub config: ValidatedConfig,
    // Existing rendered preview and format data.
}
```

- [x] Keep API v1 `diskRevision`, `candidateRevision`, and `activeRevision` names unchanged while
  making authored/effective mixups impossible internally.
- [x] Make coordinator preparation consume a `ConfigDraft` and return a prepared candidate.
- [x] Make save consume that prepared candidate instead of validating and rendering a mutable config
  again.
- [x] Retain a fresh activation preflight after durable save because environmental state can change.
- [ ] Remove duplicate full-preparation preflight only in Phase 3 after blueprint compilation is pure
  and listener/store/file/worker/watcher acquisition is represented by owned preparation states. The
  current full preparation has irreversible side effects, so deduplicating it in the proof migration
  would change failure and cleanup semantics.

### Contracts And Gates

- Preserve the exact serde shape, defaults, enum tags, unknown-field policy, canonical KDL/Lua bytes,
  and effective SHA-256 algorithm.
- Preserve import-report schema version 1, deterministic diagnostics, blockers, provenance, and
  finalized config visibility.
- Preserve root-byte optimistic concurrency and compositional-source behavior.
- Run config normalization/render round trips, all product import suites, config-source format and
  resolver suites, config coordinator fault/race tests, API config tests, and fixed effective-revision
  fixtures.

## Workstream B: Blueprint And Resource Preparation

### Goal

Separate canonical validity, pure runtime decisions, environmental acquisition, and listener source.

### Target Boundary

```rust
GenerationCompiler::compile(&ValidatedConfig) -> GenerationBlueprint

GenerationPreparer::prepare(
    GenerationBlueprint,
    ListenerSource,
    ProcessRuntime,
) -> Result<PreparedGeneration, GenerationError>
```

`GenerationBlueprint` contains immutable compiled policy and value plans. DNS resolution, file and
store opening, credentials, listener reservation/adoption, UI and watcher checks, and worker creation
belong to preparation. `ListenerSource` explicitly represents bind/reuse, descriptor adoption, and
validation-only acquisition.

### Migration Slices

- [x] Make `runtime_plan` proof-only with `&ValidatedConfig`; no validating `ConfigDraft` facade
  remains.
- [x] Characterize which current planning operations perform I/O or mutate external state. Phase3A
  pins the historical order as unsupported policy, downstream TLS, pools, HTTP, forward proxy, RTMP,
  then L4/listeners, with stage sentinels for every reachable acquisition boundary.
- [x] Move pure service, route, pool, TLS, health, topology, and RTMP decisions into a blueprint.
  `GenerationBlueprint` now retains focused immutable decisions and stable indices rather than a
  `ConfigDraft`, copied canonical service collections, or acquired resources. The compiler-module
  call graph is checked by `scripts/verify-generation-blueprint-purity.mjs`, including mutation
  self-tests for direct, associated, closure/callback, mutable and reassigned function-item aliases,
  receiver ambiguity, indirect helpers, renamed imports, and forbidden-import acquisition. The
  scanner also balances chained constructor/call/index/parenthesized receivers such as
  `Arc::new(value).method()`, `make().method()`, `(value).method()`, and `items[0].method()`. It infers
  local receiver types from constructors, local return types, and bindings where possible, then
  traverses the resolved inherent method. Unresolved local/first-party chains fail closed; only
  explicitly reviewed pure std/external value methods with a recorded reason may remain unresolved.
- [x] Retain the public `TlsProfilePlan::policy()` compatibility accessor for 0.5. Its canonical
  `source_policy` copy exists only in the acquired `TlsProfilePlan`; blueprint decisions use the
  separately compiled TLS policy fields and never consult that copy.
- [ ] Phase3B: replace or narrow the classified 0.5 `RtmpCallbackEndpointBlueprint` and
  `VodApplicationBlueprint` cross-crate acquisition APIs behind the RTMP composition root. They remain
  public temporarily so the server can acquire DNS/filesystem resources from immutable RTMP values;
  Phase3A does not remove or widen them.
- [x] Move DNS, access-log, cache, media-store, recording-store, file, and watcher acquisition into
  the preparation path. `RuntimePlan` remains resource-free; an initially empty private
  `GenerationAcquisition` takes each completed typed stage immediately. Partial failure explicitly
  drops only completed stages in reverse order without changing the pinned acquisition/error trace.
- [ ] Consolidate normal and descriptor-adopted preparation after listener acquisition.
- [x] Introduce private `GenerationResources` with concrete plan, listener, and RTMP owners.
  A provisional preparation transaction owns listener reservations, listener metrics registration,
  and RTMP preparation before its only commit creates this aggregate. `PreparedGeneration` transfers
  it intact to `RuntimeGeneration`; public plan,
  reservation, registry, and RTMP-runtime accessors preserve their signatures through delegation.
  Listener registration remains a rollback transaction until every prepared RTMP runtime starts,
  and RTMP admission/recorder shutdown preserves its existing deadline and canonical service order.
  Prepared and running RTMP resources retain canonical plan order plus an immutable keyed lookup;
  partial runtime start drops already started services in reverse order before listener rollback. A
  separately named runtime-hosting `RtmpGenerationRuntime` and generic-manager cleanup narrowing
  remain Phase3B work; Phase3A does not widen that boundary. Runtime adapters may clone shared
  immutable handles only while retaining `Arc<RuntimeGeneration>`; those adapters are not teardown
  authorities, and the aggregate is destroyed once when the final generation authority is released.
  Generation process orchestration is not part of this aggregate: `serve_generation` owns ACME and
  certificate-file watcher supervisors, and must stop/join them before releasing generation authority,
  including when a later H3 or UDP runtime join fails.
  The ACME supervisor owns a cooperative cancellation token propagated through managed renewal,
  network/connect/TLS/socket loops, every ACME request, and polling sleeps. Controlled network and
  wait boundaries check within their scheduler interval. Local state/fsync/OpenSSL work and provider
  calls have before/after checkpoints but cannot be preempted; providers receive the operation and
  must cooperate for prompt exit. Shutdown cancels before join and ACME does not detach helpers.
  The outer five-second generation-process deadline may detach the generation thread under the
  existing process contract, so ACME authority release is eventual when a local/provider call blocks.
  Terminal cancellation is persisted before job control is released. A DNS cleanup journal survives
  cancellation until provider cleanup is confirmed, then is removed exactly once. Non-supervised
  renewal uses the same path with a default never-cancelled operation.
- [ ] Make API preview topology come from the same blueprint/preparation path rather than planning
  twice.
- [x] Classify the intentional `runtime_plan(&ConfigDraft)` to
  `runtime_plan(&ValidatedConfig)` signature change in the coordinated `0.5` boundary. There is no
  draft overload or compatibility facade to remove.

### Canonical Listener Inventory

One `ListenerInventory`, compiled from `ValidatedConfig`, should own stable identity, ordering,
protocol, bind, descriptor role and kind, control/data-plane classification, and metric policy.

```rust
ListenerInventory::compile(&ValidatedConfig) -> Result<Self, ListenerInventoryError>
ListenerInventory::compatibility_with(&self, candidate: &Self, mode: RuntimeMode)
ListenerInventory::reserve(&self, previous: Option<&ListenerReservations>)
ListenerInventory::adopt(&self, descriptors: AdoptedDescriptors)
ListenerInventory::export(&self, reservations: &ListenerReservations)
```

- [ ] Prove the inventory is exactly equivalent to the existing descriptor plan before migrating
  consumers.
- [ ] Route reservation, export, adoption, descriptor limits, and Unix restart classification through
  the inventory.
- [ ] Replace master-side listener tuple reconstruction and worker-side descriptor/readiness counts.
- [ ] Register management, statistics, and statistics-page listeners as real runtime entries instead
  of synthesizing zero-counter status.
- [ ] Use mode-aware compatibility for API `restartRequired` so supervised topology changes cannot be
  saved as apparently activatable changes.

### Contracts And Gates

- Preserve planning errors, topology ordering, listener rename/reorder behavior, descriptor slot and
  role order, the 64-descriptor limit, CLOEXEC ownership, Unix namespace safety, and UDP/QUIC kinds.
- Preserve validation failure parity by acquiring and dropping RTMP access logs, media roots,
  recording stores, and service preparations. Validation must not start recorder reapers, pull
  controllers, or auto-push transport; preserve all activation side-effect ordering.
- Run service-plan, health, topology, listener reservation, descriptor adoption, generation
  preparation parity, API preflight, and supervised TCP/Unix/UDP/H3 suites.

Phase3A's historical behavior fixture is generated and authenticated by
`scripts/verify-generation-blueprint-baseline.sh`. The verifier archives exact commit
`2d9c5fe66cd096d7a1d8e3bada8d5784b5f97f6c`, verifies hardcoded SHA-256 digests for the immutable
instrumentation patch and harness, applies them only inside `/tmp/opencode`, runs the archived code,
and byte-compares the generated result with `fixtures/generation-blueprint-2d9c5fe.json`. Schema 2
serializes the complete normalized validated decision tree, including HTTP action/method/host/path,
access/proxy/cache/gzip/static/fixed/redirect policy; cache stores and timelines; forward H1/H2/H3
auth, destination, CONNECT/CONNECT-UDP, limits and cache; L4; TLS identities, profiles and policy;
pool endpoints, passive and active health; and RTMP service, application, callback, VOD, relay, media,
recorder, auto-push and exec summaries. It independently serializes acquired service/TLS and pool
outputs, exact topology nodes/edges, errors, acquisition-trace stop points, and generation-validation
environmental failure codes with historical RTMP runtime-start counts. Temporary roots are
canonicalized and secret values are omitted. The verifier cleans temporary state on every exit and
has adversarial digest mutation checks.

## Workstream C: Admission And Runtime Hosting

### Goal

Make process/listener traffic admission and generation start/readiness explicit owners rather than
responsibilities hidden inside monitoring and `main.rs`.

### Admission Boundary

Introduce a process-scoped `ListenerRuntime` and a linear traffic lease:

```rust
ListenerRuntime::admit(
    &self,
    generation: &RunningGeneration,
    kind: RuntimeReferenceKind,
) -> Result<TrafficLease, AdmissionError>
```

`TrafficLease` owns the shared generation gate/reference plus listener and process permits. Service
permits, Pingora inner admissions, QUIC streams, UDP pseudo-sessions, and RTMP-specific limits remain
in protocol adapters. `RuntimeMetrics` becomes a snapshot/export facade over state rather than the
authority that admits traffic.

### Runtime Host Boundary

```rust
RuntimeHost::start(GenerationStartup) -> StartingGeneration
StartingGeneration::wait_ready(deadline) -> ReadyGeneration
DirectGenerationController::publish(ReadyGeneration) -> RunningGeneration
RunningGeneration::retire(deadline) -> RetiredGeneration
```

Only these handles may mark runtime start or failure. The host owns protocol assembly, runtime thread
handles, readiness, and bounded shutdown; the controller owns direct-mode candidate publication and
retirement. `main` remains CLI, signal, and top-level composition.

### Migration Slices

- [ ] Characterize acquisition order, exact generation-reference counts, rollback on every failed
  permit, and listener readiness transitions for each protocol.
- [ ] Extract common listener/process/generation admission without genericizing protocol service
  permits.
- [ ] Migrate reverse HTTP, forward HTTP, TCP, RTMP, H3, and UDP independently.
- [ ] Move `GenerationProcess`, runtime protocol assembly, and readiness waits from the binary into
  private server modules without behavior changes.
- [ ] Wrap existing startup and readiness state in linear handles.
- [ ] Move direct-mode previous-process retirement and shutdown into the controller.
- [ ] Migrate the supervised worker to `RuntimeHost` while leaving process replacement with the
  master.

### Contracts And Gates

- Preserve process-wide capacity across overlapping generations, administrative drain, cumulative
  counters, metric names/labels, listener states, access-record bounds, and unwind compensation.
- Preserve no-overlap publication, startup/quiesce deadlines, immediate-previous rollback, candidate
  quarantine, old-connection retention, recorder finalization, H2/H3 GOAWAY, UDP drain, and bounded
  detached joins.
- Run monitoring overlap/admission tests, protocol wrapper failure tests, generation race and
  publication tests, active/UDP/RTMP drain process tests, forward H1/H2/H3, reverse H3, TCP,
  WebSocket, and supervised replacement suites.

## Workstream D: Master-Owned Supervised Lifecycle

### Goal

Make the master the only owner of global supervised replacement, rollback, drain, and process
shutdown while each worker owns exactly one local runtime generation.

### Target Boundary

Management and configuration workflows depend on a mode-aware lifecycle port:

```rust
trait LifecycleControl {
    fn status(&self) -> GenerationStatus;
    fn request_reload(&self, expected: &EffectiveRevision) -> LifecycleRequest;
    fn request_rollback(&self, expected: &EffectiveRevision) -> LifecycleRequest;
    fn request_drain(&self, expected: &EffectiveRevision) -> LifecycleRequest;
    fn request_shutdown(&self, expected: &EffectiveRevision) -> LifecycleRequest;
}
```

Direct mode adapts `DirectGenerationController`. Supervised mode sends bounded authenticated requests
to the master. A `SupervisedGenerationCatalog` owns active, candidate, previous, quarantined, and
restart-required launch documents. A read-only `SupervisorSnapshot` returns role-qualified lifecycle,
revisions, listener state, events, and aggregate counters to the active worker.

### Migration Slices

- [ ] Add real-process characterization for management operations through `MasterRunner`.
- [ ] Extract the current reload state into a pure supervised generation catalog while preserving
  master event ordering.
- [ ] Introduce `LifecycleControl` and migrate direct mode without behavior changes.
- [ ] Publish read-only supervisor snapshots to the active worker.
- [ ] Add authenticated worker-to-master reload, shutdown, rollback, and drain requests one operation
  at a time.
- [ ] Reject worker-local global lifecycle operations in supervised mode after each operation has a
  master-owned route.
- [ ] Aggregate active and retired listener counters so cumulative values do not decrease during
  replacement and draining connections remain observable.
- [ ] Consolidate direct and supervised config change notification only after both have explicit
  lifecycle-control targets.

### Protocol Decision

Bidirectional administration should use a coordinated control protocol version 3 with exact fixtures
and explicit version-2 rejection. Mixed master/worker versions are not a current product contract, so
do not add a dual-protocol compatibility layer without deployment evidence.

Before broadly enabling supervision, reconcile the current product documentation with
`run_if_supported`: packaging and product claims must agree on whether direct or supervised serving is
the default.

### Contracts And Gates

- Preserve descriptor manifest version 1, Unix transport framing, revision preconditions, management
  JSON outcomes, metric families, stale-correlation rejection, and current restart-required
  restrictions.
- Prove only the active worker can submit lifecycle requests; stale and candidate/retired requests
  fail closed.
- Prove explicit rollback after retired-worker reap, master-owned shutdown, topology-aware restart
  classification, monotonic counters, SIGHUP/management parity, and all existing timeout, stale-ack,
  fault, model-trace, descriptor-leak, crash, and rollback behavior.

## Workstream E: RTMP 0.5 Boundary

### Goal

Retain cohesive RTMP state machines while replacing the second composition root and broad mutable
surface with value plans and opaque runtime/control handles.

### Compatibility Metadata

- [ ] Move nginx-RTMP directive metadata, forms, support status, value kinds, compatibility reports,
  and validation to `oxiroute-import::nginx`.
- [ ] Preserve the exact directive registry and compatibility report counts.
- [ ] Remove the production dependency from `oxiroute-import` to `oxiroute-rtmp`.
- [ ] Do not create an RTMP compatibility crate unless a second production consumer appears.

### Runtime Composition Root

The server should translate validated configuration to value-only RTMP service plans. RTMP should own
construction of applications, stores, workers, relays, resolvers, and service runtimes:

```rust
RtmpRuntimeSet::prepare(
    plans: impl IntoIterator<Item = RtmpServicePlan>,
    context: RtmpPrepareContext,
) -> Result<RtmpRuntimeSet, RtmpPrepareError>

RtmpRuntimeSet::service(&self, id: &str) -> Option<RtmpServiceHandle>
RtmpRuntimeSet::control(&self) -> RtmpControlHandle
RtmpRuntimeSet::begin_shutdown(&self, deadline: Instant) -> RtmpShutdown
```

- [ ] Introduce value-only application, media, recorder, relay, and auto-push plans without depending
  on `oxiroute-config`.
- [ ] Move RTMP store, resolver, worker, and application construction behind `RtmpRuntimeSet`.
- [ ] Keep server-owned access logging as a sidecar.
- [ ] Split registry internals into stream catalog and session/control ownership behind existing
  behavior.
- [ ] Migrate management, monitoring, Prometheus, and session startup to `RtmpControlHandle` and
  `RtmpServiceHandle`.
- [ ] Consolidate generation-owned RTMP state in `RtmpGenerationRuntime`.
- [ ] Make raw registry, hub, registration, lease, media-store, recording-store, worker, and raw
  service-runtime constructors crate-private at the `0.5` boundary.

### Bootstrap Semantics

Recorder, relay, auto-push, and live fanout intentionally differ in retention, ordering, fallback
audio, codec switching, overflow recovery, and byte accounting. Do not force them through one mutable
cache.

Share only event classification and checked batch measurement:

```rust
enum BootstrapClass {
    Metadata,
    AudioHeader,
    VideoHeader(VideoCodec),
    Keyframe(VideoCodec),
    FallbackAudio,
}
```

- [ ] Add a table-driven characterization matrix for AVC/HEVC/AV1 switching, audio-only streams,
  missing keyframes, event order, exact-fit/one-byte-over capacity, reserved events, and overflow
  recovery.
- [ ] Introduce common classification and owner-supplied item-size accounting.
- [ ] Migrate recorder, relay, auto-push, and live fanout independently and prove byte/order parity.
- [ ] Split private recording/relay/media modules only after the shared contracts settle and without
  widening visibility.

### Contracts And Gates

- Preserve RTMP config serde, startup error categories, management JSON, metric names/labels, IDs,
  recorder error mappings, and HTTP statuses.
- Run RTMP crate suites for catalog, fanout, publish/playback, multi-stream sessions, recording,
  relay, and auto-push; server service-plan, RTMP API, recording, and process-runtime tests; media,
  chunk, AMF, and handshake fuzz gates; and bounded FFmpeg/OBS interoperability evidence.
- Produce a public API diff against `0.4.1` and document every intentional removal in `0.5` release
  notes.

## Workstream F: Control-Plane Contract Ownership

### Immediate Contract Baseline

Resolve known valid-state drift before generating an artifact:

- [ ] Align UI relay phase/failure decoders with Rust `pulling`, `policy`, and `source` values.
- [ ] Reconcile operational event names and structured outcomes between Rust and TypeScript.
- [ ] Make event-page `latestCursor` authoritative on both sides; remove the UI fallback only in the
  same contract change.
- [ ] Extend the real Rust-process-to-TypeScript contract test to events, audit, TLS, and management
  mutations instead of relying only on TypeScript-authored fixtures.

### API Projection DTOs

- [ ] Add API-owned request, response, error, decimal-counter, and enum DTOs under the management API
  boundary.
- [ ] Convert from domain snapshots through explicit projection functions.
- [ ] Stop passing `RuntimeSnapshot`, generation state, listener/pool state, topology domain objects,
  ACME state, importer evidence, and arbitrary `serde_json::Value` directly to JSON responses.
- [ ] Replace topology generic attributes/metrics with discriminated API DTOs by node and overlay
  kind.
- [ ] Keep separate config request/view DTOs and preserve the redacted-secret sentinel and merge
  behavior.

### Endpoint Registry And Authentication

Each surface module should contribute immutable endpoint specifications while retaining its handler:

```text
operation id, exact path parser, method, auth policy, response mode,
request/success/error schemas, body/content policy, audit category
```

- [ ] Introduce `EndpointSpec` and derive recognition, auth, method policy, dispatch, and audit
  classification from the composed registry.
- [ ] Separate `ManagementAuth` from configuration state; it owns the secure bearer for every
  protected control-plane surface.
- [ ] Preserve the current order: correlation validation, exact route recognition, duplicate-header
  and bearer checks, method policy, body decoding, dispatch, audit result, correlation response.
- [ ] Model JSON, SSE, Prometheus, ranged bytes, media/VOD, readiness, and UI assets as explicit
  response modes rather than forcing them through one body abstraction.

### Generated Contract

The target checked artifact is `contracts/control-plane.openapi.json`, generated from API DTO schemas
and the endpoint registry, not handwritten YAML and not domain snapshots.

- [ ] Evaluate `utoipa::ToSchema` for Rust DTO schema generation and an `xtask` for deterministic
  endpoint document generation.
- [ ] Generate TypeScript operation/types with `openapi-typescript` and AJV 2020 standalone ESM
  validators that still accept `unknown` at the transport boundary.
- [ ] Generate synthetic fixtures from DTO builders; never capture live payloads or secrets.
- [ ] Add artifact freshness and compatibility diff gates, using `oasdiff` or an equivalent checked
  comparator.
- [ ] Migrate UI surfaces in this order: status/inventories, management mutations, monitoring,
  topology, audit/TLS, events/SSE, RTMP, then configuration/import.
- [ ] Keep `ui/src/api.ts` and `transport.ts` stable until all wrappers have migrated.

### Security And Compatibility

- Response DTOs are the primary allowlist. UI key suppression remains defense in depth only.
- Preserve `/api/v1` URLs, methods, statuses, content types, auth-before-method behavior, exact-path
  matching, public readiness/metrics exceptions, decimal-string counters, mixed existing casing,
  omission/null distinctions, and correlation/audit behavior.
- Do not replace runtime validation with generated TypeScript types or casts.
- Do not use strict unknown-field rejection as a substitute for constructing a secret-safe output
  projection.
- Run enum exhaustiveness and golden JSON tests, secret canaries, exhaustive route/auth/method
  matrices, SSE polling parity, Rust process contract tests, UI decoder/component/typecheck/build
  gates, and all management browser suites.

## Workstream G: Supervised Process Containment

This production-safety project follows master-owned lifecycle; it is not a prerequisite for the
configuration or control-plane work.

Linux process groups do not contain descendants that create another process group or session, and
RTMP exec workers can do so. The owning fix is one delegated cgroup-v2 subtree per supervised worker,
implemented behind `WorkerProcess` in `oxiroute-supervisor-process`.

- [ ] Version the launcher argument/metadata contract before adding containment information.
- [ ] Add cgroup-v2 capability and delegation probing without changing behavior.
- [x] Reconcile systemd `Delegate=` and control-group protection for a pinned delegated subtree.
- [ ] Attach the launcher before worker spawn and kill through `cgroup.kill` while retaining launcher
  authentication and reaping.
- [ ] Require containment first for supervised configurations with exec profiles.
- [ ] Prove replacement, crash, drop, timeout, and shutdown leave nested process-group and `setsid`
  fixture descendants absent and the worker cgroup empty.
- [ ] Keep direct and non-Linux runtimes unchanged; do not substitute `/proc` descendant scans.

The packaging checkpoint uses empty `Delegate=` (delegation with no requested controllers),
`DelegateSubgroup=supervisor`, and `ProtectControlGroups=private`, requiring systemd 257 or newer on a
unified cgroup-v2 host. The service's host `ControlGroup` is the delegated boundary and appears as
`/sys/fs/cgroup` in the service's private cgroup namespace; the main process is pinned at
`/sys/fs/cgroup/supervisor`, while systemd retains the boundary attributes and `.control`. This grants
the unprivileged service write authority only below its own unit cgroup and does not create, attach,
kill, or clean up per-worker cgroups. Those process-owned behaviors remain separate unchecked items.

## Delivery Waves

The workstreams have dependencies and must not be started as one repository-wide change.

| Wave | Deliverables | Safe parallel work |
| --- | --- | --- |
| 0 | Fix known API contract drift; record `0.5`, schema, supervision-default, and protocol decisions; pin fixtures. | API drift tests, public API inventory, listener differential fixtures. |
| 1 | Add `ValidatedConfig`; move nginx-RTMP compatibility policy to import; add first API projection DTOs. | These touch separate owners if facades remain stable. |
| 2 | Migrate source/import/render/revisions; introduce listener inventory; add value-only RTMP plans. | Source/import and listener inventory can proceed independently. |
| 3 | Introduce generation blueprint/preparer and RTMP runtime/control handles. | API DTO migration can continue outside config endpoints. |
| 4 | Extract listener admission, runtime host, and direct generation controller. | RTMP bootstrap characterization only, no implementation sharing yet. |
| 5 | Add master-owned lifecycle control, supervisor snapshots, and aggregate counters. | OpenAPI artifact tooling may proceed after endpoint DTO ownership settles. |
| 6 | Migrate endpoint registry/auth and generated UI contracts; migrate config/import last. | Private RTMP module moves only after public narrowing lands. |
| 7 | Implement bootstrap classification/budget sharing and cgroup containment as independent hardening projects. | Both are independent after their characterization gates. |

Do not parallelize changes that both touch configuration type-state, generation preparation,
listener identity, or lifecycle publication. Those boundaries form one critical path.

## Verification Program

Every implementation slice runs focused tests first and then affected application gates. Builds use at
most four workers.

```sh
cargo +1.97.1 fmt --all -- --check
nice cargo +1.97.1 clippy --workspace --all-targets --locked -j 4 -- -D warnings
nice cargo +1.97.1 test --workspace --all-targets --locked -j 4
nice pnpm --dir ui test -- --maxWorkers=4
nice pnpm --dir ui typecheck
nice pnpm --dir ui build
git diff --check
```

Additional gates are mandatory where relevant:

- GitNexus upstream impact before edits, change detection after edits, and cycle checks.
- Canonical rendering and effective-revision fixed fixtures.
- API artifact freshness, compatibility diff, runtime-decoder rejection, and secret canaries.
- Generation publication, supersession, rollback, shutdown, readiness, and drain race suites.
- Supervision model traces, protocol/descriptor byte fixtures, stale messages, timeouts, crashes, and
  descriptor/cgroup cleanup.
- RTMP fuzz smoke and bounded FFmpeg/OBS interoperability for RTMP behavior changes.
- Release archive and public API diff gates for the `0.5` boundary.
- Checked `2d9c5fe` API baselines and classified `0.5` deltas for config, config-source, import,
  server, and RTMP via `scripts/verify-public-api.sh`; keep the RTMP Phase 0 snapshot immutable.

## Explicit Non-Goals

- No generic `utils`, actor framework, socket trait, data-plane plugin system, or source-adapter trait.
- No crate split for HTTP adapters, RTMP media/recording/relay, or server runtime without a second
  consumer and an acyclic ownership interface.
- No one mutable RTMP bootstrap cache.
- No merger of authoritative supervision state, worker observations, and process ownership enums.
- No direct serialization of secret-bearing or mutable domain objects as API DTOs.
- No generated type-only frontend client that weakens the `unknown` transport boundary.
- No Axum or OpenAPI-first handler rewrite merely to simplify contract generation.
- No secure-filesystem abstraction shared across cache, recording, and ACME until their threat,
  recovery, locking, and durability contracts actually align.
- No move-only decomposition where it widens private visibility or splits coupled transitions.

## Completion Criteria

The architecture program is complete when:

- Only `ConfigDraft` is deserializable/mutable and only `ValidatedConfig` reaches planning/rendering.
- Runtime decisions, acquired resources, starting workers, ready workers, running generations, and
  retired generations are distinct owned states.
- Every listener consumer derives identity and compatibility from one inventory.
- Monitoring observes admission/lifecycle state but does not own traffic admission.
- Direct and supervised management operations reach the correct lifecycle authority.
- The supervised master can expose active/candidate/retired state and monotonic aggregate counters.
- The server no longer constructs RTMP implementation internals and the `0.5` public surface contains
  only value policy, service/session, control, content, and snapshot APIs.
- API responses are explicit projections and one endpoint registry drives auth, method, audit, and
  generated contract metadata.
- TypeScript types and runtime validators are generated from the checked Rust contract while secret
  projections and the `unknown` trust boundary remain explicit.
- Every slice passes its focused and repository gates, GitNexus reports only expected impact, and the
  dependency graph remains cycle-free.
