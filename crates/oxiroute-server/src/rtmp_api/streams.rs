use std::str::FromStr;

use oxiroute_rtmp::{
    CatalogError, RecorderId, RecorderPhase, RecorderSnapshot, RecordingAction, RelaySnapshot,
    RtmpCatalogSnapshot, RtmpRegistry, StreamId, StreamSnapshot, TrackSnapshot,
    VideoCodecIdentifier,
};
use serde_json::{Value, json};

use super::ApiResponse;

#[derive(Clone, Copy)]
pub(super) enum Route<'a> {
    Streams,
    Stream(&'a str),
    Recording {
        stream_id: &'a str,
        recorder_id: &'a str,
        action: &'a str,
    },
}

pub(super) fn match_route(path: &str) -> Option<Route<'_>> {
    if path == "/api/v1/rtmp/streams" {
        return Some(Route::Streams);
    }

    let mut segments = path.strip_prefix("/api/v1/rtmp/streams/")?.split('/');
    let stream_id = segments.next()?;
    match (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) {
        (None, None, None, None) if !stream_id.is_empty() => Some(Route::Stream(stream_id)),
        (Some("recorders"), Some(recorder_id), Some(action), None)
            if !stream_id.is_empty() && !recorder_id.is_empty() && !action.is_empty() =>
        {
            Some(Route::Recording {
                stream_id,
                recorder_id,
                action,
            })
        }
        _ => None,
    }
}

pub(super) fn handle(
    route: Route<'_>,
    method: &str,
    registry: &RtmpRegistry,
    now_unix_ms: u64,
) -> ApiResponse {
    match route {
        Route::Streams => {
            if method != "GET" {
                return ApiResponse::method_not_allowed("GET");
            }
            ApiResponse::json(200, &catalog_json(&registry.snapshot()))
        }
        Route::Stream(stream_id) => stream_response(method, registry, stream_id),
        Route::Recording {
            stream_id,
            recorder_id,
            action,
        } => recording_response(
            method,
            registry,
            stream_id,
            recorder_id,
            action,
            now_unix_ms,
        ),
    }
}

fn stream_response(method: &str, registry: &RtmpRegistry, stream_id: &str) -> ApiResponse {
    if method != "GET" {
        return ApiResponse::method_not_allowed("GET");
    }
    let Ok(stream_id) = StreamId::from_str(stream_id) else {
        return ApiResponse::error(400, "invalid_stream_id", "stream ID is invalid");
    };
    let snapshot = registry.snapshot();
    snapshot
        .streams
        .iter()
        .find(|stream| stream.id == stream_id)
        .map_or_else(
            || ApiResponse::error(404, "stream_not_found", "stream does not exist"),
            |stream| ApiResponse::json(200, &stream_json(stream)),
        )
}

fn recording_response(
    method: &str,
    registry: &RtmpRegistry,
    stream_id: &str,
    recorder_id: &str,
    action: &str,
    now_unix_ms: u64,
) -> ApiResponse {
    if method != "POST" {
        return ApiResponse::method_not_allowed("POST");
    }
    let Ok(stream_id) = StreamId::from_str(stream_id) else {
        return ApiResponse::error(400, "invalid_stream_id", "stream ID is invalid");
    };
    let Ok(recorder_id) = RecorderId::from_str(recorder_id) else {
        return ApiResponse::error(400, "invalid_recorder_id", "recorder ID is invalid");
    };
    let action = match action {
        "start" => RecordingAction::Start,
        "stop" => RecordingAction::Stop,
        _ => return ApiResponse::route_not_found(),
    };

    let result = match action {
        RecordingAction::Start => registry.start_recording(stream_id, recorder_id, now_unix_ms),
        RecordingAction::Stop => registry.stop_recording(stream_id, recorder_id, now_unix_ms),
    };
    match result {
        Ok(recorder) => {
            let status = if matches!(
                recorder.phase,
                RecorderPhase::Starting { .. } | RecorderPhase::Stopping { .. }
            ) {
                202
            } else {
                200
            };
            ApiResponse::json(status, &recorder_json(&recorder))
        }
        Err(error) => catalog_error(
            &error,
            registry.snapshot().capabilities.manual_recording,
            action,
        ),
    }
}

fn catalog_json(snapshot: &RtmpCatalogSnapshot) -> Value {
    json!({
        "revision": snapshot.revision.to_string(),
        "as_of_unix_ms": snapshot.as_of_unix_ms,
        "capabilities": {
            "live_ingest": snapshot.capabilities.live_ingest,
            "manual_recording": snapshot.capabilities.manual_recording,
        },
        "streams": snapshot.streams.iter().map(stream_json).collect::<Vec<_>>(),
    })
}

