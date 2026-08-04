# API Reference

The current control plane is JSON over loopback HTTP. The base path for management routes is
`/api/v1`. Exact errors, response fields, and security caveats are normative in
[API_UI_SPEC.md](../API_UI_SPEC.md).

## Route Groups

| Group | Routes | Auth/current role |
| --- | --- | --- |
| Configuration | `GET /api/v1/config`, `POST /api/v1/config/validate`, `PUT /api/v1/config` | Management bearer token; typed drafts, preflight, revision-safe write |
| Observability | `GET /api/v1/monitoring`, `/topology`, `/status` | Management bearer token; redacted active state |
| Inventory | `GET /api/v1/listeners`, `/pools`, `/servers`, `/generations`, `/tls` | Management bearer token |
| Listener/pool/server actions | `POST /api/v1/listeners/administrative-state`, `/pools/administrative-state`, `/servers/administrative-state`, `/servers/health-override`, `/servers/checks`; `PUT /api/v1/servers/max-connections` | Management bearer token and active-generation revision |
| DNS | `POST /api/v1/servers/refresh-dns` | Management bearer token; validated target batch and explicit non-atomic outcomes |
| Generation actions | `POST /api/v1/generations/reload|rollback|drain` | Management bearer token and active-generation revision |
| TLS/process | `POST /api/v1/tls/reconcile`, `/process/drain`, `/process/shutdown` | Management bearer token and active-generation revision |
| Audit | `GET /api/v1/audit?after={cursor}&limit={n}`, `GET /api/v1/audit/status` | Management bearer token; durable redacted history and persistence status |
| Events | `GET /api/v1/events?after={cursor}&limit={n}`, `GET /api/v1/events/stream` | Management bearer token; bounded cursor polling or SSE |
| RTMP | `GET /api/v1/rtmp/streams`, `/streams/{streamId}` | Management bearer token; redacted active catalog |
| RTMP controls | `POST .../recorders/{recorderId}/start|stop` | Management bearer token; loopback management listener and exact-ID manual controls |
| RTMP VOD | `GET /api/v1/rtmp/vod/{service}/{application}/{source}/{path}` | Management bearer token; one contiguous byte range and bounded source/session policy |
| Statistics | `GET /ready`, `GET /metrics`, `GET /stats`, `POST /stats/admin` | Exact `GET /ready` and `GET /metrics` are public; restricted statistics reads/mutations use loopback plus the statistics token/revision |

Every recognized `/api/v1` route requires exactly one management Bearer token. The only public
recognized API probes are exact `GET /ready` and `GET /metrics`. Separately configured
`stats.pages[]` listeners are public page-only contracts with their own loopback same-origin form
policy; they are not remote management routes.

Native import is intentionally CLI/offline or compositional-source only. There is no import API or
import UI workflow. Event SSE is bounded, bearer-authenticated, cursor-based, and backed only by
the in-memory ring; it is not durable audit storage. Audit history is queried separately through
`/api/v1/audit` and never serves as an SSE fallback.

## Authentication

Authenticated routes require exactly one `Authorization: Bearer <token>` header. The token is loaded
from a restrictive regular file, hashed at startup, and compared without exposing its bytes in
responses. Configuration `PUT` also requires one raw `If-Config-Revision` header.

Recorder control is served on the authenticated loopback management listener and consumes the same
management token as the other recognized `/api/v1` routes. This is not a remote-management
authorization model.

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
- Management responses return `X-Correlation-ID`; a valid caller-supplied value is retained within
  the 64-byte safe-character bound, otherwise the service generates one.
- A `503` sampling or canonical-state error is not replaced with fabricated zeroes.

## Example Calls

```sh
TOKEN=$(tr -d '\r\n' < /tmp/oxiroute-management.token)
curl -s -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:9080/api/v1/monitoring | jq '.listeners, .upstreamPools'
curl -s -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:9080/api/v1/config | jq '{diskRevision, activeRevision, configFormat, compositional}'
```

Avoid putting the token in shell history in long-lived environments; use the installed client or a
process-safe secret mechanism instead.
