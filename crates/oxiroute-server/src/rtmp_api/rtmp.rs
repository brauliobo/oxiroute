use std::str::FromStr;

use http::header::HeaderName;
use oxiroute_rtmp::{
    RtmpCatalogSnapshot, RtmpRegistry, RtmpSessionControlAction, RtmpSessionControlError,
    SessionId, StreamSnapshot,
};
use pingora::protocols::http::ServerSession;
use serde_json::{Value, json};

use super::ApiResponse;
use crate::secure_bearer::{HeaderCardinality, single_header};

const MAX_STATS_STREAMS: usize = 1_024;
const MAX_STATS_CLIENTS: usize = 1_024;
const IF_RTMP_SESSION_REVISION: HeaderName = HeaderName::from_static("if-rtmp-session-revision");

#[derive(Clone, Copy)]
pub(super) enum Route<'a> {
    Stats(StatsView),
    Drop {
        session_id: &'a str,
        action: RtmpSessionControlAction,
    },
}

#[derive(Clone, Copy)]
pub(super) enum StatsView {
    All,
    Global,
    Live,
    Clients,
}

pub(super) fn match_route(path: &str) -> Option<Route<'_>> {
    match path {
        "/api/v1/rtmp/stats" => return Some(Route::Stats(StatsView::All)),
        "/api/v1/rtmp/stats/global" => return Some(Route::Stats(StatsView::Global)),
        "/api/v1/rtmp/stats/live" => return Some(Route::Stats(StatsView::Live)),
        "/api/v1/rtmp/stats/clients" => return Some(Route::Stats(StatsView::Clients)),
        _ => {}
    }

    let mut segments = path.strip_prefix("/api/v1/rtmp/clients/")?.split('/');
    let session_id = segments.next()?;
    match (segments.next(), segments.next(), segments.next()) {
        (Some("drop"), None, None) if !session_id.is_empty() => Some(Route::Drop {
            session_id,
            action: RtmpSessionControlAction::Client,
        }),
        (Some("publisher"), Some("drop"), None) if !session_id.is_empty() => Some(Route::Drop {
            session_id,
            action: RtmpSessionControlAction::Publisher,
        }),
        (Some("subscriber"), Some("drop"), None) if !session_id.is_empty() => Some(Route::Drop {
            session_id,
            action: RtmpSessionControlAction::Subscriber,
        }),
        _ => None,
    }
}

pub(super) fn handle(
    route: Route<'_>,
    method: &str,
    registry: &RtmpRegistry,
    session: Option<&ServerSession>,
) -> ApiResponse {
    match route {
        Route::Stats(view) if method == "GET" => stats_response(view, registry),
        Route::Stats(_) => ApiResponse::method_not_allowed("GET"),
        Route::Drop { .. } if method == "POST" => session
            .map_or_else(ApiResponse::unauthorized, |session| {
                drop_response(route, registry, session)
            }),
        Route::Drop { .. } => ApiResponse::method_not_allowed("POST"),
    }
}

fn stats_response(view: StatsView, registry: &RtmpRegistry) -> ApiResponse {
    let snapshot = registry.snapshot();
    let clients = registry
        .session_snapshots()
        .into_iter()
        .filter(|client| client.connected)
        .collect::<Vec<_>>();
    let streams_truncated = snapshot.streams.len() > MAX_STATS_STREAMS;
    let clients_truncated = clients.len() > MAX_STATS_CLIENTS;
    let global = global_json(&snapshot);
    let live = snapshot
        .streams
        .iter()
        .take(MAX_STATS_STREAMS)
        .map(stream_json)
        .collect::<Vec<_>>();
    let clients = clients
        .iter()
        .take(MAX_STATS_CLIENTS)
        .map(client_json)
        .collect::<Vec<_>>();
    let common = json!({
        "revision": snapshot.revision.to_string(),
        "asOfUnixMs": snapshot.as_of_unix_ms,
    });
    let body = match view {
        StatsView::All => json!({
            "revision": common["revision"],
            "asOfUnixMs": common["asOfUnixMs"],
            "global": global,
            "live": live,
            "clients": clients,
            "liveTruncated": streams_truncated,
            "clientsTruncated": clients_truncated,
        }),
        StatsView::Global => json!({
            "revision": common["revision"],
            "asOfUnixMs": common["asOfUnixMs"],
            "global": global,
        }),
        StatsView::Live => json!({
            "revision": common["revision"],
            "asOfUnixMs": common["asOfUnixMs"],
            "live": live,
            "truncated": streams_truncated,
        }),
        StatsView::Clients => json!({
            "revision": common["revision"],
            "asOfUnixMs": common["asOfUnixMs"],
            "clients": clients,
            "truncated": clients_truncated,
        }),
    };
    ApiResponse::json(200, &body)
}