fn stream_json(stream: &StreamSnapshot) -> Value {
    json!({
        "id": stream.id.to_string(),
        "revision": stream.revision.to_string(),
        "server_id": stream.key.server_id,
        "application": stream.key.application,
        "name": stream.key.name,
        "created_at_unix_ms": stream.created_at_unix_ms,
        "publisher": stream.publisher.map(|publisher| json!({
            "session_id": publisher.session_id.to_string(),
            "attached_at_unix_ms": publisher.attached_at_unix_ms,
        })),
        "subscriber_count": stream.subscriber_count,
        "media": {
            "audio": track_json(&stream.media.audio),
            "video": track_json(&stream.media.video),
            "fanout_payload_bytes": stream.media.fanout_payload_bytes_queued.to_string(),
        },
        "relays": stream.relays.iter().map(relay_json).collect::<Vec<_>>(),
        "recording_supported": !stream.recorders.is_empty(),
        "manual_recording": stream.recorders.iter().any(|recorder| recorder.manual),
        "recorders": stream.recorders.iter().map(recorder_json).collect::<Vec<_>>(),
    })
}

fn relay_json(relay: &RelaySnapshot) -> Value {
    json!({
        "id": relay.id.to_string(),
        "destination": {
            "address": relay.status.destination.address.to_string(),
            "application": relay.status.destination.application,
            "stream_name": relay.status.destination.stream_name,
        },
        "phase": relay_phase(relay.status.phase),
        "last_failure": relay.status.last_failure.map(relay_failure),
        "queue_messages": relay.status.queue_messages,
        "queue_bytes": relay.status.queue_bytes.to_string(),
        "connection_attempts": relay.status.connection_attempts.to_string(),
        "connections": relay.status.connections.to_string(),
        "reconnects": relay.status.reconnects.to_string(),
        "events_enqueued": relay.status.events_enqueued.to_string(),
        "events_sent": relay.status.events_sent.to_string(),
        "events_dropped": relay.status.events_dropped.to_string(),
        "payload_bytes_sent": relay.status.payload_bytes_sent.to_string(),
    })
}

const fn relay_phase(phase: oxiroute_rtmp::RtmpRelayPhase) -> &'static str {
    match phase {
        oxiroute_rtmp::RtmpRelayPhase::Connecting => "connecting",
        oxiroute_rtmp::RtmpRelayPhase::Publishing => "publishing",
        oxiroute_rtmp::RtmpRelayPhase::Backoff => "backoff",
        oxiroute_rtmp::RtmpRelayPhase::Stopped => "stopped",
    }
}

const fn relay_failure(failure: oxiroute_rtmp::RtmpRelayFailure) -> &'static str {
    match failure {
        oxiroute_rtmp::RtmpRelayFailure::Connect => "connect",
        oxiroute_rtmp::RtmpRelayFailure::Handshake => "handshake",
        oxiroute_rtmp::RtmpRelayFailure::Session => "session",
        oxiroute_rtmp::RtmpRelayFailure::Transport => "transport",
        oxiroute_rtmp::RtmpRelayFailure::Thread => "thread",
    }
}

fn track_json(track: &TrackSnapshot) -> Value {
    let codec_id = track
        .video_codec
        .and_then(VideoCodecIdentifier::flv_codec_id)
        .or(track.flv_codec_id);
    let codec_fourcc = track.video_codec.and_then(VideoCodecIdentifier::four_cc);
    let codec_name = track
        .video_codec
        .and_then(video_codec_name)
        .or_else(|| codec_id.and_then(flv_codec_name));
    let recording_supported = track.video_codec.map_or_else(
        || matches!(track.flv_codec_id, Some(7 | 10)),
        VideoCodecIdentifier::recording_supported,
    );
    json!({
        "codec_id": codec_id,
        "codec_fourcc": codec_fourcc.map(|four_cc| String::from_utf8_lossy(&four_cc).into_owned()),
        "codec_name": codec_name,
        "recording_supported": recording_supported,
        "payload_bytes": track.payload_bytes_received.to_string(),
        "last_rtmp_timestamp_ms": track.last_rtmp_timestamp_ms,
        "last_observed_at_unix_ms": track.last_observed_at_unix_ms,
    })
}

fn recorder_json(recorder: &RecorderSnapshot) -> Value {
    json!({
        "id": recorder.id.to_string(),
        "name": recorder.name,
        "manual": recorder.manual,
        "phase": phase_json(recorder.phase),
        "changed_at_unix_ms": recorder.changed_at_unix_ms,
        "bytes_written": recorder.bytes_written.to_string(),
        "current_relative_name": recorder.current_relative_name,
        "last_completed_relative_name": recorder.last_completed_relative_name,
        "recoverable_partial_name": recorder.recoverable_partial_name,
        "published_but_not_durable_relative_name": recorder.published_but_not_durable_relative_name,
        "segments_started": recorder.segments_started.to_string(),
        "segments_completed": recorder.segments_completed.to_string(),
        "discontinuities": recorder.discontinuities.to_string(),
    })
}

