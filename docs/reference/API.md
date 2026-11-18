# API Reference

The current control plane is JSON over loopback HTTP. Management routes remain under `/api/v1`, with
the corrected event-only contract under `/api/v2/events`. Exact errors, response fields, and security caveats are normative in
[API_UI_SPEC.md](../API_UI_SPEC.md).

The checked [OpenAPI 3.1 control-plane artifact](../../contracts/control-plane.openapi.json) currently
covers the six protected read-only status, listener, pool, server, generation, and in-flight request operations and the two RTMP media-ticket mint operations. Its
freshness gate is `scripts/verify/control-plane-contract.sh`; routes outside that registry remain
documented by this page and the normative API/UI specification until their DTO and endpoint metadata
are migrated.

## Route Groups

| Group | Routes | Auth/current role |
| --- | --- | --- |
| Configuration | `GET /api/v1/config`, `POST /api/v1/config/validate`, `PUT /api/v1/config` | Management bearer token; typed drafts, preflight, revision-safe write |
| Native import reports | `GET /api/v1/import-reports`, `GET /api/v1/import-reports/{index}` | Management bearer token; bounded redacted evidence and read-only finalized KDL preview |
| Observability | `GET /api/v1/monitoring`, `/topology`, `/status`, `/stats` | Management bearer token; redacted active state; `/stats` is the stats page snapshot (see "Stats page API" in OPERATIONS.md) |
| Inventory | `GET /api/v1/listeners`, `/pools`, `/servers`, `/generations`, `/tls` | Management bearer token |
| Listener/pool/server actions | `POST /api/v1/listeners/administrative-state`, `/pools/administrative-state`, `/servers/administrative-state`, `/servers/health-override`, `/servers/checks`; `PUT /api/v1/servers/max-connections`, `PUT /api/v1/servers/weight` | Management bearer token and active-generation revision |
| Evacuate | `POST /api/v1/servers/evacuate`, `POST /api/v1/pools/evacuate` | Management bearer token; drain plus a one-shot cancel and re-dispatch of in-flight HTTP requests (see below) |
| DNS | `POST /api/v1/servers/refresh-dns` | Management bearer token; validated target batch and explicit non-atomic outcomes |
| Generation actions | `POST /api/v1/generations/reload|rollback|drain` | Management bearer token and active-generation revision |
| TLS/process | `POST /api/v1/tls/reconcile`, `/process/drain`, `/process/shutdown` | Management bearer token and active-generation revision |
| Managed ACME actions | `POST /api/v1/tls/renew`, `/tls/revoke`, `/tls/delete`, `/tls/account/rollover` | Management bearer token and active-generation revision; failed actions retain active material |
| Managed ACME jobs | `POST /api/v1/tls/jobs/cancel`, `/tls/jobs/pause`, `/tls/jobs/resume` | Management bearer token and active-generation revision; cooperative controls with redacted job IDs |
| Audit | `GET /api/v1/audit?after={cursor}&limit={n}`, `GET /api/v1/audit/status` | Management bearer token; durable redacted history and persistence status |
| Events v1 | `GET /api/v1/events?after={cursor}&limit={n}`, `GET /api/v1/events/stream` | Management bearer token; shipped 0.4.1 polling/SSE shape |
| Events v2 | `GET /api/v2/events?after={cursor}&limit={n}`, `GET /api/v2/events/stream` | Management bearer token; authoritative latest cursor and corrected vocabulary |
| RTMP | `GET /api/v1/rtmp/streams`, `/streams/{streamId}` | Management bearer token; redacted active catalog |
| RTMP controls | `POST .../recorders/{recorderId}/start|stop` | Management bearer token; loopback management listener and exact-ID manual controls |
| RTMP recordings | `GET /api/v1/rtmp/recordings`, `GET\|DELETE /api/v1/rtmp/recordings/{recordingId}`, `GET /api/v1/rtmp/recordings/{recordingId}/content` | Management bearer token; recording inventory, metadata, ranged download and audited delete |
| RTMP VOD | `GET /api/v1/rtmp/vod/{service}/{application}/{source}/{path}` | Management bearer token; one contiguous byte range and bounded source/session policy |
| RTMP HLS/DASH media | `GET /api/v1/rtmp/media/{service}/{application}/{stream}/{object}` | Management bearer token; bounded HLS playlists/fragments or DASH `manifest.mpd`, `init.mp4`, and `.m4s` objects; one contiguous byte range |
| RTMP media tickets | `POST /api/v1/rtmp/media/{service}/{application}/{stream}/ticket`, `POST /api/v1/rtmp/vod/{service}/{application}/{source}/ticket` | Management bearer token; mints `{ticket, path, expiresAtUnixMs, ttlSeconds}` (`ttlSeconds` 1-600, default 90). The ticket is accepted as `?ticket=` only on `GET` of the matching media/VOD object without an `Authorization` header; in the OpenAPI artifact as `createMediaTicket` and `createVodTicket` (see RTMP_SPEC.md) |
| In-flight requests | `GET /api/v1/requests?pool=&route=&phase=&min_age_ms=&limit=&requestId=` | Management bearer token; live requests oldest first (default limit 100, at most 1000) with `total` and `truncated`; in the OpenAPI artifact as `getRequests` |
| Statistics | `GET /ready`, `GET /metrics`, `GET /status.json`, `GET /stats`, `POST /stats/admin` | Exact `GET /ready`, `GET /metrics` and `GET /status.json` are public; restricted statistics reads/mutations use loopback plus the statistics token/revision |

