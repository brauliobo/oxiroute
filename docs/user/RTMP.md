# RTMP Guide

OxiRoute's current RTMP slice is a bounded live publish/play service with runtime visibility and
legacy AVC/AAC FLV recording. It is not complete nginx-rtmp compatibility.

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
`record_mask` may select audio, video, or keyframes-only video; at least one of audio/video is
required. `append` resumes the exact existing segment when its FLV tail is valid, and `lock` holds
an exclusive advisory lock while the segment is active. `max_size` and `max_frames` bound one
segment; omission means no per-segment limit. Notifications retain only the latest bounded
start/stop/failure outcome in the recorder snapshot.
    max_queue_messages 256
    max_queue_bytes 8388608
    shutdown_timeout_ms 5000
    max_storage_bytes 10737418240
    max_storage_files 10000
    max_active_recorders 8
  }
}
```

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

## Current Boundaries

- Legacy AVC video and AAC audio are recordable. Enhanced AVC, HEVC, and AV1 can be observed/fanned
  out but fail recording with an explicit unsupported-codec state.
- Publisher fanout is bounded per subscriber; saturated viewers resynchronize instead of growing an
  unbounded queue.
- Continuous recording and exact-ID manual controls are integrated. Authenticated remote recorder
  administration and cross-process quota coordination are not.
- HLS, DASH, VOD, callbacks, broad control parity, isolated exec, and complete directive lowering
  remain future slices. Native `allow`/`deny` and application `max_connections` have bounded
  lowering; canonical token rules and publisher/viewer ceilings are canonical-only.
- An RTMP parser accepting a directive does not mean the runtime enforces that directive. The
  compatibility registry reports enforced and disable-only forms separately from parsed-only,
  source-no-op, source-bug, deprecated, and platform-limited forms.
