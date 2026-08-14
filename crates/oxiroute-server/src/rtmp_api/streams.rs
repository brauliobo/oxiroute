use std::str::FromStr;

use oxiroute_rtmp::{
    CatalogError, RecorderId, RecorderPhase, RecordingAction, RtmpControlHandle, StreamId,
};

use super::ApiResponse;
use super::dto::{RtmpCatalogResponse, RtmpRecorderResponse, RtmpStreamResponse};

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
    control: &RtmpControlHandle,
    now_unix_ms: u64,
) -> ApiResponse {
    match route {
        Route::Streams => {
            if method != "GET" {
                return ApiResponse::method_not_allowed("GET");
            }
            ApiResponse::json(
                200,
                &serde_json::to_value(RtmpCatalogResponse::project(&control.catalog_snapshot()))
                    .expect("RTMP catalog response DTO serializes"),
            )
        }
        Route::Stream(stream_id) => stream_response(method, control, stream_id),
        Route::Recording {
            stream_id,
            recorder_id,
            action,
        } => recording_response(method, control, stream_id, recorder_id, action, now_unix_ms),
    }
}

fn stream_response(method: &str, control: &RtmpControlHandle, stream_id: &str) -> ApiResponse {
    if method != "GET" {
        return ApiResponse::method_not_allowed("GET");
    }
    let Ok(stream_id) = StreamId::from_str(stream_id) else {
        return ApiResponse::error(400, "invalid_stream_id", "stream ID is invalid");
    };
    let snapshot = control.catalog_snapshot();
    snapshot
        .streams
        .iter()
        .find(|stream| stream.id == stream_id)
        .map_or_else(
            || ApiResponse::error(404, "stream_not_found", "stream does not exist"),
            |stream| {
                ApiResponse::json(
                    200,
                    &serde_json::to_value(RtmpStreamResponse::from(stream))
                        .expect("RTMP stream response DTO serializes"),
                )
            },
        )
}

fn recording_response(
    method: &str,
    control: &RtmpControlHandle,
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
        RecordingAction::Start => control.start_recording(stream_id, recorder_id, now_unix_ms),
        RecordingAction::Stop => control.stop_recording(stream_id, recorder_id, now_unix_ms),
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
            ApiResponse::json(
                status,
                &serde_json::to_value(RtmpRecorderResponse::from(&recorder))
                    .expect("RTMP recorder response DTO serializes"),
            )
        }
        Err(error) => catalog_error(
            &error,
            control.catalog_snapshot().capabilities.manual_recording,
            action,
        ),
    }
}

fn catalog_error(
    error: &CatalogError,
    manual_recording: bool,
    action: RecordingAction,
) -> ApiResponse {
    match error {
        CatalogError::AdmissionClosed => ApiResponse::error(
            409,
            "rtmp_admission_closed",
            "RTMP runtime is shutting down",
        ),
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
