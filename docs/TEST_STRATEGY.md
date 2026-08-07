# Test strategy

## Method

Every behavior change starts with the smallest failing test at the owning abstraction.
Implementation follows only after the failure demonstrates the missing behavior. Each
logical step is formatted, linted, tested, and committed independently.

The initial repository followed this sequence:

1. Four Lua configuration acceptance tests compiled and failed at the loader stub.
2. The restricted loader and validation made those tests pass.
3. Runtime planning compiled and failed at its stub.
4. Protocol-specific Pingora service planning made that test pass.

## Current worktree evidence

The current worktree adds or verifies the following bounded slices without promoting their
partial capabilities to parity:

- Import report tests retain deterministic source fingerprints, declared Varnish version,
  canonical provenance, and blockers for both finalized and blocked Varnish cases. The matching
  Varnish report cases are recorded in `coverage/evidence.json`.
- RTMP hardening rejects malformed AMF command payloads without panicking, and chunk tests feed
  two fragmented message streams one byte at a time while preserving per-stream state.
- `crates/oxiroute-server/tests/process_udp_drain.rs` covers UDP reload, existing-session
  retention, rejection of new work after drain, deadline cancellation, and listener release.
- UI contract/component coverage and the desktop/mobile browser suite cover redacted native report
  selection, provenance, preview, read-only GET boundaries, and responsive layout. The browser
  run passes with 21 tests and one existing desktop-only skip.
- GitNexus reports zero import cycles. This is a structural check, not protocol or production
  interoperability evidence.

## Test layers

The lists below distinguish checked-in coverage from planned release gates. Bounded parser fuzz
harnesses and the real-browser runner are checked in under `fuzz/`, `ui/tests/browser/`, and
`.github/workflows/`; the separate optional fuzz-smoke workflow is not a full fuzz campaign.

### Unit and property tests

- Config type decoding, validation, canonical rendering, revisions, and diagnostics.
- Tagged listener/endpoint decoding, DNS and Unix normalization/bounds, nullable listener/body
  limits, balancing defaults, and per-pool/total endpoint caps.
- Statistics page-only decoding/rendering, URI/refresh/admin bounds, bind conflicts, isolated
  routing, and same-origin loopback administration.
- Health-check defaults/type-specific fields, timing/threshold bounds including timeout equal to
  interval, DNS probe resolution, and
  Unix/TLS/health incompatibilities.
- Certificate/TLS-profile decoding, DNS/path/cardinality bounds, references, ALPN policy, upstream
  SNI/custom-CA fields, HTTP version ranges, and TLS/health/L4 incompatibilities.
- Route precedence including ASCII case-insensitive exact authority, health-state thresholds,
  health-aware round-robin, weighted round-robin, and least-connections selection, deterministic
  weighted distribution and lease ties/release, bounded ordered DNS address fallback shared by
  probes and traffic, configurable retries/passive-health policy, and limits.
- TCP timeout/half-close state, UDP passive failure attribution and exclusion classification, and RTMP catalog, fanout, FLV, canonical recorder validation/rendering,
  path/store/worker/controller/reaper state, bounded reaper backpressure outside registry/controller
  locks, and continuous/manual publisher integration.
- Cache freshness, recovery, quota/eviction, collapsed fills, conditional/range admission,
  surrogate-tag purge, and rejection of foreign prepared entries before memory or disk admission.
  Reverse HTTP integration covers memory reuse, persistent restart reuse, revalidation, and
  streaming bypass.
- nginx, HAProxy, Apache, Squid, and Varnish parser tokens, ordered source/include graphs,
  inheritance/resolution, diagnostics, decision accounting, provenance, and conservative semantic
   conversion. Squid additionally covers canonical direct-forward lowering, cross-format native
  references, static parent peer ordering/fallback, DNS/SSRF answer pinning, authenticated
  absolute-form/CONNECT daemon wires, HTTP/1 CONNECT-UDP Capsule datagrams, and bounded tunnel flushing;
  Varnish exact canonical lowering, native-reference evidence, report, preview, and fail-closed
  invocation tests cover the finalized subset.

