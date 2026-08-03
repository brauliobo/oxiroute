use std::{
    fmt::Write as _,
    io,
    path::Path,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

use super::{
    ApiResponse,
    config::{self, ConfigApiState, Route as ConfigRoute},
    management::{self, ManagementState},
    observability::{self, Route as ObservabilityRoute},
    response::{system_time_ms, to_http_response},
    streams::{self, Route as StreamRoute},
    ui::UiAssets,
};
use crate::{
    GenerationManager, RuntimeMetrics, TopologySnapshot,
    config_coordinator::{CanonicalConfigCoordinator, ConfigRevision},
    secure_bearer::{HeaderCardinality, single_header},
};
use async_trait::async_trait;
use bytes::Bytes;
use http::{
    Response,
    header::{ACCEPT, AUTHORIZATION, CACHE_CONTROL, HeaderName, TRANSFER_ENCODING},
};
use oxiroute_rtmp::RtmpRegistry;
use pingora::{
    apps::{HttpPersistentSettings, HttpServerApp, ReusedHttpStream, http_app::ServeHttp},
    http::ResponseHeader,
    protocols::http::ServerSession,
    server::ShutdownWatch,
};
use tokio::time::{Instant, MissedTickBehavior, interval_at};

const EVENT_STREAM_BATCH_LIMIT: usize = 64;
const EVENT_STREAM_FRAME_LIMIT: usize = 16 * 1024;
const EVENT_STREAM_HEARTBEAT: Duration = Duration::from_secs(15);
const EVENT_STREAM_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const LAST_EVENT_ID: HeaderName = HeaderName::from_static("last-event-id");

enum ApiRoute<'a> {
    Config(ConfigRoute),
    Observability(ObservabilityRoute),
    Stream(StreamRoute<'a>),
}

pub struct RtmpManagementApi {
    config: Option<ConfigApiState>,
    coordinator: Option<CanonicalConfigCoordinator>,
    generations: Option<GenerationManager>,
    management: Option<ManagementState>,
    metrics: RuntimeMetrics,
    registry: Arc<RtmpRegistry>,
    topology: Arc<TopologySnapshot>,
    ui: Option<UiAssets>,
}

pub struct RtmpManagementHttpApp {
    api: RtmpManagementApi,
}

impl RtmpManagementApi {
    #[must_use]
    pub fn new(
        registry: Arc<RtmpRegistry>,
        metrics: RuntimeMetrics,
        topology: Arc<TopologySnapshot>,
    ) -> Self {
        Self {
            config: None,
            coordinator: None,
            generations: None,
            management: None,
            metrics,
            registry,
            topology,
            ui: None,
        }
    }

    /// Loads a prebuilt Vue application into the management service.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when `index.html` or an asset cannot be read at startup.
    pub fn with_ui_dir(
        registry: Arc<RtmpRegistry>,
        metrics: RuntimeMetrics,
        topology: Arc<TopologySnapshot>,
        directory: impl AsRef<Path>,
    ) -> io::Result<Self> {
        Ok(Self {
            config: None,
            coordinator: None,
            generations: None,
            management: None,
            metrics,
            registry,
            topology,
            ui: Some(UiAssets::load(directory.as_ref())?),
        })
    }

    #[must_use]
    pub fn into_http_app(self) -> RtmpManagementHttpApp {
        RtmpManagementHttpApp { api: self }
    }

    #[must_use]
    pub fn with_generation_manager(mut self, generations: GenerationManager) -> Self {
        if let Some(config) = &mut self.config {
            config.set_generation_manager(generations.clone());
        }
        if let Some(coordinator) = &self.coordinator {
            self.management = Some(ManagementState::new(
                coordinator.clone(),
                generations.clone(),
                self.metrics.clone(),
            ));
        }
        self.generations = Some(generations);
        self
    }

    #[must_use]
    pub fn with_process_shutdown(mut self, shutdown: Arc<AtomicBool>) -> Self {
        if let Some(management) = &mut self.management {
            management.set_process_shutdown(shutdown);
        }
        self
    }

    /// Enables authenticated configuration routes with an injected token.
    ///
    /// # Errors
    ///
    /// Returns an error when the token does not meet the management-token policy.
    pub fn with_config_coordinator(
        mut self,
        coordinator: CanonicalConfigCoordinator,
        active_revision: ConfigRevision,
        token: &str,
    ) -> io::Result<Self> {
        self.coordinator = Some(coordinator.clone());
        self.config = Some(ConfigApiState::new(coordinator, active_revision, token)?);
        Ok(self)
    }

    /// Enables authenticated configuration routes using a securely opened token file.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the file or token fails the management-token policy.
    pub fn with_config_coordinator_from_token_file(
        mut self,
        coordinator: CanonicalConfigCoordinator,
        active_revision: ConfigRevision,
        token_file: &Path,
    ) -> io::Result<Self> {
        self.coordinator = Some(coordinator.clone());
        self.config = Some(ConfigApiState::from_token_file(
            coordinator,
            active_revision,
            token_file,
        )?);
        Ok(self)
    }

    #[must_use]
    pub fn handle(&self, method: &str, path: &str, now_unix_ms: u64) -> ApiResponse {
        if let Some(response) = self.ui.as_ref().and_then(|ui| ui.response(path)) {
            return if method == "GET" {
                response
            } else {
                ApiResponse::method_not_allowed("GET")
            };
        }

        let Some(route) = match_api_route(path) else {
            return ApiResponse::route_not_found();
        };
        match route {
            ApiRoute::Config(route) => self
                .config
                .as_ref()
                .map_or_else(ApiResponse::route_not_found, |_| {
                    ConfigApiState::unauthenticated_response(route, method)
                }),
            ApiRoute::Observability(route) => observability::handle(
                route,
                method,
                &self.metrics,
                self.registry.as_ref(),
                self.topology.as_ref(),
                self.generations.as_ref(),
            ),
            ApiRoute::Stream(route) => {
                streams::handle(route, method, self.registry.as_ref(), now_unix_ms)
            }
        }
    }

    fn handle_at_system_time(&self, method: &str, path: &str) -> ApiResponse {
        match system_time_ms() {
            Ok(now_unix_ms) => self.handle(method, path, now_unix_ms),
            Err(response) => response,
        }
    }

    fn authentication_error(
        &self,
        method: &str,
        path: &str,
        path_and_query: &str,
        session: &ServerSession,
    ) -> Option<ApiResponse> {
        let public_read = method == "GET" && matches!(path, "/ready" | "/metrics");
        let api_request =
            management::match_route(path_and_query).is_some() || match_api_route(path).is_some();
        if !api_request || public_read {
            return None;
        }
        if matches!(
            single_header(&session.req_header().headers, &AUTHORIZATION),
            HeaderCardinality::Duplicate
        ) {
            return Some(ApiResponse::error(
                400,
                "duplicate_authorization",
                "multiple Authorization headers are not accepted",
            ));
        }
        if !self
            .config
            .as_ref()
            .is_some_and(|config| config.authorized(session))
        {
            return Some(ApiResponse::unauthorized());
        }
        None
    }

    async fn api_response(&self, session: &mut ServerSession) -> ApiResponse {
        let (method, path, path_and_query) = {
            let request = session.req_header();
            (
                request.method.as_str().to_owned(),
                request.uri.path().to_owned(),
                request
                    .uri
                    .path_and_query()
                    .map_or_else(|| request.uri.path().to_owned(), ToString::to_string),
            )
        };

        if let Some(response) = self.authentication_error(&method, &path, &path_and_query, session)
        {
            return response;
        }
        if let Some(route) = management::match_route(&path_and_query) {
            if let (Some(config), Some(management)) = (&self.config, &self.management) {
                let _ = config;
                return management.handle(route, &method, session).await;
            }
            return ApiResponse::route_not_found();
        }
        if let Some(route) = config::match_route(&path) {
            if let Some(config) = &self.config {
                return config.handle_http(route, &method, session).await;
            }
            return self.handle_at_system_time(&method, &path);
        }
        if method != "GET" && streams::match_route(&path).is_some() {
            let route = streams::match_route(&path).expect("matched stream route");
            let Some(generations) = &self.generations else {
                return ApiResponse::error(
                    503,
                    "generation_unavailable",
                    "generation state is unavailable",
                );
            };
            let mut revisions = session
                .req_header()
                .headers
                .get_all("if-generation-revision")
                .iter();
            let Some(revision) = revisions
                .next()
                .filter(|_| revisions.next().is_none())
                .and_then(|value| value.to_str().ok())
            else {
                return ApiResponse::error(
                    428,
                    "precondition_required",
                    "If-Generation-Revision is required",
                );
            };
            let mutation = match generations.begin_mutation(revision) {
                Ok(mutation) => mutation,
                Err(error) => {
                    return ApiResponse::error(
                        409,
                        error.code(),
                        "the active generation revision changed",
                    );
                }
            };
            match system_time_ms() {
                Ok(now_unix_ms) => {
                    return streams::handle(
                        route,
                        &method,
                        mutation.generation().registry(),
                        now_unix_ms,
                    );
                }
                Err(response) => return response,
            }
        }
        self.handle_at_system_time(&method, &path)
    }
}

impl RtmpManagementHttpApp {
    async fn stream_events(
        &self,
        session: &mut ServerSession,
        cursor: EventStreamCursor,
        shutdown: &ShutdownWatch,
    ) {
        let mut shutdown = shutdown.clone();
        session.set_keepalive(None);
        session.set_write_timeout(Some(EVENT_STREAM_WRITE_TIMEOUT));

        let mut header = ResponseHeader::build(200, None).expect("valid SSE status");
        header
            .insert_header("content-type", "text/event-stream; charset=utf-8")
            .expect("valid SSE content type");
        header
            .insert_header(CACHE_CONTROL, "no-cache, no-store")
            .expect("valid SSE cache policy");
        header
            .insert_header(TRANSFER_ENCODING, "chunked")
            .expect("valid SSE transfer encoding");
        header
            .insert_header("x-content-type-options", "nosniff")
            .expect("valid SSE content protection");
        if session
            .write_response_header(Box::new(header))
            .await
            .is_err()
        {
            return;
        }

        let limit = cursor.limit;
        let mut cursor = cursor
            .after
            .unwrap_or_else(crate::operational_event::current_cursor);
        if !self.write_sse_frame(session, ready_frame(cursor)).await {
            return;
        }

        let start = Instant::now() + EVENT_STREAM_HEARTBEAT;
        let mut heartbeat = interval_at(start, EVENT_STREAM_HEARTBEAT);
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            if *shutdown.borrow() {
                let _ = self.write_sse_frame(session, shutdown_frame()).await;
                return;
            }

            let notification = crate::operational_event::wait_for_event();
            let page = crate::operational_event::page(cursor, limit);
            if page.cursor_lost {
                let _ = self
                    .write_sse_frame(
                        session,
                        resync_frame(cursor, page.oldest_cursor, page.latest_cursor),
                    )
                    .await;
                return;
            }
            if !page.events.is_empty() {
                for event in page.events {
                    if !self
                        .write_sse_frame(session, operational_frame(&event))
                        .await
                    {
                        return;
                    }
                    cursor = event.cursor;
                }
                continue;
            }

            tokio::select! {
                biased;
                () = notification => {}
                _ = heartbeat.tick() => {
                    if !self.write_sse_frame(session, heartbeat_frame()).await {
                        return;
                    }
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        let _ = self.write_sse_frame(session, shutdown_frame()).await;
                        return;
                    }
                }
            }
        }
    }

    async fn write_sse_frame(&self, session: &mut ServerSession, frame: Vec<u8>) -> bool {
        if frame.len() > EVENT_STREAM_FRAME_LIMIT {
            return false;
        }
        session
            .write_response_body(Bytes::from(frame), false)
            .await
            .is_ok()
    }
}

