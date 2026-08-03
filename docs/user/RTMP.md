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

## Add Recording

Continuous and manual recorders use the same bounded store and worker pipeline. A manual recorder
starts only after an exact stream and recorder control request:

```kdl
(array)recorders {
  (object)- {
    name "archive"
    start "manual"
    root_directory "/var/lib/oxiroute/recordings"
    suffix_template "-%Y-%m-%dT%H-%M-%S.flv"
    append_unix_seconds #false
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
- HLS, DASH, VOD, callbacks, broad access/control parity, isolated exec, and complete directive
  lowering remain future slices.
- An RTMP parser accepting a directive does not mean the runtime enforces that directive.