Current property-style coverage includes deterministic rendering, revision behavior, bounded
parser/runtime cases, and ACME renewal-window state logic. UDP pseudo-session properties, broader
parser round trips, and long-running media/supervision properties remain planned.

### Integration tests

- Loopback-only upstreams and ephemeral ports; no root or Internet dependency.
- HTTP methods, bodies, trailers, hop-by-hop fields, upgrades, gRPC, and negotiated versions.
- Reverse HTTP lifecycle coverage includes bounded response buffering, rejection of chunked and
  oversized buffered responses, total deadline cancellation, early-response request draining,
  single outcome accounting, and replay-safe refused-stream retry gates.
- HTTP retry target exclusion, budgets through three, delayed same-server attempts, immediate final
  redispatch, the 64-attempt Pingora bound, budget exhaustion, pre-send connection retries for
  methods and bodies that have not reached an upstream, and no-replay gates after bodies, unsafe
  methods, upgrades, or established upstream connections.
- DNS endpoints retain canonical identity while resolving at connection time; Unix HTTP and L4
  upstreams, Unix listeners, and platform failure behavior are covered separately. Unit and wire
  coverage prove health, HTTP, and L4 use the same bounded address order and can reach a healthy
  secondary address without consuming route retry budget.
- Active TCP connection probes and HTTP request host/path/status/timeout behavior using loopback
  origins; startup unknown state, transition thresholds, probe shutdown, independent
  completion-based endpoint schedules, and the shared concurrency bound.
- Health-aware HTTP routing returns `503` for a matched pool with no selectable endpoint; monitoring
  tests cover pool/endpoint state, timestamps, failure reasons, and exact decimal-string counters.
- Passive-health tests cover canonical configuration bounds, layer-4 versus layer-7 observation,
  bounded error thresholds, immediate and mark-down ejection, recovery thresholds, immutable
  generation ownership, UI field editing, and Prometheus/event observability without revoking an
  existing lease.
- TCP full duplex, half-close, slow readers, backpressure, and cancellation.
- TCP connect/idle/lifetime deadlines and partial traffic accounting across failure paths.
- TLS SNI/ALPN, upstream verification, certificate rotation, expiry, and bounded downstream client
  certificate authentication for the current H1/H2/H3 listener paths; broader interoperability
  remains a gate.
- Atomic config writes, exact revision preconditions, stale conflicts, complete preflight before
  disk mutation, redaction, authentication, explicit pending-activation outcomes, and truthful
  restart-required Unix mode changes, including candidates with other edits, that leave the active
  generation and socket untouched; reservation tests cover namespace leases, connect-refused stale
  sockets, fail-closed restrictive sockets, concurrent ownership, and unsafe writable parents.
- Management API route authentication, monitoring/topology/RTMP/config response shapes, bounded
  event polling, generation/TLS/process operations, and real HTTP behavior, including
  decimal-string cumulative counters and complete top-level/listener topology state parsing. Exact
  `GET /ready` and `GET /metrics` public-probe behavior is tested separately.
- Continuous recording start/media/finalization, manual exact-ID start/stop, read-only candidate
  root preflight versus activation open, redacted relative recorder observability, and topology root
  omission.
- Monitoring counter lifecycle, Linux process/host parser fixtures, response shape, stale refresh,
  nullable capacities, transport-qualified binds, pool algorithms, active leases, and
  non-overlapping polling.