#[derive(Clone, Copy)]
struct EventStreamCursor {
    after: Option<u64>,
    limit: usize,
}

fn parse_event_stream_cursor(
    query: Option<&str>,
    last_event_id: Option<&str>,
) -> Result<EventStreamCursor, ApiResponse> {
    let mut after = None;
    let mut limit = EVENT_STREAM_BATCH_LIMIT;
    if let Some(query) = query {
        for pair in query.split('&') {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            match name {
                "after" if after.is_none() => {
                    after = Some(value.parse().map_err(|_| {
                        ApiResponse::error(400, "invalid_cursor", "event cursor is invalid")
                    })?);
                }
                "after" => {
                    return Err(ApiResponse::error(
                        400,
                        "invalid_cursor",
                        "event cursor must be specified once",
                    ));
                }
                "limit" if (1..=EVENT_STREAM_BATCH_LIMIT).contains(&value.parse().unwrap_or(0)) => {
                    limit = value.parse().expect("validated event stream limit");
                }
                "limit" => {
                    return Err(ApiResponse::error(
                        400,
                        "invalid_limit",
                        "event stream limit must be between 1 and 64",
                    ));
                }
                _ => {
                    return Err(ApiResponse::error(
                        400,
                        "invalid_query",
                        "event stream query parameter is invalid",
                    ));
                }
            }
        }
    }

    if let Some(last_event_id) = last_event_id {
        after =
            Some(last_event_id.parse().map_err(|_| {
                ApiResponse::error(400, "invalid_cursor", "Last-Event-ID is invalid")
            })?);
    }
    Ok(EventStreamCursor { after, limit })
}

