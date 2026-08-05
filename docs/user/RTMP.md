# RTMP Guide

OxiRoute's current RTMP slice is a bounded live publish/play service with runtime visibility,
legacy AVC/AAC FLV recording, and HLS transmuxing. It is not complete nginx-rtmp compatibility.

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
  }
}
```

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

## What Is Observable

The catalog and monitoring API expose publisher/subscriber identity, codec observations, byte and
fanout totals, recorder phase, relative current/completed/recoverable names, segment counters,
discontinuities, and relay state. They do not expose recording roots, stream query arguments, or
private material.

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
- Publisher fanout is bounded per subscriber; saturated viewers resynchronize instead of growing an
  unbounded queue.
- Continuous recording and exact-ID manual controls are integrated. Authenticated remote recorder
  administration and cross-process quota coordination are not.
- DASH output is available for validated AVC/AAC publishers. It writes bounded fragmented MP4
  segments and an MPD under the configured media quota; malformed or unsupported codec forms fail
  closed with no MPEG-TS masquerading as DASH. Native nginx-DASH lowering, richer callback fields,
  broad control parity, isolated exec, and complete directive lowering remain future slices. Named
  local/HTTP VOD sources, bounded RTMP playback workers, and the
  authenticated management range endpoint are integrated. Native `allow`/`deny` and application `max_connections` have bounded
  lowering; canonical token rules and publisher/viewer ceilings are canonical-only.
- An RTMP parser accepting a directive does not mean the runtime enforces that directive. The
  compatibility registry reports enforced and disable-only forms separately from parsed-only,
  source-no-op, source-bug, deprecated, and platform-limited forms.