Every recognized `/api/v1` or `/api/v2` route requires exactly one management Bearer token. The only public
recognized API probes are exact `GET /ready`, `GET /metrics` and `GET /status.json`. Separately configured
`stats.pages[]` listeners are public page-only contracts with their own loopback same-origin form
policy; they are not remote management routes.

Native source files remain CLI/offline or compositional-source inputs. The authenticated import-report
routes and provenance workspace expose retained evidence only; they never rewrite native files or
activate standalone reports. The audit routes are browsed by the separate authenticated audit
workspace and never use the event ring as a fallback. Event SSE is bounded, bearer-authenticated,
cursor-based, and backed only by
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

Config responses redact RTMP token secrets and callback URL userinfo and query values as
`<redacted>`. The redacted body is a valid `PUT`/validate request: each placeholder is replaced by
the stored value of the same RTMP service, application and field before preflight, so secrets are
preserved and never echoed. A placeholder with no matching stored value, or a callback URL whose
non-redacted parts changed, returns `422` `redacted_rtmp_token_unresolved` or
`redacted_rtmp_callback_unresolved` and writes nothing. To change a secret, send the new literal
value; `<redacted>` itself cannot be stored as a secret.

## Response Rules

- Cumulative `u64` values are decimal strings; gauges and bounded counts are JSON numbers.
- Timestamps use RFC 3339 or explicitly named Unix-millisecond fields.
- Topology is an active-generation graph with stable IDs, canonical config paths, and runtime overlays.
- Sensitive paths and secret material are omitted or redacted.
- ACME action responses contain only certificate names, categorical outcomes, job IDs, and disk/active
  revisions where applicable. Account keys, account/order URLs, JWS bodies, challenge tokens, DNS TXT
  values, and private certificate material are never returned.
- Management responses return `X-Correlation-ID`; a valid caller-supplied value is retained within
  the 64-byte safe-character bound, otherwise the service generates one.
- A `503` sampling or canonical-state error is not replaced with fabricated zeroes.

## RTMP Stream Catalog

`GET /api/v1/rtmp/streams` returns `streams` (streams that currently have a publisher, a viewer, or a
recorder) and `recently_ended_streams` (streams whose publisher left within the last hour, at most 64,
newest last). `GET /api/v1/rtmp/streams/{streamId}` returns one entry of `streams`.

A stream `id` is derived from the service id, application and stream name, so it is the same after a
reconnect or a restart. `subscriber_count` is the number of viewers.

Every `streams` entry carries `state` (`live` with a publisher, `idle` without one) and
`media.profile`:

```json
{
  "state": "live",
  "subscriber_count": 3,
  "media": {
    "profile": {
      "video_width": 1280,
      "video_height": 720,
      "frame_rate_millihertz": 30000,
      "video_bitrate_bps": 2500000,
      "audio_bitrate_bps": 128000,
      "keyframe_interval_ms": 2000,
      "audio_sample_rate_hz": 44100,
      "audio_channels": 2
    }
  }
}
```

