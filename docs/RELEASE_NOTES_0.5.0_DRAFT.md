# OxiRoute 0.5.0 Release Notes (Draft)

## Rust API Changes

nginx-RTMP source compatibility now belongs to `oxiroute_import::nginx`. The registry metadata,
compatibility reports, and directive validation retain their existing behavior under that module.
`parse_nginx_config`, `NginxDirective`, and `NginxParseError` were root-public standalone parser API
and have been removed from `oxiroute-rtmp`. They had no in-repository production consumer. Callers
should migrate to `oxiroute_import::nginx`'s bounded byte parser, loader, report, provenance,
diagnostic, and semantic APIs.

The removed standalone parser normalized `record_path /var/media\ files;` to a value without the
backslash. The authoritative importer intentionally preserves the unrecognized backslash-space
escape as `/var/media\ files` without a diagnostic. Its existing bounded lexer diagnostics and raw
source spans remain authoritative; importer parsing was not changed to emulate the removed parser.

The following 16 root-reachable exports were intentionally removed from `oxiroute-rtmp`:

- Types: `DirectiveCompatibilityReport`, `DirectiveContext`, `DirectiveError`, `DirectiveForm`,
  `DirectiveSpec`, `DirectiveStatus`, `DirectiveStatusCounts`, `NginxDirective`, `NginxParseError`,
  `RelayKind`, `RuntimeSupport`, and `ValueKind`.
- Functions: `directive_compatibility_report`, `directive_specs`, `validate_directive`, and
  `parse_nginx_config`.

Removing the directive types also removes their inherent `as_str`, `total`,
`compatibility_status`, and `runtime_form` methods from the RTMP crate. No runtime behavior,
configuration serialization, management API, metric, wire, or disk contract changes in this slice.

### Configuration proof API

The public mutable and deserializable configuration input type is now `ConfigDraft`. The 0.4.1
`oxiroute_config::Config` name and the mutating `validate_config(&mut Config)` facade were removed.
Callers deserialize or construct `ConfigDraft`, make authored edits, and consume it with
`ConfigDraft::validate(self) -> Result<ValidatedConfig, ConfigError>`.

`ValidatedConfig` remains transparently serializable and cloneable, but is not deserializable or
mutable. Planning, typed rendering, generation storage, TLS preparation, listener reservation, and
successful import evidence accept or store `ValidatedConfig`. Use `as_draft()` for read-only field
access and `to_draft()` only when beginning an explicit edit and revalidation transition.

The generic `render_value` and `render_uci_document` helpers are no longer public configuration
rendering APIs. Public typed rendering is
`oxiroute_config_source::render_config(format, &ValidatedConfig)`.

### Import candidate capability changes

Importer candidate finalization is now capability-based. A blocked import still retains its safely
lowered canonical work inside `oxiroute-import` for diagnostics, composition, provenance, and report
projection. Downstream crates cannot extract that importer-owned blocked `Config`, destructure the
candidate state, convert the candidate to a draft, or obtain a `ValidatedConfig` by stripping the
candidate's blockers. Successful imports expose `Option<&ValidatedConfig>` through `validated()`.

The following importer APIs are intentionally removed:

- `CanonicalDraft`, including its public canonical-model fields and `Default` implementation.
- `CanonicalFinalization`, its `Blocked` and `Finalized` variants, and its `config()`,
  `into_config()`, and `is_finalized()` methods.
- The public `CanonicalCandidate::draft` field and `CanonicalCandidate::config()`,
  `CanonicalCandidate::into_config()`, and `CanonicalCandidate::is_finalized()` methods.
- The interim `CanonicalCandidate::draft()` accessor and public `CanonicalCandidateState` export.
  Candidate state and blocked `Config` storage are now crate-private.
- `nginx::ImportReport::draft`, `nginx::RtmpImportReport::draft`, and
  `nginx::StreamImportReport::draft` fields.
- `nginx::ImportReport::{config, into_config}`, `nginx::RtmpImportReport::{config, into_config}`,
  and `nginx::StreamImportReport::{config, into_config}` methods.
- The interim public `draft()` accessors on those three nginx subreports.

Migration guidance:

- Replace successful-candidate `config()` calls with `validated()`. Use
  `validated.as_draft()` for borrowed read-only canonical access or `validated.to_draft()` only when
  an owned draft is intentionally required from an already validated candidate.
