# RTMP Guide

OxiRoute's current RTMP slice is a bounded live publish/play service with runtime visibility,
legacy AVC/AAC FLV recording, HLS/DASH output, bounded VOD, same-daemon auto-push, and isolated exec
profiles. It is not complete nginx-rtmp compatibility.

## Configure A Live Application

The smallest shape is a listener, an RTMP service, and one live application:

```kdl
(array)listeners {
  (object)- {
    (object)bind {
      address "127.0.0.1:1935"
      type "socket"
    }
    name "live"
    protocol "rtmp"
    service "live"
  }
}

(array)rtmp_services {
  (object)- {
    name "live"
    max_inbound_message_size 8388608
    ack_window_size 5000000
    (array)applications {
      (object)- {
        name "live"
        live #true
        idle_streams #true
        (array)recorders {
        }
      }
    }
  }
}
```

Publish to `rtmp://127.0.0.1:1935/live/<stream-name>`. Viewers use the same application and stream
name. The exact session and media behavior is defined in [RTMP_SPEC.md](../RTMP_SPEC.md).

## Use Multiple Message Streams

A single RTMP connection may create up to eight message streams. Each message-stream ID owns one
publisher or subscriber role, so one connection can publish or play several stream names without
mixing their media. Duplicate or conflicting role transitions, unknown IDs, and attempts beyond the
bound are rejected without changing the active catalog.

The client statistics response exposes the active roles as `messageStreams`, with each entry's
message-stream ID, application, stream name without query arguments, and role. Teardown removes only
the matching role and preserves the other roles on the connection.

`max_inbound_message_size` defaults to 8 MiB and bounds assembled inbound messages before payload
allocation. `ack_window_size` defaults to 5,000,000 bytes and is announced during session startup;
both values are service-wide and validated as positive bounded integers.

## Restrict Publish And Play

Publish and play policies are evaluated independently. Rules are ordered; when a policy has rules,
the first matching network decides the result and unmatched peers are denied. A stream-query token
is checked without becoming part of the stream identity:

```kdl
(object)publish {
  (array)rules {
    (object)- {
      action "deny"
      network "192.0.2.0/24"
    }
    (object)- {
      action "allow"
      network "all"
    }
  }
  (object)token {
    source "stream_query"
    parameter "token"
    secret "replace-with-a-private-value"
  }
}
(object)play {
  (object)token {
    source "stream_query"
    parameter "viewer"
    secret "replace-with-a-private-value"
  }
}
(object)limits {
  max_connections 256
  max_publishers 8
  max_viewers 1024
}
```

Place these objects inside an application. Use `?token=...` on the stream name for publish or play.
The management API redacts token secrets from typed configuration and rendered previews.

## Add Recording

Continuous and manual recorders use the same bounded store and worker pipeline. A manual recorder
starts only after an exact stream and recorder control request:

```kdl
(array)recorders {
  (object)- {
    name "archive"
    start "manual"
    root_directory "/var/lib/oxiroute/recordings"
    (object)record_mask {
      audio #true
      video #true
      keyframes #false
    }
    suffix_template "-%Y-%m-%dT%H-%M-%S.flv"
    append_unix_seconds #false
    append #false
    lock #true
    max_size 1073741824
    max_frames 1000000
    notify #true
    rotation_interval_ms 3600000
    max_queue_messages 256
    max_queue_bytes 8388608
    shutdown_timeout_ms 5000
    max_storage_bytes 10737418240
    max_storage_files 10000
    max_active_recorders 8
    retention "delete_oldest"
    retention_max_age_ms 2592000000
  }
}
```

Recordings inventory: `GET /api/v1/rtmp/recordings` lists every recording with its stream, start,
end, duration, size, codecs and status (`recording`, `complete`, or `partial` after a crash), and
`GET /api/v1/rtmp/recordings/{id}/content` downloads one with `Range` support; `DELETE` removes a
finished recording. Each recording has a hidden sidecar `.oxiroute-meta-<name>.json` next to it.
Ids are stable across restarts. See the API reference.

Storage quotas and retention: `max_storage_files` and `max_storage_bytes` bound the whole recording
root. With the default `retention "reject"`, reaching a quota fails the recorder with the specific
reason `quota_files` or `quota_bytes` (visible in `GET /api/v1/rtmp/streams` as
`phase.code`) and an `rtmp_recorder_failed` event; the continuous recorder retries on the next
media and recovers by itself as soon as a file is deleted or space is otherwise freed. With
`retention "delete_oldest"` the oldest finished recordings in the root are deleted to make room
and recording never stops; the file being written is never deleted. `retention_max_age_ms`
deletes finished recordings older than the given age whenever a recording starts. All recorders
sharing one root must use the same quota and retention settings.