fn last_event_id(session: &ServerSession) -> Result<Option<String>, ApiResponse> {
    let mut ids = session.req_header().headers.get_all(&LAST_EVENT_ID).iter();
    match (ids.next(), ids.next()) {
        (None, _) => Ok(None),
        (Some(_), Some(_)) => Err(ApiResponse::error(
            400,
            "invalid_cursor",
            "Last-Event-ID must be specified once",
        )),
        (Some(value), None) => value
            .to_str()
            .map(str::to_owned)
            .map(Some)
            .map_err(|_| ApiResponse::error(400, "invalid_cursor", "Last-Event-ID is invalid")),
    }
}

fn ready_frame(cursor: u64) -> Vec<u8> {
    format!("event: ready\ndata: {{\"cursor\":{cursor}}}\n\n").into_bytes()
}

fn operational_frame(event: &crate::operational_event::OperationalEvent) -> Vec<u8> {
    let data = serde_json::to_string(event).expect("typed operational event serializes");
    let mut frame = String::new();
    let _ = writeln!(frame, "id: {}", event.cursor);
    let _ = writeln!(frame, "event: {}", event.event.as_str());
    let _ = writeln!(frame, "data: {data}\n");
    frame.into_bytes()
}

fn resync_frame(requested: u64, oldest: Option<u64>, latest: u64) -> Vec<u8> {
    format!(
        "event: resync_required\ndata: {{\"cursor\":{requested},\"oldestCursor\":{},\"latestCursor\":{latest}}}\n\n",
        oldest.map_or_else(|| "null".to_owned(), |cursor| cursor.to_string())
    )
    .into_bytes()
}