- Replace `is_finalized()` checks with `validated().is_some()`.
- Replace report-only reads of draft counts, presence flags, version, or `max_connections` with
  `summary()`, which returns `CanonicalCandidateSummary` without exposing canonical objects.
- Use public provenance, blockers, requirements, overlays, occurrence ledgers, diagnostics, and
  `ImportReportEnvelope` for blocked-candidate inspection. There is deliberately no replacement API
  that returns blocked canonical objects.

This is an ownership and capability invariant for the importer-held blocked candidate. Public source
and report evidence remains available, and callers may independently author a semantically equivalent
`Config`; the API does not claim that exact or equivalent values are impossible to derive independently.

### Configuration source ownership changes

Restricted Lua loading and deterministic typed rendering now belong to `oxiroute-config-source`.
The `mlua` dependency moved with that ownership; the sandbox remains text-only with no standard
libraries, a one-MiB source and output bound, four MiB of additional memory, and a one-million
instruction limit.

The 0.4.1 `oxiroute_config::load_lua(&str) -> Result<Config, ConfigError>` API moved to
`oxiroute_config_source::load_lua(&str) -> Result<ValidatedConfig, LuaConfigError>`. Lua evaluation,
source-bound, and canonical-validation failures are now owned by `LuaConfigError`; canonical
validation remains a typed `ConfigError` source. `ConfigError::Lua` and the source-specific
`ConfigError::SourceTooLarge` variant were removed.

The 0.4.1 `oxiroute_config::render_lua(&Config)` API was removed. The single typed rendering entry
point is now `oxiroute_config_source::render_config(format, &ValidatedConfig)`, which returns
`ConfigSourceError` and retains a typed `LuaConfigError` source for restricted Lua failures.

The 0.4.1 draft-returning composition facade
`oxiroute_config::compose_configs(&[Config]) -> Result<Config, ConfigCompositionError>` was removed.
Use `compose_validated_configs(Vec<ConfigDraft>) -> Result<ValidatedConfig, ConfigCompositionError>` for
complete authored drafts. Source resolution uses `compose_validated_fragments` to accept validated
native fragments, invalidate their individual namespace proofs at the merge boundary, and revalidate
the complete namespace once.

The temporary development-only names `load_validated_lua`, `render_validated_lua`, and
`render_validated_config` existed during the proof-type migration but were never 0.4.1 APIs. They are
not part of the release baseline removal list.

### RTMP value-plan API

`oxiroute-rtmp` now exposes value-only RTMP service, application, access, media, HLS, DASH,
recording, relay push/pull, auto-push, VOD, callback, and exec plans. `RtmpPrepareMode`,
`RtmpPrepareContext`, `RtmpPrepareCategory`, `RtmpPrepareSource`, and contextual
`RtmpPrepareError` establish the
preparation vocabulary without acquiring resources or starting runtime work. Every plan is
structurally opaque and can only be created through validating constructors; accessors expose only
immutable views or copied values. Constructors validate only runtime-intrinsic representability and
safety, including protocol identities and paths, parser hard bounds, nonzero structural values,
recorder track/rotation policy, media duration relations, exec safety, URL syntax, CIDR prefixes, and
credential path/value syntax. Canonical counts, maxima, duplicates, cross-references, shared-root
policy, live-feature relationships, and outbound destination/transport admission remain owned by
`ValidatedConfig`. Plan construction does not resolve DNS, read files or environment variables,
create paths or sockets, sample time or randomness, or start threads and executors.

The server now compiles validated configuration into focused immutable generation blueprints before
acquiring runtime resources. Pool, endpoint, health, upstream TLS/H3, cache, HTTP action and route,
forward proxy, listener, L4, downstream TLS, RTMP callback/application, and VOD decisions retain
stable indices and value policies rather than copied canonical service collections. Production
assembles and acquires the existing runtime objects in its prior order from those decisions, with
canonical access-log configuration retained only at its acquisition boundary. Preparation errors can
be contextualized with service, application, recorder, and exec-profile identity. Relay and VOD plans
validate outbound-policy CIDR syntax intrinsically while destination and transport admission remain
deferred to canonical validation and runtime preparation.

