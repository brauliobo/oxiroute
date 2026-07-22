# Management API and UI specification

## Deployment boundary

The control plane shares the daemon initially but remains logically separate from traffic
listeners. It binds to loopback by default. Remote access requires explicit TLS and
authentication configuration; permissive CORS is never a default.

## API conventions

- Base path: `/api/v1`.
- JSON request and response bodies.
- RFC 3339 UTC timestamps.
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
| `GET` | `/metrics` | Prometheus exposition on a separately configurable listener. |

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