fn global_json(snapshot: &RtmpCatalogSnapshot) -> Value {
    let mut publishers = 0_u64;
    let mut subscribers = 0_u64;
    let mut audio_payload_bytes = 0_u64;
    let mut video_payload_bytes = 0_u64;
    for stream in &snapshot.streams {
        publishers = publishers.saturating_add(u64::from(stream.publisher.is_some()));
        subscribers =
            subscribers.saturating_add(u64::try_from(stream.subscriber_count).unwrap_or(u64::MAX));
        audio_payload_bytes =
            audio_payload_bytes.saturating_add(stream.media.audio.payload_bytes_received);
        video_payload_bytes =
            video_payload_bytes.saturating_add(stream.media.video.payload_bytes_received);
    }
    json!({
        "activeStreams": snapshot.streams.len(),
        "publishers": publishers,
        "subscribers": subscribers,
        "audioPayloadBytes": audio_payload_bytes.to_string(),
        "videoPayloadBytes": video_payload_bytes.to_string(),
        "liveIngest": snapshot.capabilities.live_ingest,
        "manualRecording": snapshot.capabilities.manual_recording,
    })
}

fn stream_json(stream: &StreamSnapshot) -> Value {
    json!({
        "id": stream.id.to_string(),
        "service": stream.key.server_id,
        "application": stream.key.application,
        "name": stream.key.name,
        "createdAtUnixMs": stream.created_at_unix_ms,
        "publisherSessionId": stream.publisher.map(|publisher| publisher.session_id.to_string()),
        "subscriberCount": stream.subscriber_count,
        "audioPayloadBytes": stream.media.audio.payload_bytes_received.to_string(),
        "videoPayloadBytes": stream.media.video.payload_bytes_received.to_string(),
    })
}

fn client_json(client: &oxiroute_rtmp::RtmpClientSnapshot) -> Value {
    json!({
        "id": client.session_id.to_string(),
        "service": client.service_id,
        "peerIp": client.peer_addr.map(|address| address.to_string()),
        "connectedAtUnixMs": client.connected_at_unix_ms,
        "application": client.application,
        "stream": client.stream_name,
        "role": client.role.as_str(),
        "revision": client.revision.to_string(),
    })
}

fn drop_response(
    route: Route<'_>,
    registry: &RtmpRegistry,
    session: &ServerSession,
) -> ApiResponse {
    let Route::Drop {
        session_id: session_id_text,
        action,
    } = route
    else {
        unreachable!("drop response requires a drop route")
    };
    let session_id = match SessionId::from_str(session_id_text) {
        Ok(session_id) => session_id,
        Err(_) => return ApiResponse::error(400, "invalid_session_id", "session ID is invalid"),
    };
    let expected_revision =
        match single_header(&session.req_header().headers, &IF_RTMP_SESSION_REVISION) {
            HeaderCardinality::Missing => {
                return ApiResponse::error(
                    428,
                    "precondition_required",
                    "If-Rtmp-Session-Revision is required",
                );
            }
            HeaderCardinality::Duplicate => {
                return ApiResponse::error(
                    400,
                    "duplicate_session_revision",
                    "multiple RTMP session revisions are not accepted",
                );
            }
            HeaderCardinality::Single(value) => {
                match value.to_str().ok().and_then(|value| value.parse().ok()) {
                    Some(revision) => revision,
                    None => {
                        return ApiResponse::error(
                            400,
                            "invalid_session_revision",
                            "If-Rtmp-Session-Revision must be a nonnegative decimal integer",
                        );
                    }
                }
            }
        };
    match registry.request_session_control(session_id, action, expected_revision) {
        Ok(outcome) => ApiResponse::json(
            202,
            &json!({
                "outcome": if outcome.already_requested { "already_requested" } else { "requested" },
                "sessionId": session_id.to_string(),
                "target": action.as_str(),
                "sessionRevision": expected_revision.to_string(),
            }),
        ),
        Err(RtmpSessionControlError::NotFound) => {
            ApiResponse::error(404, "session_not_found", "RTMP session does not exist")
        }
        Err(RtmpSessionControlError::RevisionMismatch { actual, .. }) => ApiResponse::json(
            409,
            &json!({
                "error": {
                    "code": "session_revision_conflict",
                    "message": "RTMP session revision changed",
                },
                "actualRevision": actual,
            }),
        ),
        Err(RtmpSessionControlError::RoleMismatch { actual, .. }) => ApiResponse::json(
            409,
            &json!({
                "error": {
                    "code": "session_role_conflict",
                    "message": "RTMP session role does not match the requested target",
                },
                "actualRole": actual.as_str(),
            }),
        ),
        Err(RtmpSessionControlError::AlreadyPending) => ApiResponse::error(
            409,
            "session_control_pending",
            "another RTMP session control request is pending",
        ),
    }
}