`TlsProfilePlan::policy()` remains available for public compatibility. Its canonical policy copy is
created only in acquired runtime state; generation blueprints make TLS decisions from compiled fields
and do not retain or consult that copy. `RtmpCallbackEndpointBlueprint` and
`VodApplicationBlueprint` remain classified 0.5 additions temporarily because the server currently
acquires callback DNS and VOD filesystem/network resources across the crate boundary. Phase3B will
replace or narrow those APIs behind the RTMP composition root rather than removing them in this
release slice.

The compiler path has a repository-owned call-graph purity gate and an authenticated archive-based
`2d9c5fe` behavior fixture. `scripts/verify-generation-blueprint-baseline.sh` verifies the exact
source commit plus hardcoded instrumentation and harness digests before regenerating and comparing
the schema-2 fixture. It serializes the complete normalized validated HTTP, cache, forward, L4, TLS,
pool/health, and RTMP composition decisions plus independent acquired service/TLS and pool outputs,
exact topology, errors, acquisition-trace stop points, and generation-validation environmental
failure codes with historical RTMP runtime-start counts. Error precedence remains unsupported policy,
downstream TLS, pools, HTTP, forward proxy, RTMP, then L4/listeners. DNS resolution, certificate and
credential reads, access-log, cache, media and recording store opening, and callback/VOD resource
acquisition remain outside pure compilation.

`RuntimePlan` is now an immutable resource-free decision object. Its public resource fields were
removed; acquired services, pools/health, TLS material, cache/log backends, RTMP catalogs, and runtime
stores are owned only by the generation. `validate_runtime_plan` performs environmental preflight and
returns the immutable plan without retaining those resources, while `runtime_plan` performs only
decision compilation.

`GenerationResources` is the sole lifecycle root and teardown authority for the immutable plan and
acquired data-plane runtime services. `GenerationAcquisition` begins empty, owns each completed TLS, pool,
HTTP, forward, RTMP, and L4 stage immediately, and explicitly releases completed stages in reverse
order on failure. A higher preparation transaction then owns provisional listener reservations,
listener metrics registration, and prepared RTMP runtimes; only its final commit creates
`GenerationResources`. Normal bind/reuse and supervised descriptor adoption remain separate APIs.
Disk-cache registry entries carry insertion identities and opening states; backend upgrade,
configuration comparison, use, and drop occur outside the registry mutex, and publication/removal use
token-qualified compare-and-swap semantics under concurrent open and retirement.

Generation-thread orchestration is intentionally separate from that data-plane root.
`serve_generation` owns ACME renewal and certificate-file watcher supervisors for the duration of the
generation process, and stops/joins them before its generation authority can be released. Later H3 or
UDP join failures therefore cannot detach an ACME worker or retain reconciler state past generation
shutdown.

Automatic ACME renewal carries one cooperative cancellation token and monotonic deadline. DNS,
connect, TLS handshake, socket read/write, and polling/sleep loops check cancellation at intervals of
at most 50 ms (the TLS retry loop checks every 1 ms). Local state writes, fsyncs, OpenSSL value work,
and in-process provider calls have checkpoints before and after the call; they are not preemptible.
Providers receive the operation context and are expected to cooperate, but a provider that blocks
without checking it can delay ACME thread exit. Generation shutdown requests cancellation before
joining. The existing outer five-second generation-process deadline remains authoritative and may
detach the generation thread; ACME authority and resources are then released only when the blocking
local/provider call eventually returns. ACME itself does not detach helpers or force-kill work.
Cancellation persists the managed job as `cancelled` once control returns and clears job control only
after terminal state is durable. DNS cancellation before confirmed provider cleanup retains the
pending journal for deferred recovery; only confirmed cleanup removes it, exactly once. Manual
renewal APIs retain existing behavior with a default never-cancelled operation.

Runtime adapters may clone immutable or internally shared resource handles only while the same
adapter retains `Arc<RuntimeGeneration>`. Those handles cannot outlive the generation root and do not
independently initiate teardown; `GenerationResources` is destroyed exactly once when the final
generation authority is released. Background health execution and asynchronous management media/VOD
work carry that generation authority explicitly. This slice adds transaction ownership only and does
not perform the later listener-source API consolidation.

