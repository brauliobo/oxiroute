# Management API and UI specification

## Deployment boundary

The control plane shares the daemon but remains logically separate from traffic listeners. The
current schema requires a loopback bind. Every recognized management/API route requires exactly one
bearer token loaded at startup, except exact `GET /ready` and `GET /metrics`. UI assets are
read-only static responses, and separately configured HAProxy-compatible statistics pages have
their own page-only contract. There is no remote management mode or permissive CORS default.

## API conventions

- Base path: `/api/v1`.
- JSON request and response bodies.
- RFC 3339 UTC timestamps for future configuration resources; existing runtime and monitoring
  fields explicitly suffixed `UnixMs` or `_unix_ms` use Unix-millisecond numbers.
- Stable machine-readable error codes plus human-readable details.
- Secret values are write-only references or redacted placeholders.
- Configuration writes use one raw `If-Config-Revision` header containing the current 64-hex disk
  revision. `If-Match` is not accepted as an alias.

Implemented and recognized endpoints:

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/api/v1/config` | Typed config, source format/composition metadata, disk/candidate/active revisions, format-preserving preview, and compact redacted diagnostics. |
| `POST` | `/api/v1/config/validate` | Validate a typed draft without writing or activating it. |
| `PUT` | `/api/v1/config` | Preflight and revision-checked durable canonical save; changed generations are queued for watcher-driven activation. |
| `GET` | `/api/v1/topology` | Active redacted configuration graph with runtime health overlays. |
| `GET` | `/api/v1/monitoring` | Runtime process, host load, listener traffic, pool/endpoint health, and RTMP activity snapshot. |
| `GET` | `/api/v1/status` | Active generation, disk/candidate revisions, degradation, and listener status. |
| `GET` | `/api/v1/listeners`, `/api/v1/pools`, `/api/v1/servers` | Active operational inventory. |
| `POST` | `/api/v1/listeners/administrative-state`, `/api/v1/pools/administrative-state` | Revision-checked listener or pool admission state changes. |
| `POST` | `/api/v1/servers/administrative-state`, `/api/v1/servers/health-override`, `/api/v1/servers/checks` | Revision-checked server state, observed-health override, or probe enablement changes. |
| `PUT` | `/api/v1/servers/max-connections` | Revision-checked server capacity override or reset. |
| `POST` | `/api/v1/servers/refresh-dns` | Resolve every prevalidated target and report explicit non-atomic per-server outcomes. |
| `GET` | `/api/v1/generations` | Active, previous, candidate, and quarantined generation state. |
| `POST` | `/api/v1/generations/reload`, `/api/v1/generations/rollback`, `/api/v1/generations/drain` | Revision-checked generation preparation, rollback, and drain operations. |
| `GET` | `/api/v1/tls` | Redacted certificate and Certbot watcher status. |
| `POST` | `/api/v1/tls/reconcile` | Revision-checked reconciliation of externally owned Certbot lineages. |
| `GET` | `/api/v1/events?after={cursor}&limit={n}` | Bounded cursor polling over the in-memory operational event ring. |
| `POST` | `/api/v1/process/drain`, `/api/v1/process/shutdown` | Revision-checked process drain or shutdown requests. |
| `GET` | `/api/v1/rtmp/streams` | Active RTMP catalog and runtime capabilities. |
| `GET` | `/api/v1/rtmp/streams/{streamId}` | One exact-ID active stream snapshot. |
| `POST` | `/api/v1/rtmp/streams/{streamId}/recorders/{recorderId}/start` | Request one exact-ID manual recorder start. |
| `POST` | `/api/v1/rtmp/streams/{streamId}/recorders/{recorderId}/stop` | Request one exact-ID manual recorder stop. |

Every `/api/v1/...` route in this table requires the management bearer token. The only public
recognized API probes are exact `GET /ready` and `GET /metrics`; wrong methods and unknown paths do
not create an authentication bypass.

Native import routes and unbounded event streaming are not implemented. Bounded event polling is
implemented and is not an SSE contract. Recorder routes control configured `start = "manual"`
workers in the active publisher incarnation and use the same bearer authorization as other
recognized management routes.
`capabilities.manual_recording` is true when the active config contains at least one manual
recorder; continuous-only recording does not set that capability.

The statistics listener additionally serves public `GET /metrics` and `GET /ready`. Authenticated
`GET /stats` renders the local server table. `GET /api/v1/status` on that listener is also
authenticated. `POST /stats/admin` accepts one JSON
`{pool, server, action}` target plus `If-Generation-Revision`; `GET` and `HEAD` return `405` and can
never mutate state.

Standalone `stats.pages[]` listeners are a separate public page-only contract. `GET`/`HEAD` below
the configured URI prefix returns the HAProxy-compatible table; unrelated observability/API paths
return `404`. HEAD sends no body but retains the GET representation's `Content-Length`. Each page
has its own optional `max_connections` and client/request-header/keep-alive downstream timeouts,
enforced and reported through the normal listener admission and metrics path. `admin = "localhost"`
exposes form controls only to loopback peers and accepts a form POST only when the transport is
loopback, Host is `localhost` or a loopback IP literal with an optional valid port, and Origin has
that exact HTTP authority. When Origin is absent, an HAProxy-compatible same-authority HTTP Referer
is accepted. Forwarded identity headers, DNS Host names, duplicate headers, and mismatched
authorities fail closed. It does not use the statistics Bearer token. `admin = "disabled"` permits
no mutation. The token-protected statistics API uses the same IPv4-mapped loopback transport check.

Manual recorder responses are exact:

- `200` with the recorder snapshot when the requested state is already settled.
- `202` with the recorder snapshot whenever the returned phase is `starting` or `stopping`.
- `400 invalid_stream_id` or `400 invalid_recorder_id` for malformed UUIDs.
- `404` for an unknown action path or `404 rtmp_resource_not_found` for an absent stream/recorder.
- `405` with `Allow: POST` for another method.
- `409 rtmp_state_conflict` for no active publisher, a continuous recorder, or an opposite
  transition in progress.
- `501 rtmp_recording_unavailable` when no manual recorder capability exists in the active runtime.
- `503 rtmp_recorder_start_failed` or `503 rtmp_recorder_stop_failed` when the active recorder
  backend cannot execute the requested transition.

### Configuration authentication

Whenever `management` is configured, startup requires `OXIROUTE_MANAGEMENT_TOKEN_FILE`. The file is
opened without following symlinks, MUST be a regular file with mode `0400` or `0600`, and is bounded
to 514 bytes so a maximum-size token may include one line ending. One trailing LF or CRLF is removed;
the remaining token MUST be 32 through 512 visible ASCII bytes (`0x21` through `0x7e`). Other
whitespace is part of the token or makes it invalid.

Each recognized non-public API request requires exactly one `Authorization: Bearer <token>` header. Duplicate
authorization returns `400 duplicate_authorization`; missing, malformed, or incorrect authorization
returns `401` with `WWW-Authenticate: Bearer`.
The token is hashed after startup loading and compared in constant time. Non-configuration routes
apply the same authentication check; only exact `GET /ready` and `GET /metrics` bypass it.

### Configuration routes

`GET /api/v1/config` returns `200` with `schemaVersion`, `diskRevision`, `candidateRevision`,
`activeRevision`, normalized `config`, `configFormat`, `compositional`, `dependencyCount`,
format-preserving `configPreview`, and `diagnostics`. A restricted-Lua root also receives the legacy
`luaPreview` alias. If the persisted canonical file cannot be read and decoded, it returns
`503 canonical_config_unavailable`, the known disk revision or `null`, the unchanged active
revision, and redacted diagnostics.

`POST /api/v1/config/validate` accepts exactly `{ "config": <canonical object> }`. A `200` response
contains `candidateRevision`, `normalizedConfig`, `configFormat`, `compositional`, `dependencyCount`,
format-preserving `configPreview`, diagnostics, `restartRequired`, and a candidate topology explicitly marked
`not_active`. A restricted-Lua coordinator also returns the legacy `luaPreview` alias. Validation
compiles the complete runtime plan,
loads configured UI assets, starts then shuts down a candidate Certbot watcher, and performs a
read-only recording-root ownership/quota preflight. Recorder preflight does not create an ownership
lock, probe, partial, or recording file. Actual daemon activation separately opens and pins each
store and can still fail if a root changes after validation.

`PUT /api/v1/config` requires the same body plus `If-Config-Revision`. It performs the same complete
preflight before opening the write transaction, except that a detected active Unix-listener mode
change validates the complete runtime plan without attempting to rebind the active path and is
classified as restart-required, including when the candidate contains other changes. A `422`
preflight failure cannot mutate the canonical file. The
save then re-reads and compares the authoritative disk bytes, writes mode
`0600`, synchronizes, atomically replaces, and synchronizes the parent directory.

Typed saves preserve the root's selected syntax but normalize it to deterministic output. They are
allowed only when the authoritative root is non-compositional. If `templates`, `nginx_server`,
`haproxy_server`, or `squid_server` contributed to the loaded source, `PUT` returns `422` with
`E_COMPOSITIONAL_ROOT`; it never destroys those declarations by replacing them with a flattened
typed object. `config compose` is the explicit operator-controlled flattening path.

The configuration UI mirrors all canonical statistics-page fields, the ASCII case-insensitive exact
authority selector, retry budgets through three, and final redispatch. It preserves these values
when loading, validating, and saving an editable imported/flattened canonical configuration; the
final-redispatch control is enabled only for a positive same-server retry budget. Compositional
native-reference roots remain read-only as described above.

Successful writes return `200` with disk, candidate, and active revisions, diagnostics, and one of
three exact outcomes:

- `saved_pending_activation`: disk changed, `activationState` is `pending`, and
  `restartRequired` is `false` while the watcher starts the prepared generation.
- `unchanged_active`: the candidate revision equals the active generation, `activationState` is `active`, and
  `restartRequired` is `false`.
- `saved_restart_required`: a valid durable candidate changes the mode of an active Unix listener,
  `activationState` is `restart_required`, and `restartRequired` is `true`. The active generation
  remains unchanged until a process restart applies the complete saved candidate.

There is no `202` API response. For a changed save, the file watcher observes the durable
replacement, prepares a candidate, and the generation supervisor publishes it only after the new
runtime reports ready. The `200 saved_pending_activation` response does not claim that publication
has already completed; clients observe `activeRevision` or generation status for completion.
`saved_restart_required` is deliberately not queued for live publication.

Configuration request failures use these statuses:

- `400` for malformed JSON, malformed/duplicate revision or content-length headers, or an unreadable
  request body.
- `401` for missing or invalid bearer authorization.
- `404` for a non-exact or unavailable route.
- `405` for a wrong method on an exact route, with `Allow` set.
- `409` for a stale revision; no write occurs and the response includes the latest loadable
  authoritative configuration.
- `413` when the declared or streamed body exceeds 1 MiB.
- `415` unless one `Content-Type` header has media type `application/json`.
- `422` for a body outside the canonical schema, canonical validation failure, or runtime/UI/Certbot
  preflight failure; no write occurs.
- `428` when `If-Config-Revision` is absent on `PUT`.
- `500` when a durable write fails and the candidate is not already the authoritative disk bytes,
  or when the system clock cannot provide a supported Unix-millisecond timestamp.
- `503` when the persisted canonical file, or authoritative state needed to report a conflict,
  cannot be loaded.

Exact paths are required; trailing slashes and repeated separators return `404`.

Full certificate inventory, issuance, renewal, and certificate UI workflows are planned, not
implemented by the current TLS slice. The current authenticated `GET /api/v1/tls` status route and
`POST /api/v1/tls/reconcile` operation expose and reconcile externally owned Certbot lineages.
Lua-configured direct-file identities are prepared at startup and configured Certbot identities are
watched and atomically reconciled. The management API cannot add, replace, or renew certificate
material directly.

The implemented monitoring snapshot contains daemon uptime, process CPU/RSS/virtual memory,
threads, open file descriptors, host load averages and memory, aggregate/listener connection and
byte counters, nullable per-listener connection capacities, pool algorithm and endpoint lease
state, pool/endpoint health, and RTMP
stream/publisher/subscriber/media totals. It also includes redacted `certbotCertificates` entries
with identity name, active archive/content revision, expiry, and last outcome/error code, plus
`certbotWatcher` health and bounded counters. Source paths, SAN labels, PEM, and private material
are excluded. Process and host sampling
currently reads Linux `/proc`; a sampling or parsing failure returns `503` instead of fabricated
zeroes. CPU utilization is `null` until two successful samples establish a delta.

`upstreamPools` preserves canonical pool and endpoint order and has this response shape:

```json
{
  "upstreamPools": [
    {
      "name": "web",
      "algorithm": "round_robin",
      "availableEndpoints": 1,
      "totalEndpoints": 2,
      "unavailableSelections": "3",
      "endpoints": [
        {
          "address": "127.0.0.1:3000",
          "activeLeases": "2",
          "state": "healthy",
          "lastCheckedAtUnixMs": 1784736000000,
          "lastTransitionAtUnixMs": 1784735995000,
          "successfulChecks": "42",
          "failedChecks": "1",
          "consecutiveSuccesses": "4",
          "consecutiveFailures": "0",
          "lastFailure": null
        }
      ]
    }
  ]
}
```

Endpoint `state` is `unchecked`, `unknown`, `healthy`, or `unhealthy`. Pools without a health
policy report selectable `unchecked` endpoints; health-enabled pools begin with unavailable
`unknown` endpoints. `lastCheckedAtUnixMs` and `lastTransitionAtUnixMs` are Unix-millisecond
numbers or `null` before the corresponding event. `lastFailure` is `timeout`, `connect_failed`,
`unexpected_status`, `protocol_error`, or `null`. To preserve every cumulative `u64` exactly in
JavaScript, aggregate/listener accepted, rejected, and byte totals; endpoint leases/check totals;
pool unavailable selections; RTMP media/recorder totals; and Certbot watcher counters are base-10
integer strings. Current gauges, timestamps, and configuration-bounded counts remain JSON numbers.
`activeLeases` counts
currently held HTTP-request or L4-relay leases; `unavailableSelections` counts selection attempts
made while the pool had no selectable endpoint. Endpoint `address` is the normalized canonical
display identity: `IP:port`, `host:port`, or an absolute Unix path.

Listener `bind` is a stable transport-qualified string, either `socket:<address>` or
`unix:<path>`. `maxConnections` is a positive JSON number for a bounded listener or `null` for
unbounded admission.

RTMP stream snapshots expose configured recorder `id`, `name`, `manual`, structured phase,
`changed_at_unix_ms`, decimal-string byte/segment/discontinuity counters, and only relative
`current`, `last_completed`, recoverable-partial, or published-not-durable names. Phases are `idle`,
`starting`, `recording`, `stopping`, or `failed`; transition phases carry an operation ID,
`recording` carries its start time, and `failed` carries a stable categorical code. The monitoring
snapshot aggregates recorder bytes, segments, and discontinuities and repeats redacted per-recorder
relative names. Neither response contains a configured recording root or stream query arguments.
Failure codes are `open_failed`, `write_failed`, `close_failed`, `backend_unavailable`,
`file_sync_failed`, `publish_failed`, `directory_sync_failed`, `queue_discontinuity`,
`unsupported_codec`, `shutdown_timed_out`, `worker_panicked`, and `stale_publisher`.

RTMP stream, publisher, subscriber, and media totals are derived from the active catalog and return
to zero after publishers and subscribers detach. Listener accepted-connection and byte counters
are daemon-lifetime cumulative totals; active connections are a current gauge. HTTP listener
admission occurs after TCP accept and before TLS, so failed or rejected handshakes count as accepted
connections while only admitted transport lifetimes contribute to the active gauge.

Listener byte counters describe bytes visible at the owning runtime layer, not IP/TCP wire bytes.
RTMP counts protocol bytes, TCP relay totals retain every completed transfer including partial
traffic before a failure, and HTTP uses Pingora's application counters. Pingora's HTTP/1 sent
counter includes serialized response
headers, while its received counter covers request bodies; callers MUST NOT interpret these values
as protocol-independent billable octets. Prometheus exposition is implemented for the current
metric families; latency/error series, historical storage, and cross-platform host samplers remain
separate work.

`GET /api/v1/topology` returns schema version `1` and the active validated runtime generation as
immutable `nodes` and typed reference `edges`. Stable IDs are derived from canonical entity identity,
while each node and edge also carries a source `configPath`. Node kinds cover listeners, RTMP
listeners, TLS profiles, certificates, HTTP and L4 services, HTTP routes, upstream pools, and
endpoints. Listener node attributes carry the canonical tagged `bind` object and nullable
`maxConnections`; HTTP-service nodes carry nullable `maxRequestBodyBytes`; pool nodes carry
`algorithm`; and endpoint node attributes are the tagged identity itself (`type` plus `address`,
`host`/`port`, or `path`). Runtime `overlays` join listener metrics by listener name and
pool/endpoint health by pool name and canonical endpoint identity. Endpoint `activeLeases` and
listener cumulative traffic overlay values are base-10 strings. Top-level runtime state is
`active`, `starting`, or `degraded`; listener overlays use `configured`, `listening`, `stopped`, or
`failed`. Pool and endpoint overlays retain their pool/health state sets. Certificate private-key
paths are replaced by `<redacted>` and never enter the response. The topology endpoint itself is read-only. Candidate
topology is returned separately by config validation, and revision-aware editing uses the config
routes above.

RTMP listener topology attributes include application name/live/idle policy and a recording summary
containing only `supported`, `recorderCount`, `manualRecorderCount`, and
`continuousRecorderCount`. Recorder roots, suffix templates, quotas, relative output names, and
stream query arguments are not included in active or candidate topology.

## Events and event stream

`GET /api/v1/events` is implemented as bounded cursor polling over a 2,048-event in-memory ring.
It returns `events`, `cursor`, `hasMore`, and `oldestCursor`; callers can page with `after` and
`limit`. It is bearer-protected like every other recognized `/api/v1` route. `GET
/api/v1/events/stream` adds a bounded SSE delivery over the same ring; `GET /api/v1/events` with
`Accept: text/event-stream` is an equivalent negotiated form. Both forms require exactly one
management bearer token.

An SSE connection sends `event: ready` with `{"cursor":N}` before replay or live delivery. Without
a cursor, `N` is the current ring cursor and only later events are delivered. A `Last-Event-ID`
header takes precedence over `after={cursor}` and replays events with larger IDs. Operational event
frames use the ring cursor as the SSE `id`, the allowlisted operational event name as `event`, and
the existing typed JSON object as `data`. Idle connections receive `: heartbeat` every 15 seconds.
If the requested cursor has fallen out of the ring, the server sends `event: resync_required` with
`cursor`, `oldestCursor`, and `latestCursor`, then closes the stream; clients must reload status
or config and reconnect without the stale cursor. Replay pages are limited to 64 events, each
frame is bounded, and a client that cannot accept writes is closed after the bounded write timeout.
Shutdown closes a live stream with `event: shutdown`. There is no durable event history.

The event stream currently carries the existing generation and management operation events. Future
types include:

- `config.disk_changed`, `config.activated`, `config.rejected`
- `runtime.listener_changed`, `runtime.pool_health_changed`
- `import.completed`, `import.rejected`
- `certificate.expiring`, `certificate.renewed`, `certificate.failed`
- `acme.challenge_started`, `acme.challenge_completed`

Private keys, authorization values, cookies, and raw event strings are not serialized into event
frames; unknown event values are represented as `unknown`. Adding UI or ACME event publishers
requires extending the allowlists; actor identity, request correlation, durable audit retention,
and remote management remain separate follow-up work.

## Vue and Pug frontend

- Vue 3 single-page application built with Vite.
- Pug 3.x used only through `<template lang="pug">` in precompiled Vue SFCs.
- No runtime template compiler, server-supplied templates, or user-editable Pug.
- No global state library until component scope proves it necessary.
- Manually typed API client with component and exact canonical-field-registry tests.

Planned product views:

- Overview: active/disk revisions, listeners, traffic, errors, and expiring certificates.
- Services: listener, route, upstream, health, and timeout editor.
- Certificates: source, names, issuer, validity, next renewal, job history, and manual renew.
- Imports: source tree, support summary, diagnostics, and conversion preview.
- Events/logs: bounded recent operational events, not raw unbounded log streaming.

Current implementation status: the responsive runtime observatory, high-level topology schematic,
RTMP broadcast desk, and canonical configuration workspace are implemented. The observatory covers
host/process load, listener traffic, pool/endpoint health, active-stream, codec/media, viewer, and
recorder visibility. Refreshes do not overlap, retain the last valid sample after transient
failures, and expose loading/stale/error states. Manual controls call exact-ID routes and are
available for configured manual recorder definitions on active publishers. The topology inspector
exposes stable config paths and exact redacted attributes without recording roots.

The configuration workspace keeps its bearer token only in page memory, exposes every current
canonical field, validates through the server, renders the format-preserving backend preview and
candidate topology for review, and saves with `If-Config-Revision` when the root is
non-compositional. It consumes `configPreview` and `configFormat` for KDL, Lua, HOCON, and UCI, and
uses `compositional` plus `dependencyCount` to make compositional roots inspectable and
server-validatable but read-only. It preserves dirty drafts across refresh failures and `409`
 conflicts. The save response distinguishes pending activation from restart-required Unix listener
 changes; generation status is authoritative for publication completion. The UI does not yet provide
 SSE or durable event history. Certificate lifecycle management and imports remain planned views.

## File-change behavior

- The backend watches the canonical root's parent directory, debounces events, and periodically
  reconciles the effective configuration, including native references.
- The UI checks disk state on explicit load, unlock, or **Check disk revision**; it does not poll or
  receive watcher events.
- A clean explicit refresh loads an externally changed valid file.
- A dirty explicit refresh is marked stale and offers discard/reload.
- Save against a stale revision returns `409`; the server never performs last-writer-wins.
- An invalid external file remains visible with diagnostics while the last active runtime
  is clearly labeled as older.

## Security

- Management and traffic listener configuration are separate.
- Configuration routes require the file-backed bearer token described above. The current UI does
  not use cookies or persist the token.
- Recognized management/API routes, including recorder-control routes, require the bearer token
  except exact `GET /ready` and `GET /metrics`. The management listener remains loopback-only;
  recorder controls MUST gain explicit audit records before any future remote management mode
  exposes them.
- A future remote mode will require authenticated users, short sessions, audit records, TLS, and
  CSRF protection if cookie authentication is introduced.
- Certificate private-key bytes, ACME account keys, and DNS credentials never enter frontend state.
  The management bearer token is the explicit exception and is retained only in page memory.
- A future import UI will keep unsupported constructs read-only and will not save a lossy conversion
  without explicit ownership change.
- The backend rejects typed saves to compositional roots, and the browser disables editing/save
  controls when `compositional` is true. Validation remains available without flattening source
  files.

## Accessibility and responsiveness

The UI MUST be keyboard navigable, provide labeled controls and non-color-only status, and
work at mobile and desktop widths. Dense operational tables may use responsive detail
panels rather than forcing all columns into a narrow viewport.