Width and height come from the publisher's metadata. Frame rate and bitrates are measured over the
publisher's media once at least one second has been seen and fall back to the advertised values before
that. `keyframe_interval_ms` is the latest measured distance between keyframes. Sample rate and
channels come from the AAC configuration, else from metadata. A value that is not known is `null`.
Codecs are in `media.audio` and `media.video`.

A `recently_ended_streams` entry has `state: "offline"`, `id`, `server_id`, `application`, `name`,
`publisher_session_id`, `created_at_unix_ms`, `published_at_unix_ms`, `last_seen_at_unix_ms` (latest
media or metadata), `ended_at_unix_ms`, `media` as above (final values), and `end_reason`:

| `end_reason` | Meaning |
| --- | --- |
| `closed` | the publisher unpublished or closed its connection |
| `dropped` | an operator dropped the session |
| `stale` | the publisher stopped sending media |
| `shutdown` | the runtime was stopping |
| `failed` | a transport, protocol or runtime error ended the session |

The ended list is memory-only: it is empty after a restart, and an entry disappears when the stream
publishes again.

### RTMP event details

RTMP events in `/api/v2/events` and `/api/v2/events/stream` (and the version 1 shapes of the
existing `rtmp_*` events) carry an optional `rtmp` object:

```json
{
  "event": "rtmp_recorder_failed",
  "outcome": "failed",
  "rtmp": {
    "service": "edge",
    "application": "live",
    "stream": "camera",
    "streamId": "2a130dea-5db7-43e0-afb8-f07c4bcb1814",
    "sessionId": "5b9c6a52-4f55-4d62-9d4c-3a1f7a4e5c10",
    "role": "publisher",
    "reason": "quota_files",
    "recorder": "archive"
  }
}
```

`streamId` is the catalog stream id. `reason` is the failure code for `failed` outcomes. New v2
events: `rtmp_publish_end` (`reason` is `closed`, `dropped`, `stale`, `shutdown` or `failed`),
`rtmp_recorder_started` (`reason: "recovered"` after a failure), `rtmp_recorder_stopped` and
`rtmp_recorder_failed`. Version 1 shows them as `unknown`. A publisher that ends normally is
`rtmp_publish_end` with outcome `applied`; `rtmp_publish` with outcome `failed` is only a real error.

## RTMP Recordings

The recording inventory covers every recorder `root_directory` of the active generation. A recording
id is a stable opaque 24-character hex string derived from the configured root and the file name, so
it survives restarts; a file deleted and recreated under the same name keeps its id.

`GET /api/v1/rtmp/recordings?stream=&recorder=&status=&limit=&cursor=` returns, newest first:

```json
{
  "recordings": [{
    "id": "0123456789abcdef01234567",
    "name": "camera-1.flv",
    "status": "complete",
    "stream": "camera",
    "started_at_unix_ms": 1721657969000,
    "ended_at_unix_ms": 1721657999000,
    "duration_ms": 30000,
    "size_bytes": "1048576",
    "modified_at_unix_ms": 1721657999000,
    "video_codec": "avc",
    "audio_codec": "aac",
    "recorders": [{"service": "live", "application": "cam", "recorder": "archive"}]
  }],
  "next_cursor": null,
  "roots": [{
    "recorders": [{"service": "live", "application": "cam", "recorder": "archive"}],
    "bytes_used": "1048576", "max_bytes": "10737418240",
    "files": 1, "max_files": 10000,
    "retention": "reject", "retention_max_age_ms": null,
    "evicted_files": "0", "evicted_bytes": "0"
  }]
}
```

- `status` is `recording` while a recorder of this process writes the file, `complete` once the
  recorder finalized it, and `partial` when a recorder was cut short (crash, failed recorder) and no
  recorder owns it any more. Files without a metadata sidecar (written before sidecars existed) are
  `complete` when their FLV tail is intact and `partial` otherwise.
- `stream` is the published stream name without its query string or key. `duration_ms`, the codecs
  and the start time come from a sidecar `.oxiroute-meta-<name>.json` the recorder writes next to
  each recording; they are `null` for files that have none. `recorders` lists the recorders that
  share the recording's root.
