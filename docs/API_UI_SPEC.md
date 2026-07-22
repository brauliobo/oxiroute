# Management API and UI specification

## Deployment boundary

The control plane shares the daemon initially but remains logically separate from traffic
listeners. It binds to loopback by default. Remote access requires explicit TLS and
authentication configuration; permissive CORS is never a default.

## API conventions

- Base path: `/api/v1`.
- JSON request and response bodies.
- RFC 3339 UTC timestamps for future configuration resources; existing runtime and monitoring
  fields explicitly suffixed `UnixMs` or `_unix_ms` use Unix-millisecond numbers.
- Stable machine-readable error codes plus human-readable details.
- Secret values are write-only references or redacted placeholders.
- Configuration writes use `If-Match` with the current disk revision.

Initial endpoints:

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/api/v1/config` | Typed config, disk revision, active revision, and diagnostics. |
| `POST` | `/api/v1/config/validate` | Validate a typed draft without writing or activating it. |
| `PUT` | `/api/v1/config` | Revision-checked canonical save and activation attempt. |
| `GET` | `/api/v1/status` | Daemon, generation, listener, pool, and certificate summary. |
| `GET` | `/api/v1/events` | Server-sent events for revision, runtime, import, health, and certificate changes. |
| `GET` | `/api/v1/certificates` | Redacted certificate inventory and expiry state. |
| `POST` | `/api/v1/certificates/{name}/renew` | Queue an operator-requested renewal. |
| `POST` | `/api/v1/imports/validate` | Parse native sources and return a report without activation. |
| `GET` | `/api/v1/monitoring` | Runtime process, host load, listener traffic, pool/endpoint health, and RTMP activity snapshot. |
| `GET` | `/metrics` | Prometheus exposition on a separately configurable listener. |

The implemented monitoring snapshot contains daemon uptime, process CPU/RSS/virtual memory,
threads, open file descriptors, host load averages and memory, aggregate/listener connection and
byte counters, configured per-listener connection capacities, pool/endpoint health, and RTMP
stream/publisher/subscriber/media totals. Process and host sampling
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
`unexpected_status`, `protocol_error`, or `null`. To preserve every `u64` exactly in JavaScript,
`unavailableSelections`, `successfulChecks`, `failedChecks`, `consecutiveSuccesses`, and
`consecutiveFailures` are base-10 integer strings. Availability and total endpoint counts remain
JSON numbers because configuration bounds them. `unavailableSelections` counts selection attempts
made while the pool had no selectable endpoint.

RTMP stream, publisher, subscriber, and media totals are derived from the active catalog and return
to zero after publishers and subscribers detach. Listener accepted-connection and byte counters
are daemon-lifetime cumulative totals; active connections are a current gauge.

Listener byte counters describe bytes visible at the owning runtime layer, not IP/TCP wire bytes.
RTMP counts protocol bytes, TCP relay totals retain every completed transfer including partial
traffic before a failure, and HTTP uses Pingora's application counters. Pingora's HTTP/1 sent
counter includes serialized response
headers, while its received counter covers request bodies; callers MUST NOT interpret these values
as protocol-independent billable octets. Prometheus exposition, latency/error series, history, and
cross-platform host samplers remain separate work.

`PUT /config` outcomes:

- `200`: disk and active revisions both moved to the submitted generation.
- `202`: disk write succeeded, but an explicitly asynchronous preparation job remains.
- `409`: expected revision was stale; no write occurred.
- `422`: candidate validation failed; no write occurred.
- `503`: runtime preparation failed; disk revision may differ, prior active revision remains.

The response always includes both revisions and diagnostics so the UI cannot imply that a
disk write automatically changed live traffic.

## Event stream

Events have an increasing daemon-local ID and include current revisions. Types include:

- `config.disk_changed`, `config.activated`, `config.rejected`
- `runtime.listener_changed`, `runtime.pool_health_changed`
- `import.completed`, `import.rejected`
- `certificate.expiring`, `certificate.renewed`, `certificate.failed`
- `acme.challenge_started`, `acme.challenge_completed`

Clients reconnect with `Last-Event-ID`. If history is unavailable, the server emits a
`resync_required` event and the client reloads status/config.

## Vue and Pug frontend

- Vue 3 single-page application built with Vite.
- Pug 3.x used only through `<template lang="pug">` in precompiled Vue SFCs.
- No runtime template compiler, server-supplied templates, or user-editable Pug.
- No global state library until component scope proves it necessary.
- Typed API client generated from or checked against the API schema.

Initial views:

- Overview: active/disk revisions, listeners, traffic, errors, and expiring certificates.
- Services: listener, route, upstream, health, and timeout editor.
- Certificates: source, names, issuer, validity, next renewal, job history, and manual renew.
- Imports: source tree, support summary, diagnostics, and conversion preview.
- Events/logs: bounded recent operational events, not raw unbounded log streaming.

Current implementation status: the responsive runtime observatory and RTMP broadcast desk are
implemented with host/process load, listener traffic, pool/endpoint health, active-stream,
codec/media, viewer, and recorder visibility. Pool cards show the algorithm, available/total
endpoints, exact unavailable-selection count, and each endpoint's address, state, last-check age,
total and consecutive passed/failed checks, and latest failure. Transition timestamps are available
through the API but are not currently rendered. Refreshes do not overlap, retain the last valid
sample after transient failures, and expose loading/stale/error states. Manual controls call
exact-ID routes and remain disabled when the API reports no recording backend. Configuration,
certificate, import, and event views remain planned.

## File-change behavior

- Backend watches parent directories and hashes complete file content after debouncing.
- A clean UI draft automatically reloads an externally changed valid file.
- A dirty draft is marked stale and offers discard/reload or explicit reconciliation.
- Save against a stale revision returns `409`; the server never performs last-writer-wins.
- An invalid external file remains visible with diagnostics while the last active runtime
  is clearly labeled as older.

## Security

- Management and traffic listener configuration are separate.
- State-changing endpoints require same-origin credentials and CSRF protection when cookie authentication is used.
- Remote mode requires authenticated users, short sessions, audit records, and TLS.
- Private keys, ACME account keys, DNS credentials, raw authorization headers, and secret values never enter frontend state.
- UI status is read-only for unsupported imported constructs; it cannot save a lossy conversion without explicit ownership change.

## Accessibility and responsiveness

The UI MUST be keyboard navigable, provide labeled controls and non-color-only status, and
work at mobile and desktop widths. Dense operational tables may use responsive detail
panels rather than forcing all columns into a narrow viewport.