fn heartbeat_frame() -> Vec<u8> {
    b": heartbeat\n\n".to_vec()
}

fn shutdown_frame() -> Vec<u8> {
    b"event: shutdown\ndata: {\"reason\":\"server_shutdown\"}\n\n".to_vec()
}

fn accepts_event_stream(session: &ServerSession) -> bool {
    session
        .req_header()
        .headers
        .get_all(ACCEPT)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|value| {
            value.split(';').next().is_some_and(|media_type| {
                media_type.trim().eq_ignore_ascii_case("text/event-stream")
            })
        })
}

fn request_parts(session: &ServerSession) -> (String, String, String) {
    let request = session.req_header();
    (
        request.method.as_str().to_owned(),
        request.uri.path().to_owned(),
        request
            .uri
            .path_and_query()
            .map_or_else(|| request.uri.path().to_owned(), ToString::to_string),
    )
}

async fn write_buffered_response(session: &mut ServerSession, response: ApiResponse) -> bool {
    let (parts, body) = to_http_response(response).into_parts();
    let response_header: ResponseHeader = parts.into();
    if session
        .write_response_header(Box::new(response_header))
        .await
        .is_err()
    {
        return false;
    }
    if body.is_empty() {
        session.finish_body().await.is_ok()
    } else {
        session.write_response_body(body.into(), true).await.is_ok()
    }
}

async fn finish_http_session(session: ServerSession) -> Option<ReusedHttpStream> {
    let persistent_settings = HttpPersistentSettings::for_session(&session);
    match session.finish().await {
        Ok(stream) => stream.map(|stream| ReusedHttpStream::new(stream, Some(persistent_settings))),
        Err(_) => None,
    }
}

#[async_trait]
impl ServeHttp for RtmpManagementApi {
    async fn response(&self, session: &mut ServerSession) -> Response<Vec<u8>> {
        to_http_response(self.api_response(session).await)
    }
}