- `limit` is 1 through 1000 (default 100). A `next_cursor` is returned while more recordings match;
  pass it back as `cursor`. Unknown query parameters are a `400`.
- `GET /api/v1/rtmp/recordings/{recordingId}` returns one recording object; `404 recording_not_found`.
- `GET .../{recordingId}/content` streams the file (`video/x-flv`) with `Accept-Ranges: bytes`,
  one contiguous `Range`, `Content-Disposition: attachment` and `Cache-Control: no-store`; an
  unsatisfiable range is `416`.
- `DELETE .../{recordingId}` removes the recording and its sidecar and returns `{"deleted": {...}}`;
  a recording being written is refused with `409 recording_active`. Every delete is audited as
  `recording_delete`.

## Example Calls

```sh
TOKEN=$(tr -d '\r\n' < /tmp/oxiroute-management.token)
curl -s -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:9080/api/v1/monitoring | jq '.listeners, .upstreamPools'
curl -s -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:9080/api/v1/config | jq '{diskRevision, activeRevision, configFormat, compositional}'
```

All managed TLS mutations also require a JSON `expectedActiveRevision` field. Renewal and action
requests accept a `certificate` name; revocation additionally accepts an optional numeric `reason`.
Delete removes only persisted managed ACME state and returns `409 certificate_in_use` while an active
TLS profile still references the certificate. Every mutation returns `X-Correlation-ID`, and the same
safe correlation is persisted on the bounded job record and audit event.

## Evacuate

`POST /api/v1/servers/evacuate` and `POST /api/v1/pools/evacuate` drain servers like the administrative-state
routes (same persistence across generations by pool and server name, dynamic servers through the registry
operator state; `ready` through `/servers/administrative-state` un-drains). The servers route also cancels the
in-flight HTTP requests of each server that can be served again by another server of its pool. The pools route
drains every server of each pool first, so no server is left to take a moved request: by default it only drains
and reports the running requests as `no_destination`, and `force` cuts them. Both use the management bearer
token and, like every other mutation, are not served on the registry sockets or the public status socket (`401` on the management bind
without the token), and are audited as `server_evacuate` and `pool_evacuate` (category `control`).

```sh
curl -s -X POST -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"targets":[{"pool":"gpu-render","server":"node-a"}],"force":false,"expectedActiveRevision":"..."}' \
  http://127.0.0.1:9080/api/v1/servers/evacuate
curl -s -X POST -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"pools":["gpu-render","voice-tts"],"force":false,"expectedActiveRevision":"..."}' \
  http://127.0.0.1:9080/api/v1/pools/evacuate
```

| Field | Route | Meaning |
| --- | --- | --- |
| `targets` | servers | non-empty list of exact `{pool, server}` pairs |
| `pools` | pools | non-empty list of pool names; every server of each pool |
| `force` | both | default `false`; also cancel requests that cannot move |
| `expectedActiveRevision` | both | required, checked like the other mutations (`409` `generation_conflict` when stale) |

Unknown fields are rejected (`400`), as is an empty or oversized batch (`400` `invalid_batch`, 1 to 256 targets).
All targets are validated before anything changes: an unknown pool or server answers `404`
(`pool_not_found`, `server_not_found`) and leaves the batch unapplied. `GET` answers `405`. Success is `200`:

```json
{ "outcome": "evacuated", "force": false, "cancelledAndRedispatched": 3, "forced": 0,
  "leftRunning": { "response_started": 1, "not_replayable": 1 } }
```

`cancelledAndRedispatched` requests were cancelled and sent to another server; `forced` were cancelled
without a retry (only with `force`); `leftRunning` counts the others by reason: `response_started`,
`not_replayable`, `body_not_replayable`, `upgraded`, `limit_reached`, `no_destination` (absent reasons
are zero). Counts are per request, summed over every target of the call; each target also publishes a
`server_evacuation` event with its own counts in `/api/v2/events` (version 1 shows it as `unknown`; `server` is
absent in the event of a pool). Only HTTP reverse proxy
requests are counted; other traffic is drain only. A forced request whose response had not started gets
`503` with `x-oxiroute-reason: evacuated`. The rules and limits are in the
[runbook](../user/RUNBOOK.md#evacuate-a-server).

Avoid putting the token in shell history in long-lived environments; use the installed client or a
process-safe secret mechanism instead.