Packaged supervised UDP/H3 replacement under active traffic, broader active-traffic generation reload/drain
breadth, and managed ACME live/staging integration remain gates rather than complete release evidence.
Native dependency paths are watched and periodically re-resolved in the configuration reload paths.
UDP relay loopback coverage includes passive connect/send/receive/protocol attribution, bounded
ejection/reentry, and non-poisoning client/lifecycle/limit paths; no active UDP health-check type is
claimed. The direct UDP reload/drain process test also covers session retention, new-work rejection,
deadline cancellation, and listener release. Managed ACME protocol, state, bootstrap, TLS-ALPN
cleanup, DNS cleanup recovery, and scripted-transport paths are unit-tested. Durable audit
persistence/reopen/filter behavior is tested separately from the non-durable operational event ring.
Canonical watcher activation, route authentication, bounded event polling, SSE reconnect/resync,
RTMP statistics and session controls, malformed-AMF command handling, interleaved fragmented chunk
state, and fixed RTMP assembled-message rejection are current tested paths.

### Import conformance

Current nginx, HAProxy, Apache, Squid, and Varnish fixtures live under:

```text
crates/oxiroute-import/tests/fixtures/<product>/
crates/oxiroute-import/tests/fixtures/live/<host>/
```

The live fixture directories contain sanitized HAProxy sources, nginx source trees reconstructed
from `nginx -T` source markers, and live-origin hashed/read-only capture metadata. Tests pin the
exact direct origin hash commands and 2026-07-26 origin hashes without retaining raw capture bytes;
whitebeast HAProxy remains pending after its live configuration changed. Separate tests verify the
post-sanitization hash of every listed file, reject missing or extra files, compare operational
overlay inventories, and enforce the recorded sanitizer process. This evidence does not claim a
cryptographic signer. Tests also require stable complete source graphs, deterministic preprocessing, and
terminal accounting without treating remaining semantic blockers as a finalized-candidate claim.

The current suite combines source files with parser/semantic/lowering assertions and repository
coverage manifests. Apache vhost merge, HAProxy strict HTTP/TCP/stats, Squid static parent-peer and
direct-fallback, and Varnish exact static-cache paths have executable tests; Varnish report tests
also prove deterministic provenance and blocked-candidate preservation. Their broader product parity
remains open. The product/category layout, capability profiles, expected-model sidecars, and
optional native-validator output remain target fixture structure. Synthetic fixtures do not count
as live-host evidence unless `coverage/host-cases.json` explicitly maps them to live-origin hashed
metadata.

HAProxy lowering tests include an error-free static TCP fixture with a Unix listener, DNS
`leastconn` backend, exact per-listener admission, and no import-time DNS resolution, plus direct
tests named `positive_host_and_path_acl_conjunction_lowers_with_both_matchers_and_provenance`,
`non_equivalent_acl_conjunctions_fail_closed_without_fallback_routes`,
`unmodified_live_hostrouter_finalizes_with_exact_compatibility_policy`,
`dedicated_supported_stats_sections_lower_only_to_canonical_pages`,
`stats_frontend_response_rules_fail_closed_instead_of_disappearing`,
`bare_redispatch_lowers_to_delayed_same_server_retries_and_final_redispatch`, and
`redispatch_interval_forms_remain_blocking`. They cover the live hostrouter page, `unix@` mode,
case-insensitive Host route and fixed fallback, the non-audited host-shaped Host-plus-path fixture,
ACL reference provenance, reusable least-connections, final redispatch, and exact
listener/statistics-page timeout policy while retaining blockers for dynamic, negated, duplicate,
and unsupported forms. The `http-check send` tests additionally cover parser token retention,
semantic consumption, canonical health-field provenance, native-reference composition, runtime-plan
preparation, and blocking arbitrary headers or disabled-check contradictions. Wire coverage checks
page admission/request timeouts, DNS-rebinding and
forwarded-header rejection, Referer fallback, mapped-IPv4 loopback, and HEAD representation length.
Passive HAProxy tests additionally pin exact `observe`, `error-limit`, `on-error mark-down`, and
`retry-on conn-failure/conn-refused` lowering plus provenance, while broader retry-on tokens remain
fail-closed.