#[async_trait]
impl HttpServerApp for RtmpManagementHttpApp {
    async fn process_new_http(
        self: &Arc<Self>,
        mut session: ServerSession,
        shutdown: &ShutdownWatch,
    ) -> Option<ReusedHttpStream> {
        match session.read_request().await {
            Ok(true) => {}
            Ok(false) | Err(_) => return None,
        }
        if *shutdown.borrow() {
            session.set_keepalive(None);
        } else {
            session.set_keepalive(Some(60));
        }

        let (method, path, path_and_query) = request_parts(&session);
        let stream_route =
            management::event_stream_route(&path_and_query, accepts_event_stream(&session));
        if method == "GET" {
            if let Some(stream_route) = stream_route {
                if let Some(response) =
                    self.api
                        .authentication_error(&method, &path, &path_and_query, &session)
                {
                    if write_buffered_response(&mut session, response).await {
                        return finish_http_session(session).await;
                    }
                    return None;
                }
                let last_event_id = match last_event_id(&session) {
                    Ok(last_event_id) => last_event_id,
                    Err(response) => {
                        if write_buffered_response(&mut session, response).await {
                            return finish_http_session(session).await;
                        }
                        return None;
                    }
                };
                let cursor =
                    match parse_event_stream_cursor(stream_route.query, last_event_id.as_deref()) {
                        Ok(cursor) => cursor,
                        Err(response) => {
                            if write_buffered_response(&mut session, response).await {
                                return finish_http_session(session).await;
                            }
                            return None;
                        }
                    };
                self.stream_events(&mut session, cursor, shutdown).await;
                return finish_http_session(session).await;
            }
        }

        let response = self.api.api_response(&mut session).await;
        if !write_buffered_response(&mut session, response).await {
            return None;
        }
        finish_http_session(session).await
    }
}

fn match_api_route(path: &str) -> Option<ApiRoute<'_>> {
    config::match_route(path)
        .map(ApiRoute::Config)
        .or_else(|| observability::match_route(path).map(ApiRoute::Observability))
        .or_else(|| streams::match_route(path).map(ApiRoute::Stream))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operational_event::OperationalEvent;

    #[test]
    fn initial_and_reconnect_cursors_are_distinct_and_stable() {
        let initial = parse_event_stream_cursor(None, None).expect("initial cursor");
        assert_eq!(initial.after, None);
        assert_eq!(initial.limit, EVENT_STREAM_BATCH_LIMIT);

        let reconnect = parse_event_stream_cursor(Some("after=4&limit=3"), Some("9"))
            .expect("reconnect cursor");
        assert_eq!(reconnect.after, Some(9));
        assert_eq!(reconnect.limit, 3);
        assert!(parse_event_stream_cursor(Some("limit=65"), None).is_err());
    }

    #[test]
    fn sse_frames_include_cursors_and_bounded_heartbeat() {
        let event = OperationalEvent {
            cursor: 7,
            timestamp_unix_ms: None,
            event: crate::operational_event::EventName::GenerationPrepare,
            outcome: crate::operational_event::EventOutcome::Prepared,
            revision: None,
        };
        let frame = operational_frame(&event);

        assert!(String::from_utf8_lossy(&frame).contains("id: 7\nevent: generation_prepare"));
        assert!(frame.len() <= EVENT_STREAM_FRAME_LIMIT);
        assert_eq!(heartbeat_frame(), b": heartbeat\n\n");
        assert!(ready_frame(u64::MAX).len() <= EVENT_STREAM_FRAME_LIMIT);
        assert!(resync_frame(1, Some(2), 3).len() <= EVENT_STREAM_FRAME_LIMIT);
    }

    #[test]
    fn redacted_typed_event_data_cannot_copy_raw_credentials_into_sse() {
        let event = OperationalEvent {
            cursor: 11,
            timestamp_unix_ms: None,
            event: crate::operational_event::EventName::Unknown,
            outcome: crate::operational_event::EventOutcome::Unknown,
            revision: None,
        };
        let frame = operational_frame(&event);
        let frame = String::from_utf8(frame).expect("SSE frame");

        assert!(frame.contains(r#""event":"unknown""#));
        assert!(!frame.contains("private-key-secret"));
        assert!(!frame.contains("session-secret"));
    }
}