`record_mask` may select audio, video, or keyframes-only video; at least one of audio/video is
required. `append` resumes the exact existing segment when its FLV tail is valid, and `lock` holds
an exclusive advisory lock while the segment is active. `max_size` and `max_frames` bound one
segment; omission means no per-segment limit. Notifications retain only the latest bounded
start/stop/failure outcome in the recorder snapshot.

## Add HLS Output

An application may publish bounded HLS output for legacy H.264/AAC streams. The worker uses a
bounded asynchronous queue, writes MPEG-TS fragments atomically, and retains only the configured
playlist/storage window. Multiple variants are metadata variants of the same transmuxed media; no
transcoding process is started.

```kdl
(object)hls {
  root_directory "/var/lib/oxiroute/hls"
  segment_duration_ms 2000
  max_segment_duration_ms 10000
  playlist_length_ms 30000
  fragment_naming "sequential"
  nested #false
  cleanup #true
  max_segment_bytes 8388608
  max_queue_messages 256
  max_storage_bytes 536870912
  max_storage_files 10000
  max_active_streams 1024
  (array)variants {
    (object)- {
      name "main"
      bandwidth 1000000
      codecs "avc1.42e01e,mp4a.40.2"
      width 1280
      height 720
    }
  }
  (object)keys {
    rotation_segments 5
    url_prefix "keys/"
  }
}
```

The authenticated management endpoint is
`/api/v1/rtmp/media/SERVICE/APPLICATION/STREAM/index.m3u8`. Multi-variant applications also expose
`master.m3u8` and variant playlists; `.ts` fragments and rotated `.bin` AES-128 keys are served
through the same bounded path. HLS output supports legacy AVC/AAC only and does not expose the
filesystem root.

Use the dashboard or the management client:

```sh
oxiroute rtmp stream list
oxiroute rtmp stream show STREAM_ID
oxiroute rtmp recorder start STREAM_ID RECORDER_ID
oxiroute rtmp recorder stop STREAM_ID RECORDER_ID
```

Start and stop transitions may return before the worker reaches its settled phase. Read the returned
snapshot or refresh the stream before issuing a conflicting action.

## Play HLS Or DASH In A Browser

A browser `<video>` element cannot send an `Authorization` header, and `?access_token=` is refused.
Mint a short-lived media ticket with the management token and give the returned URL to the player:

```sh
curl -X POST -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"ttlSeconds": 120}' \
  http://127.0.0.1:9900/api/v1/rtmp/media/SERVICE/APPLICATION/STREAM/ticket
```

```json
{
  "ticket": "1893456000000.q1w2e3...",
  "path": "/api/v1/rtmp/media/SERVICE/APPLICATION/STREAM/index.m3u8?ticket=1893456000000.q1w2e3...",
  "expiresAtUnixMs": 1893456000000,
  "ttlSeconds": 120
}
```

The body is optional; `ttlSeconds` defaults to 90 and accepts 1 through 600. The request returns
`404` when the application has no HLS or DASH output. `path` names the HLS playlist, or the DASH
manifest when only DASH is configured.

- A ticket authorizes `GET` of every object of that one stream (playlists, segments, keys, DASH
  initialization and fragments). It is refused (`401`) on any other stream, route or method, never
  replaces the management token elsewhere, and is not accepted in a header.
- Playlists and manifests served for a ticketed request carry the same `ticket` on every relative
  segment, key, map and variant reference, so a player that resolves them against the playlist URL
  needs nothing else. Responses carry `Cache-Control: no-store` and `Referrer-Policy: no-referrer`.
- A ticket is checked when each request arrives. A live view that outlasts it fails with `401
  ticket_expired`; mint a new ticket and reload before `expiresAtUnixMs`.
- Tickets are signed with a random secret generated when the daemon starts. A restart invalidates
  every outstanding ticket; a configuration reload does not.
- The ticket is never written to logs, audit records or events, and minting is audited as
  `media_ticket_mint`.

Stored VOD objects use the same mechanism. `POST
/api/v1/rtmp/vod/SERVICE/APPLICATION/SOURCE/ticket` with `{"path": "movie.flv"}` (plus an optional
`ttlSeconds`) returns a ticket and a ready `path` valid for that one object.