fn phase_json(phase: RecorderPhase) -> Value {
    match phase {
        RecorderPhase::Idle => json!({ "state": "idle" }),
        RecorderPhase::Starting { operation_id } => json!({
            "state": "starting",
            "operation_id": operation_id.to_string(),
        }),
        RecorderPhase::Recording {
            operation_id,
            started_at_unix_ms,
        } => json!({
            "state": "recording",
            "operation_id": operation_id.to_string(),
            "started_at_unix_ms": started_at_unix_ms,
        }),
        RecorderPhase::Stopping { operation_id } => json!({
            "state": "stopping",
            "operation_id": operation_id.to_string(),
        }),
        RecorderPhase::Failed { operation_id, code } => json!({
            "state": "failed",
            "operation_id": operation_id.to_string(),
            "code": recorder_error_code(code),
        }),
    }
}

const fn recorder_error_code(code: oxiroute_rtmp::RecorderErrorCode) -> &'static str {
    match code {
        oxiroute_rtmp::RecorderErrorCode::OpenFailed => "open_failed",
        oxiroute_rtmp::RecorderErrorCode::WriteFailed => "write_failed",
        oxiroute_rtmp::RecorderErrorCode::CloseFailed => "close_failed",
        oxiroute_rtmp::RecorderErrorCode::BackendUnavailable => "backend_unavailable",
        oxiroute_rtmp::RecorderErrorCode::FileSyncFailed => "file_sync_failed",
        oxiroute_rtmp::RecorderErrorCode::PublishFailed => "publish_failed",
        oxiroute_rtmp::RecorderErrorCode::DirectorySyncFailed => "directory_sync_failed",
        oxiroute_rtmp::RecorderErrorCode::QueueDiscontinuity => "queue_discontinuity",
        oxiroute_rtmp::RecorderErrorCode::UnsupportedCodec => "unsupported_codec",
        oxiroute_rtmp::RecorderErrorCode::ShutdownTimedOut => "shutdown_timed_out",
        oxiroute_rtmp::RecorderErrorCode::WorkerPanicked => "worker_panicked",
        oxiroute_rtmp::RecorderErrorCode::StalePublisher => "stale_publisher",
    }
}

fn flv_codec_name(codec_id: u8) -> Option<&'static str> {
    match codec_id {
        7 => Some("avc"),
        10 => Some("aac"),
        _ => None,
    }
}

fn video_codec_name(codec: VideoCodecIdentifier) -> Option<&'static str> {
    match codec {
        VideoCodecIdentifier::Flv(codec_id) => flv_codec_name(codec_id),
        VideoCodecIdentifier::FourCc(four_cc) if four_cc == *b"avc1" => Some("avc"),
        VideoCodecIdentifier::FourCc(four_cc) if four_cc == *b"hvc1" => Some("hevc"),
        VideoCodecIdentifier::FourCc(four_cc) if four_cc == *b"av01" => Some("av1"),
        VideoCodecIdentifier::FourCc(_) => None,
    }
}

fn catalog_error(
    error: &CatalogError,
    manual_recording: bool,
    action: RecordingAction,
) -> ApiResponse {
    match error {
        CatalogError::RecordingUnavailable if manual_recording => recorder_failure(action),
        CatalogError::RecordingUnavailable => ApiResponse::error(
            501,
            "rtmp_recording_unavailable",
            "manual recording is unavailable in the active runtime",
        ),
        CatalogError::StreamNotFound(_) | CatalogError::RecorderNotFound { .. } => {
            ApiResponse::error(
                404,
                "rtmp_resource_not_found",
                "RTMP resource does not exist",
            )
        }
        CatalogError::RecorderFailed { .. } => recorder_failure(action),
        CatalogError::NoPublisher(_)
        | CatalogError::RecorderNotManual(_)
        | CatalogError::TransitionInProgress(_)
        | CatalogError::StaleOperation
        | CatalogError::InvalidCompletion
        | CatalogError::PublisherAlreadyAttached { .. }
        | CatalogError::PublisherMismatch { .. }
        | CatalogError::SubscriberNotFound { .. } => ApiResponse::error(
            409,
            "rtmp_state_conflict",
            "the requested recorder transition conflicts with current state",
        ),
    }
}

fn recorder_failure(action: RecordingAction) -> ApiResponse {
    match action {
        RecordingAction::Start => ApiResponse::error(
            503,
            "rtmp_recorder_start_failed",
            "the recorder could not be started",
        ),
        RecordingAction::Stop => ApiResponse::error(
            503,
            "rtmp_recorder_stop_failed",
            "the recorder could not be stopped",
        ),
    }
}
