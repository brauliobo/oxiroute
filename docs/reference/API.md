# API Reference

The current control plane is JSON over loopback HTTP. The base path for management routes is
`/api/v1`. Exact errors, response fields, and security caveats are normative in
[API_UI_SPEC.md](../API_UI_SPEC.md).

## Route Groups

| Group | Routes | Auth/current role |
| --- | --- | --- |
| Configuration | `GET /api/v1/config`, `POST /api/v1/config/validate`, `PUT /api/v1/config` | Management bearer token; typed drafts, preflight, revision-safe write |
| Runtime | `GET /api/v1/status`, `/listeners`, `/pools`, `/servers`, `/generations` | Loopback and route-specific bearer rules |
| Observability | `GET /api/v1/monitoring`, `/topology` | Loopback management view; returns redacted active state |
| RTMP | `GET /api/v1/rtmp/streams`, `/streams/{streamId}` | Loopback catalog; exact recorder controls are POST routes |
| RTMP controls | `POST .../recorders/{recorderId}/start|stop` | Loopback-only current control boundary |
| Generation actions | `POST /api/v1/generations/reload|rollback|drain` | Revision-checked management operation |
| DNS | `POST /api/v1/servers/refresh-dns` | Validated target batch; explicit non-atomic outcomes |
| Statistics | `GET /ready`, `GET /metrics`, `GET /stats`, `POST /stats/admin` | Public probes; restricted reads/mutations as configured |

Native import is intentionally CLI/offline only. There is no import API, UI workflow, or unbounded
event stream in the current contract.

## Authentication

Authenticated routes require exactly one `Authorization: Bearer <token>` header. The token is loaded
from a restrictive regular file, hashed at startup, and compared without exposing its bytes in
responses. Configuration `PUT` also requires one raw `If-Config-Revision` header.

Recorder control currently has a separate loopback-only boundary and does not consume the management
token. This is not a remote-management authorization model.

## Configuration Write Flow

```text
GET config -> edit typed JSON -> POST validate -> review preview/candidate topology -> PUT with disk revision
```

`PUT` performs complete preflight before durable mutation. Outcomes distinguish a saved candidate
pending activation, an unchanged active generation, and a valid save that requires restart. A stale
revision returns `409` and does not perform a last-writer-wins write.

## Response Rules

- Cumulative `u64` values are decimal strings; gauges and bounded counts are JSON numbers.
- Timestamps use RFC 3339 or explicitly named Unix-millisecond fields.
- Topology is an active-generation graph with stable IDs, canonical config paths, and runtime overlays.
- Sensitive paths and secret material are omitted or redacted.
- A `503` sampling or canonical-state error is not replaced with fabricated zeroes.

## Example Calls

```sh
TOKEN=$(tr -d '\r\n' < /tmp/oxiroute-management.token)
curl -s http://127.0.0.1:9080/api/v1/monitoring | jq '.listeners, .upstreamPools'
curl -s -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:9080/api/v1/config | jq '{diskRevision, activeRevision, configFormat, compositional}'
```

Avoid putting the token in shell history in long-lived environments; use the installed client or a
process-safe secret mechanism instead.