UI contract coverage includes the exact Vitest cases `accepts new canonical variants and rejects
invalid final redispatch shapes`, `edits ASCII case-insensitive authority matching and gates final
redispatch`, and `preserves and edits imported statistics pages and compatibility routing through
save`. The provenance workspace tests also cover redacted source graphs, canonical field
provenance, preview rendering, selection retention, and authenticated report requests.

The configuration editor suite also covers bounded weighted-round-robin weight editing, including
validation, server add/remove alignment, and algorithm switching, plus the bounded RTMP relay DNS
refresh control.

nginx HTTP conformance uses the explicit fragment API. Tests reject complete nginx files, require
exact response-control suppression before proxy finalization, preserve nginx hide/pass defaults,
share named upstream pools across routes/listeners, lower the bounded static-index subset without
rerunning nginx location selection, and carry only semantic first-wins host claims into routes.

nginx-RTMP conformance tests cover deterministic include inheritance, strict listener/application
lowering, continuous and all-media manual recording, supported named recorder blocks, bounded HLS
and key rotation, allowlisted exec profiles, same-daemon auto-push policy, native defaults,
complete-root HTTP+RTMP composition, no import-time root access, path/suffix/interval boundaries,
provenance, one terminal decision per occurrence, and fail-closed blocking of partial masks,
local-time suffixes, unsupported recorder fields, global policy, native DASH, and unsupported
service behavior. Synthetic fixtures remain implementation evidence only.

### Protocol conformance and interoperability

- HTTP semantics use standards-derived tests and independent clients/servers.
- HTTP/2 versions are asserted from negotiation and wire behavior, including the active forward
  listener's authority-only classic CONNECT path and rejection of arbitrary request forms. The active
  `forward_http3` listener is asserted through independent QUIC/H3 process-level tests for authority-only
  classic CONNECT and bounded failure paths; no positive H3 absolute-form forwarding claim is made.
- H3 upstream unit/wire tests use an independent QUIC origin to assert TLS/SNI/`h3` negotiation,
  request bodies, safe response framing and trailers, no-ALPN rejection, bounded request/response
  admission, malformed input, cancellation, and disabled migration/0-RTT. Reverse H3 process tests
  implement and test fixed/redirect/static/proxy responses, generation-owned GOAWAY drain, reload,
  generation-owned UDP listener release, exhaustion, malformed traffic, and no silent protocol
  fallback; passing active-traffic drain evidence remains a gate.
- Supervised process tests transfer authenticated typed TCP, Unix, UDP, and QUIC descriptors, verify
  close-on-exec ownership and worker status, exercise UDP relay and H3 wire serving, and cover
  generic replacement, rollback, drain, listener-start failure, and worker crash recovery; typed
  UDP/H3 replacement under active traffic remains a production gate.
- TLS tests use independent OpenSSL/rustls clients where applicable.
- gRPC and WebSocket behavior use independent implementations. Bounded PROXY v1/v2 parser and
  TCP/UDP integration tests cover malformed, timeout, mismatch, over-read, and quota boundaries;
  broader interoperability conformance remains planned.

Current Unix TLS/H2/gRPC coverage includes:

- OpenSSL TLS 1.0/1.1 clients prove downstream rejection without HTTP bytes; rustls clients prove
  verified reverse and `forward_http1` downstream TLS/H1, bounded pre-handshake admission,
  incompatible ALPN and invalid-client closure, a TLS 1.3-only listener minimum, TLS/H2 ALPN and a real H2 stream,
  full-chain delivery to a root-only client, ECDSA identity loading, pre-handshake connection caps,
  exact-over-wildcard SNI selection, no-SNI default selection, and H2-only no-ALPN closure before
  HTTP parsing.
- rustls upstream origins prove custom-CA and hostname failures send no HTTP bytes, observe the
  configured SNI, verify an intermediate-only custom trust anchor, and cover H2-only success, H2
  ALPN mismatch, and flexible HTTP/1.1 fallback.
