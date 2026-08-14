use std::str::FromStr;

use http::header::HeaderName;
use oxiroute_rtmp::{
    RtmpControlHandle, RtmpSessionControlAction, RtmpSessionControlError, SessionId,
};
use pingora::protocols::http::ServerSession;

use super::ApiResponse;
use super::dto::{
    RtmpClientStatsResponse, RtmpGlobalStatsResponse, RtmpLiveStatsResponse,
    RtmpSessionControlResponse, RtmpSessionRevisionConflictResponse,
    RtmpSessionRoleConflictResponse, RtmpStatsResponse,
};
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
    control: &RtmpControlHandle,
    session: Option<&ServerSession>,
) -> ApiResponse {
    match route {
        Route::Stats(view) if method == "GET" => stats_response(view, control),
        Route::Stats(_) => ApiResponse::method_not_allowed("GET"),
        Route::Drop { .. } if method == "POST" => session
            .map_or_else(ApiResponse::unauthorized, |session| {
                drop_response(route, control, session)
            }),
        Route::Drop { .. } => ApiResponse::method_not_allowed("POST"),
    }
}

fn stats_response(view: StatsView, control: &RtmpControlHandle) -> ApiResponse {
    let snapshot = control.catalog_snapshot();
    let clients = control
        .session_snapshots()
        .into_iter()
        .filter(|client| client.connected)
        .collect::<Vec<_>>();
    let body = match view {
        StatsView::All => serde_json::to_value(RtmpStatsResponse::project(
            &snapshot,
            &clients,
            MAX_STATS_STREAMS,
            MAX_STATS_CLIENTS,
        )),
        StatsView::Global => serde_json::to_value(RtmpGlobalStatsResponse::project(&snapshot)),
        StatsView::Live => {
            serde_json::to_value(RtmpLiveStatsResponse::project(&snapshot, MAX_STATS_STREAMS))
        }
        StatsView::Clients => serde_json::to_value(RtmpClientStatsResponse::project(
            &snapshot,
            &clients,
            MAX_STATS_CLIENTS,
        )),
    };
    ApiResponse::json(200, &body.expect("RTMP stats response DTO serializes"))
}

fn drop_response(
    route: Route<'_>,
    control: &RtmpControlHandle,
    session: &ServerSession,
) -> ApiResponse {
    let Route::Drop {
        session_id: session_id_text,
        action,
    } = route
    else {
        unreachable!("drop response requires a drop route")
    };
    let Ok(session_id) = SessionId::from_str(session_id_text) else {
        return ApiResponse::error(400, "invalid_session_id", "session ID is invalid");
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
    match control.request_session_control(session_id, action, expected_revision) {
        Ok(outcome) => ApiResponse::json(
            202,
            &serde_json::to_value(RtmpSessionControlResponse::project(
                outcome.already_requested,
                &session_id,
                action,
                expected_revision,
            ))
            .expect("RTMP session control response DTO serializes"),
        ),
        Err(RtmpSessionControlError::NotFound) => {
            ApiResponse::error(404, "session_not_found", "RTMP session does not exist")
        }
        Err(RtmpSessionControlError::RevisionMismatch { actual, .. }) => ApiResponse::json(
            409,
            &serde_json::to_value(RtmpSessionRevisionConflictResponse::new(actual))
                .expect("RTMP session revision conflict DTO serializes"),
        ),
        Err(RtmpSessionControlError::RoleMismatch { actual, .. }) => ApiResponse::json(
            409,
            &serde_json::to_value(RtmpSessionRoleConflictResponse::new(actual))
                .expect("RTMP session role conflict DTO serializes"),
        ),
        Err(RtmpSessionControlError::AlreadyPending) => ApiResponse::error(
            409,
            "session_control_pending",
            "another RTMP session control request is pending",
        ),
    }
}