Validation uses non-retaining proofs for access logs, persistent caches, media roots, and recording
stores: it does not create absent log/cache/media paths, start access-log workers, acquire cache or
recording ownership leases, or retain global registry entries. Bounded DNS and listener probes remain
environmental preflight. Activation reacquires fresh resources and starts workers once. Prepared and
running RTMP resources retain canonical plan order plus keyed lookup; partial start drops started
services in reverse order. Failed RTMP start does not commit process listener registration. Existing
`RuntimeGeneration` reservation, registry, RTMP runtime, admission-close, and recorder-shutdown APIs
retain their behavior through generation-owned accessors, including the caller-provided recorder
deadline and shutdown order.
Normal bind/reuse and supervised descriptor adoption remain distinct acquisition paths in this
phase; listener-source consolidation and dedicated runtime hosting remain later work.

The purity gate resolves balanced chained receivers, including constructor, function-call,
parenthesized, indexed, and `Arc::new` expressions. When it can infer a first-party receiver type it
traverses that inherent method; unresolved local or first-party chains are rejected unless the
terminal operation is an explicitly reviewed pure std/external value method. Mutation tests cover
forbidden acquisition hidden behind every supported chain shape and a reviewed pure chain.

The plans reuse existing RTMP value policies where those policies contain no acquired store,
resolver, worker, hub, or session state. Relay destinations and callbacks remain unresolved values,
credential references remain paths rather than loaded secrets, and callback URLs plus all public
plan filesystem roots, credential paths, auto-push paths, VOD sources, tokens, and exec inputs are
redacted from debug output. `RtmpPrepareContext` retains configured candidate listener addresses in
sorted, deduplicated order; it does not reserve or bind them. Existing raw runtime values remain
source-compatible in this phase; runtime acquisition, server migration, and their later visibility
narrowing remain later work.

Callers that intentionally mutate a validated value must use `to_draft()`, perform the mutation, and
complete a new owned `validate()` transition before rendering. Read-only callers can retain the proof
or use `as_draft()` for canonical field access. `ResolvedSource::config` now carries
`ValidatedConfig` directly.

Coordinator documents, persistable candidates, runtime generations, and finalized import evidence
now retain `ValidatedConfig`. Drafts remain only at source, edit, import-lowering, fuzz, and test
construction boundaries.

### Classified public API ledger

The repository-owned all-features comparison against exact commit
`2d9c5fe66cd096d7a1d8e3bada8d5784b5f97f6c` classifies every detected public API item below. A
"changed" item is one export whose before/after records appear as a pair in the checked delta.
Cross-crate signatures are named through aliases proven reachable from each first-party dependency's
rustdoc root, so private definition paths are neither reported nor invented. Baseline schema 4 uses
the independently checked canonicalizer fixture with reviewed SHA-256
`896c406527e412456f4f3a51281ced1363331def95e90b99b086f00726ac39e5`; the provenance verifier
hardcodes and authenticates that digest before executing an isolated copy.

- `oxiroute-config` (5 removed, 3 added, 2 changed): removed `Config`, `compose_configs`,
  `load_lua`, `render_lua`, and `validate_config`; added `ConfigDraft`, `ValidatedConfig`, and
  `compose_validated_configs`; changed `ConfigCompositionError` and `ConfigError` to reflect the
  proof transition and source-ownership changes described above.
- `oxiroute-config-source` (2 removed, 3 added, 3 changed): removed `render_uci_document` and
  `render_value`; added `LuaConfigError`, `compose_validated_fragments`, and `load_lua`; changed
  `ConfigSourceError`, `render_config`, and `ResolvedSource` to carry typed Lua errors and
  `ValidatedConfig`.
- `oxiroute-import` (2 removed, 14 added, 6 changed): removed `CanonicalDraft` and
  `CanonicalFinalization`; added `CanonicalCandidateSummary` plus
  `nginx::{DirectiveCompatibilityReport, DirectiveContext, DirectiveError, DirectiveForm,
  DirectiveSpec, DirectiveStatus, DirectiveStatusCounts, RelayKind, RuntimeSupport, ValueKind,
  directive_compatibility_report, directive_specs, validate_directive}`; changed
  `CandidateEvidence`, `evidence::CandidateEvidence`, `CanonicalCandidate`,
  `nginx::ImportReport`, `nginx::RtmpImportReport`, and `nginx::StreamImportReport` to retain
  validated success evidence while withholding blocked canonical drafts.