- An independent H2 client/origin pair proves gRPC response DATA plus ordinary and trailers-only
  trailing metadata survive the full downstream-proxy-upstream path. Additional H2/gRPC wire
  cases prove multi-DATA request and response streaming, flow-control backpressure, bounded large
  bodies, listener connection admission, read/write deadlines, downstream cancellation with an
  upstream reset, upstream reset propagation, malformed content-length handling, required client
  authentication, and no H2-to-H1 downgrade under an H2-only policy.
- Rotation tests prove existing connections retain their certificate, new handshakes use a newly
  published complete generation without session resumption, rotating one SNI identity does not
  change another, and concurrent handshake waves switch completely after publication. A
  multithreaded generation-layer race repeatedly publishes while readers snapshot and proves every
  observation is one complete old or new generation.
- Certbot lineage tests cover common numbered revisions, mixed and malformed live links, archive
  containment, descriptor-relative no-follow reads, private-key reuse, exact artifact composition,
  bounded material, secure key modes, ancestor replacement after archive descriptor pinning,
  debounced and periodic reconciliation, watch rebuilding, bounded shutdown publication, redacted
  monitoring, and real-wire renewal that preserves existing connections and unrelated SNI identities.
- Low-security OpenSSL origin/control pairs prove upstream TLS 1.0/1.1 can negotiate directly but
  is refused by OxiRoute's pre-handshake TLS 1.2/cipher policy without decrypted origin bytes;
  TLS 1.2 remains a positive control.

Active-traffic reload/drain breadth, ACME CA-staging integration, broader HTTP/2 and gRPC
conformance, fuzz/crash campaigns, and packaged supervised UDP/H3 replacement under active traffic
remain release gates. The canonical watcher, live generation activation, bounded event polling, SSE
paths, and durable audit store are implemented but do not by themselves close those broader gates.

### Checked-in fuzzing

Checked-in bounded harnesses cover configuration/native source parsing, forward targets, over-read
I/O, RTMP handshake/chunk/AMF decoding, TLS ClientHello, PROXY v1/v2, bounded UDP datagram headers,
HTTP/1 parsing, and the current public parser surfaces. `cargo check` and the optional `fuzz/smoke.sh`
workflow prove buildability and a bounded smoke path; they do not claim coverage, corpus stability,
or crash-free long-running execution. Daemon routing, socket/session lifecycle, private integration
helpers, and full protocol wire sessions remain deliberate harness boundaries.

The release workflow does not require a full `cargo-fuzz` campaign. The separate optional
`.github/workflows/fuzz-smoke.yml` workflow compiles the isolated fuzz workspace and runs its smoke
when `cargo-fuzz` and nightly tooling are available; full bounded campaigns, corpus triage, and
long-running crash evidence remain separate release gates.

### UI end-to-end

- Vue component tests cover bearer unlock/re-lock, current canonical-field editing, validation,
  Lua/candidate review, clean refresh, dirty-draft retention, revision conflicts, exact save
  outcomes, navigation, RTMP statistics/session controls, passive-health/retry editing, bounded
  weighted-round-robin weight editing, durable audit filtering/cursor pagination, native import
  report selection/blockers, certificate actions, event parsing, and redaction. TLS-ALPN-01 selection
  and its guidance that a listener on public TCP port 443 must already be deployed are covered;
  selecting it does not deploy the listener, and listener deployment plus CA-staging evidence remain
  release gates. Native-file editing remains outside the frontend workflow.
- Monitoring component tests cover pool availability, endpoint state/check totals, exact counters,
  failure labels, empty-pool rendering, and retention after transient failures.
