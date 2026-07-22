use std::{str::FromStr, sync::Arc, time::SystemTime};

use async_trait::async_trait;
use http::{
    header::{ALLOW, CONTENT_LENGTH, CONTENT_TYPE},
    HeaderValue, Response, StatusCode,
};
use oxiroute_rtmp::{
    CatalogError, RecorderId, RecorderPhase, RecorderSnapshot, RecordingAction,
    RtmpCatalogSnapshot, RtmpRegistry, StreamId, StreamSnapshot, TrackSnapshot,
};
use pingora::{apps::http_app::ServeHttp, protocols::http::ServerSession};
use serde_json::{json, Value};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApiResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub allow: Option<&'static str>,
}

impl ApiResponse {
    fn json(status: u16, value: &Value) -> Self {
        Self {
            status,
            body: value.to_string().into_bytes(),
            allow: None,
        }
    }

    fn error(status: u16, code: &'static str, message: impl Into<String>) -> Self {
        let value = json!({
            "error": {
                "code": code,
                "message": message.into(),
            }
        });
        Self::json(status, &value)
    }

    fn method_not_allowed(allow: &'static str) -> Self {
        let mut response = Self::error(405, "method_not_allowed", "method is not allowed");
        response.allow = Some(allow);
        response
    }
}

pub struct RtmpManagementApi {
    registry: Arc<RtmpRegistry>,
}

impl RtmpManagementApi {
    #[must_use]
    pub fn new(registry: Arc<RtmpRegistry>) -> Self {
        Self { registry }
    }

    #[must_use]
    pub fn handle(&self, method: &str, path: &str, now_unix_ms: u64) -> ApiResponse {
        let segments: Vec<_> = path
            .trim_matches('/')
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect();

        match segments.as_slice() {
            ["api", "v1", "rtmp", "streams"] => {
                if method != "GET" {
                    return ApiResponse::method_not_allowed("GET");
                }
                ApiResponse::json(200, &catalog_json(&self.registry.snapshot()))
            }
            ["api", "v1", "rtmp", "streams", stream_id] => {
                if method != "GET" {
                    return ApiResponse::method_not_allowed("GET");
                }
                let Ok(stream_id) = StreamId::from_str(stream_id) else {
                    return ApiResponse::error(400, "invalid_stream_id", "stream ID is invalid");
                };
                let snapshot = self.registry.snapshot();
                snapshot
                    .streams
                    .iter()
                    .find(|stream| stream.id == stream_id)
                    .map_or_else(
                        || ApiResponse::error(404, "stream_not_found", "stream does not exist"),
                        |stream| ApiResponse::json(200, &stream_json(stream)),
                    )
            }
            ["api", "v1", "rtmp", "streams", stream_id, "recorders", recorder_id, action] => {
                self.handle_recording(method, stream_id, recorder_id, action, now_unix_ms)
            }
            _ => ApiResponse::error(404, "route_not_found", "route does not exist"),
        }
    }

    fn handle_recording(
        &self,
        method: &str,
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
            _ => return ApiResponse::error(404, "route_not_found", "route does not exist"),
        };

        match self
            .registry
            .request_recording(stream_id, recorder_id, action, now_unix_ms)
        {
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
            Err(error) => catalog_error(&error),
        }
    }
}

#[async_trait]
impl ServeHttp for RtmpManagementApi {
    async fn response(&self, session: &mut ServerSession) -> Response<Vec<u8>> {
        let request = session.req_header();
        let response = match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
            Ok(duration) => match u64::try_from(duration.as_millis()) {
                Ok(now_unix_ms) => {
                    self.handle(request.method.as_str(), request.uri.path(), now_unix_ms)
                }
                Err(_) => ApiResponse::error(
                    500,
                    "system_clock_invalid",
                    "system clock is outside the supported range",
                ),
            },
            Err(_) => ApiResponse::error(
                500,
                "system_clock_invalid",
                "system clock predates the Unix epoch",
            ),
        };

        to_http_response(response)
    }
}

fn to_http_response(response: ApiResponse) -> Response<Vec<u8>> {
    let mut result = Response::new(response.body);
    *result.status_mut() =
        StatusCode::from_u16(response.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    result
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let content_length = HeaderValue::from_str(&result.body().len().to_string())
        .expect("decimal content length is a valid header");
    result.headers_mut().insert(CONTENT_LENGTH, content_length);
    if let Some(allow) = response.allow {
        result
            .headers_mut()
            .insert(ALLOW, HeaderValue::from_static(allow));
    }
    result
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
        "recorders": stream.recorders.iter().map(recorder_json).collect::<Vec<_>>(),
    })
}

fn track_json(track: &TrackSnapshot) -> Value {
    json!({
        "codec_id": track.flv_codec_id,
        "codec_name": track.flv_codec_id.and_then(codec_name),
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
            "code": format!("{code:?}").to_ascii_lowercase(),
        }),
    }
}

fn codec_name(codec_id: u8) -> Option<&'static str> {
    match codec_id {
        7 => Some("avc"),
        10 => Some("aac"),
        _ => None,
    }
}

fn catalog_error(error: &CatalogError) -> ApiResponse {
    match error {
        CatalogError::RecordingUnavailable => {
            ApiResponse::error(501, "rtmp_recording_not_implemented", error.to_string())
        }
        CatalogError::StreamNotFound(_) | CatalogError::RecorderNotFound { .. } => {
            ApiResponse::error(404, "rtmp_resource_not_found", error.to_string())
        }
        CatalogError::NoPublisher(_)
        | CatalogError::RecorderNotManual(_)
        | CatalogError::TransitionInProgress(_)
        | CatalogError::StaleOperation
        | CatalogError::InvalidCompletion
        | CatalogError::PublisherAlreadyAttached { .. }
        | CatalogError::PublisherMismatch { .. }
        | CatalogError::SubscriberNotFound { .. } => {
            ApiResponse::error(409, "rtmp_state_conflict", error.to_string())
        }
    }
}