- `oxiroute-server` (4 removed, 6 added, 28 changed): removed
  `config_coordinator::{CanonicalConfigDocument, ConfigRevision, ConfigRevisionParseError,
  ValidatedConfigDraft}`; added `config_coordinator::{AuthoredRevision, EffectiveRevision,
  PersistableConfigCandidate, ResolvedConfigDocument, RevisionParseError}`; changed
  `GenerationError`, `HealthBuildError`, `ServicePlanError`, `ConfigWatcher`, `GenerationManager`,
  `GenerationRevision`, `GenerationStatus`, `ListenerMetrics`, `ListenerReservations`,
  `RtmpManagementApi`, `RuntimeGeneration`, `RuntimeMetrics`, `runtime_plan`,
  `runtime_plan_with_passive_failure_policy`, `service_specs`, `tls::prepare_tls`,
  `tls::prepare_tls_with_dns01_providers`, and
  `config_coordinator::{CanonicalConfigCoordinator, ConfigConflict, ConfigLoadOutcome,
  ConfigLoadRejection, ConfigSaveFailure, ConfigSaveOutcome, ConfigValidationOutcome,
  NativeImportSourceDocument}`. These changes distinguish authored and effective revisions and
  require validated configuration proof at planning, TLS, listener, generation, and coordinator
  boundaries.
  In particular, `runtime_plan`, `runtime_plan_with_passive_failure_policy`, and `service_specs`
  now require `&ValidatedConfig` instead of `&ConfigDraft`. This is the intentional coordinated
  `0.5` planning signature break; no draft overload or compatibility facade is retained.
- `oxiroute-rtmp` (16 removed, 36 added, 22 changed): removed
  `DirectiveCompatibilityReport`, `DirectiveContext`, `DirectiveError`, `DirectiveForm`,
  `DirectiveSpec`, `DirectiveStatus`, `DirectiveStatusCounts`, `NginxDirective`, `NginxParseError`,
  `RelayKind`, `RuntimeSupport`, `ValueKind`, `directive_compatibility_report`, `directive_specs`,
  `parse_nginx_config`, and `validate_directive`; added `RtmpPrepareMode`, `RtmpPrepareCategory`,
  `RtmpPrepareError`, `RtmpPrepareSource`, `RtmpPrepareContext`, `RtmpCallbackEventPlan`,
  `RtmpCallbackEndpointBlueprint`, `VodApplicationBlueprint`, the 20 opaque `Rtmp*Plan` value types
  for access rules and tokens, service, application, media, HLS/DASH,
  recorder, relay push/pull/client/credentials, auto-push, VOD, callbacks, fanout, and
  exec/environment policy, plus value-validation errors for HLS, callbacks, auto-push, VOD,
  recording-store, and session limits and `validate_callback_url_intrinsic`. Changed
  `DestinationPolicyError`, `ExecProfileError`, `RecorderWorkerStartError`, `ExecEnvironment`,
  `ExecProfile`, `HlsKeyConfig`, `HlsVariant`, `RecorderWorkerConfig`, `RecordingStoreLimits`,
  `RtmpAccessRule`, `RtmpAutoPushConfig`, `RtmpNetwork`, `RtmpOutboundPolicy`, `RtmpSessionCeilings`,
  `RtmpSessionLimits`, `RtmpTokenPolicy`, `VodApplication`, `VodLimits`, `VodSourceDefinition`, and
  `RtmpCallbackEndpoint` with additive error sources, equality, accessors, intrinsic validators, or
  explicit blueprint acquisition used by opaque plans. The directive registry
  APIs moved to
  `oxiroute_import::nginx`; the standalone parser has no replacement facade.

The machine-readable before/after records are checked in as
`docs/developer/fixtures/*-public-api-0.5.delta`. The ledger counts sum to 152 classified items:
29 removals, 62 additions, and 61 changed exports.

Standalone import report schema v1 is unchanged: finalized candidates still serialize their
canonical `config`; blocked candidates serialize `config: null`; and the existing `candidate.draft`
count/flag summary remains intentionally compatible. Provenance, blockers, requirements, overlays,
diagnostics, source graph, and ledgers also retain their existing names and semantics. The workspace
version remains unchanged until the coordinated 0.5 release boundary.