- Component tests exercise mobile controls and keyboard navigation in jsdom.
- `ui/tests/browser/dashboard.spec.ts` runs against the built static UI in desktop Chromium and a
  mobile Chromium device profile. It covers dashboard layout, token unlock/relock, save/review,
  revision conflict, dirty-draft external edits, SSE reconnect from `Last-Event-ID`, operational
  controls, certificate redaction, durable audit browsing, stale revision retention, and redacted
  native import report selection/provenance/preview through authenticated GET-only requests. It
  asserts the read-only boundary and responsive report layout. The browser run passes with 21 tests
  and one existing desktop-only skip.
- The browser harness aborts non-local requests and scripts API/SSE responses; it does not start the
  daemon or contact a production or ACME endpoint.

## Failure injection

Canonical storage, certificate publication, and recording storage/worker tests inject failures
around read-only preflight, activation open, ownership, quota sharing, write, sync,
replacement/publication, cleanup, worker start, nonblocking queue discontinuity, bounded reaper
backpressure/cancellation, and shutdown. Cross-process quota coordination, broader runtime activation,
typed UDP/H3 replacement, and crash-injection matrices remain planned release evidence.

## Release gates

The current full local check set is:

```sh
cargo +1.97.1 fmt --all --check
cargo +1.97.1 clippy --workspace --all-targets --locked --jobs 4 -- -D warnings
cargo +1.97.1 test --workspace --locked
cargo +1.97.1 fmt --manifest-path fuzz/Cargo.toml --check
cargo +1.97.1 check --manifest-path fuzz/Cargo.toml --locked --jobs 4
bash fuzz/smoke.sh
pnpm --dir ui test
pnpm --dir ui build
pnpm --dir ui test:browser -- --workers=2
```

The current worktree run passes the Rust 1.97.1 format, clippy, and locked workspace-test gates,
the fuzz format/check gates, and the UI unit, browser type-check, build, and browser gates. The
browser matrix reports 21 passed tests and one existing desktop-only skip. `bash fuzz/smoke.sh`
exits successfully but skips execution because `cargo-fuzz` is unavailable; this is not long-running
fuzz or crash-corpus evidence. The checked-in Linux workflow at
`.github/workflows/ci.yml` enforces the workspace format/lint/tests, locked Rust 1.97.1 tests,
coverage-manifest, package metadata, UI, browser, and local ACME jobs. It does not run the fuzz
commands. The separate optional `.github/workflows/fuzz-smoke.yml` compiles the isolated fuzz
workspace and runs its bounded smoke when tooling is available.
`.github/workflows/audit.yml` adds dependency, license, source,
RustSec, and UI vulnerability checks; `.github/workflows/release.yml` verifies version metadata,
archive contents/checksums, and build provenance. Representative focused gates are
`cargo test -p oxiroute-config --test lua_config`,
`cargo test -p oxiroute-import --test coverage_manifests`,
`cargo test -p oxiroute-import --test nginx_rtmp`,
`cargo test -p oxiroute --test rtmp_api`,
`cargo test -p oxiroute --test rtmp_recording`,
`cargo test -p oxiroute --test wire_tls_interop`,
`cargo test -p oxiroute-rtmp --test recorder_session --test recording_store --test recording_worker --test push_relay --test session_policy`,
and `pnpm --dir ui test`.

Future automation may add CA-staging ACME integration, longer fuzz campaigns, crash-corpus retention,
and supported-platform full builds. Browser tests use fake local API/SSE responses and do not claim
ACME protocol or active-daemon interoperability coverage.
`pnpm --dir ui build` already runs `vue-tsc --noEmit` before the Vite build.

The release gates still open are explicit: CA-staging issuance and renewal for all three managed
challenge types; active production-traffic reload/drain and packaged supervised replacement,
rollback, restart, and crash recovery under traffic; process-level FFmpeg/OBS interoperability;
long-running fuzz runs and crash-corpus triage; broader independent protocol/import interoperability;
and a clean dependency audit. `cargo audit -D warnings` currently denies three unmaintained-
dependency warnings, so no audit pass is claimed.

A capability cannot move to `stable` in a public matrix while its failure-path,
reload/rotation, observability, and interoperability tests are missing.