## Refresh Relay Destination DNS

Push and pull targets resolve during runtime planning and retain their canonical hostname. To refresh
DNS answers for relay reconnects, set the application relay interval:

```kdl
(object)relay {
  dns_refresh_ms 60000
}
```

The interval defaults to 60 seconds and accepts 1 through 300 seconds. A due refresh runs before a
connection attempt, rechecks the full answer against the RTMP outbound policy, address family, and
direct-listener loop protection, and retains the previous valid address if resolution fails. Relay
state and Prometheus expose bounded refresh attempt, success, and failure counters.

## What Is Observable

The catalog and monitoring API expose publisher/subscriber identity per active message stream, codec
observations, byte and fanout totals, recorder phase, relative current/completed/recoverable names,
segment counters, discontinuities, relay state, and bounded client `messageStreams` snapshots. They
do not expose recording roots, stream query arguments, or private material.

A stream whose publisher left stays listed as `offline` in `recently_ended_streams` for one hour (at
most 64 streams, cleared on restart), with `last_seen_at_unix_ms` and an `end_reason` (`closed`,
`dropped`, `stale`, `shutdown`, `failed`). `media.profile` shows resolution, frame rate, bitrates,
keyframe interval and audio format, and `subscriber_count` shows the viewers. Stream ids stay the
same across reconnects and restarts.

The dashboard disables manual controls when there is no publisher, no manual recorder, a transition
is in progress, or an observed codec is not recordable.

## Add Bounded VOD

An application may expose named local or HTTP VOD sources. RTMP playback uses the
`source/path.flv` stream name; the authenticated management API serves the same object at
`/api/v1/rtmp/vod/SERVICE/APPLICATION/SOURCE/path.flv` and accepts one byte range.

```kdl
(object)vod {
  max_sessions 64
  max_file_bytes 67108864
  max_duration_ms 21600000
  (array)sources {
    (object)- {
      type "local"
      name "archive"
      root_directory "/var/lib/oxiroute/recordings"
    }
    (object)- {
      type "http"
      name "origin"
      origin "https://media.example.test/library"
    }
  }
}
```

Local roots use pinned no-follow descriptors. HTTP origins must use HTTP or HTTPS, contain no
credentials/query/fragment, resolve through the service outbound policy, and follow at most three
redirects. File size, duration, active sessions, and range count are bounded; multiple ranges and
chunked upstream responses are rejected.

## Current Boundaries

- Legacy AVC video and AAC audio are recordable. Enhanced AVC, HEVC, and AV1 can be observed/fanned
  out but fail recording with an explicit unsupported-codec state.
- Enhanced RTMP video (`avc1`, `hvc1`, `av01` FourCC) is accepted from publishers, including the
  `colorInfo` metadata packet that ffmpeg sends ahead of the first keyframe, and fanned out as
  received. Other enhanced packet types (sequence end, MPEG-2 TS, multitrack, ModEx, command frames)
  end the publish with an `unsupported enhanced video packet type` error instead of being dropped
  silently.
- Publisher fanout is bounded per subscriber; saturated viewers resynchronize instead of growing an
  unbounded queue.
- Continuous recording and exact-ID manual controls are integrated. Authenticated remote recorder
  administration and cross-process quota coordination are not.
- Same-daemon auto-push is available only through authenticated owner-only Unix workers; it does not
  create an arbitrary RTMP endpoint or forward peer media recursively. Isolated command/transcode
  profiles are bounded and allowlisted; broader nginx exec directive parity is not.
- DASH output is available for validated AVC/AAC publishers. It writes bounded fragmented MP4
  segments and an MPD under the configured media quota; malformed or unsupported codec forms fail
  closed with no MPEG-TS masquerading as DASH. Native nginx-DASH lowering, richer callback fields,
  native control/callback parity and complete directive lowering remain future slices. Named
  local/HTTP VOD sources, bounded RTMP playback workers, and the
  authenticated management range endpoint are integrated. Native `allow`/`deny` and application `max_connections` have bounded
  lowering; canonical token rules and publisher/viewer ceilings are canonical-only.
- An RTMP parser accepting a directive does not mean the runtime enforces that directive. The
  compatibility registry reports enforced and disable-only forms separately from parsed-only,
  source-no-op, source-bug, deprecated, and platform-limited forms.
