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
    media::{self, Route as MediaRoute},
    observability::{self, Route as ObservabilityRoute},
    response::{system_time_ms, to_http_response},
    rtmp::{self, Route as RtmpRoute},
    streams::{self, Route as StreamRoute},
    ui::UiAssets,
    vod::{self, Route as VodRoute},
};
use crate::{
    GenerationManager, RuntimeMetrics, TopologySnapshot,
    config_coordinator::{CanonicalConfigCoordinator, EffectiveRevision},
    lifecycle_control::{DirectLifecycleControl, LifecyclePort},
    operational_event::{self, AuditCategory, AuditContext, AuditLimits, AuditResult, AuditStore},
    secure_bearer::{HeaderCardinality, single_header},
};
use async_trait::async_trait;
use bytes::Bytes;
use http::{
    Response,
    header::{ACCEPT, AUTHORIZATION, CACHE_CONTROL, HeaderName, TRANSFER_ENCODING},
};
use oxiroute_rtmp::{MediaStoreError, RtmpControlHandle, VodError, VodRange};
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
const CORRELATION_ID: HeaderName = HeaderName::from_static("x-correlation-id");

enum ApiRoute<'a> {
    Config(ConfigRoute),
    Observability(ObservabilityRoute),
    Rtmp(RtmpRoute<'a>),
    Stream(StreamRoute<'a>),
    Vod,
    Media,
}

pub struct RtmpManagementApi {
    config: Option<ConfigApiState>,
    coordinator: Option<CanonicalConfigCoordinator>,
    generations: Option<GenerationManager>,
    management: Option<ManagementState>,
    metrics: RuntimeMetrics,
    control: RtmpControlHandle,
    topology: Arc<TopologySnapshot>,
    ui: Option<UiAssets>,
    audit: Arc<AuditStore>,
    process_shutdown: Option<Arc<AtomicBool>>,
}

pub struct RtmpManagementHttpApp {
    api: RtmpManagementApi,
}

impl RtmpManagementApi {
    #[must_use]
    pub fn new(
        control: RtmpControlHandle,
        metrics: RuntimeMetrics,
        topology: Arc<TopologySnapshot>,
    ) -> Self {
        Self {
            config: None,
            coordinator: None,
            generations: None,
            management: None,
            metrics,
            control,
            topology,
            ui: None,
            audit: Arc::new(AuditStore::memory(AuditLimits::default())),
            process_shutdown: None,
        }
    }

    /// Loads a prebuilt Vue application into the management service.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when `index.html` or an asset cannot be read at startup.
    pub fn with_ui_dir(
        control: RtmpControlHandle,
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
            control,
            topology,
            ui: Some(UiAssets::load(directory.as_ref())?),
            audit: Arc::new(AuditStore::memory(AuditLimits::default())),
            process_shutdown: None,
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
        self.generations = Some(generations);
        self.rebuild_management();
        self
    }

    #[must_use]
    pub fn with_process_shutdown(mut self, shutdown: Arc<AtomicBool>) -> Self {
        self.process_shutdown = Some(shutdown);
        self.rebuild_management();
        self
    }

    fn rebuild_management(&mut self) {
        let (Some(coordinator), Some(generations)) = (&self.coordinator, &self.generations) else {
            self.management = None;
            return;
        };
        let lifecycle: Arc<LifecyclePort> = Arc::new(DirectLifecycleControl::new(
            coordinator.clone(),
            generations.clone(),
            self.process_shutdown.clone(),
        ));
        self.management = Some(ManagementState::new(
            generations.clone(),
            lifecycle,
            self.metrics.clone(),
            Arc::clone(&self.audit),
        ));
    }

    /// Enables authenticated configuration routes with an injected token.
    ///
    /// # Errors
    ///
    /// Returns an error when the token does not meet the management-token policy.
    pub fn with_config_coordinator(
        mut self,
        coordinator: CanonicalConfigCoordinator,
        active_revision: EffectiveRevision,
        token: &str,
        mode: crate::RuntimeMode,
    ) -> io::Result<Self> {
        self.audit = operational_event::configure_audit_store(None);
        let config = ConfigApiState::new(coordinator.clone(), active_revision, token, mode)?;
        self.coordinator = Some(coordinator);
        self.config = Some(config);
        self.rebuild_management();
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
        active_revision: EffectiveRevision,
        token_file: &Path,
        mode: crate::RuntimeMode,
    ) -> io::Result<Self> {
        self.audit = operational_event::configure_audit_store(Some(token_file));
        let config = ConfigApiState::from_token_file(
            coordinator.clone(),
            active_revision,
            token_file,
            mode,
        )?;
        self.coordinator = Some(coordinator);
        self.config = Some(config);
        self.rebuild_management();
        Ok(self)
    }

    #[must_use]
    pub fn handle(&self, method: &str, path: &str, now_unix_ms: u64) -> ApiResponse {
        let context = AuditContext::generated();
        if let Some(response) = self.ui.as_ref().and_then(|ui| ui.response(path)) {
            let response = if method == "GET" {
                response
            } else {
                ApiResponse::method_not_allowed("GET")
            };
            return response.with_correlation(context.correlation_id);
        }

        let Some(route) = match_api_route(path) else {
            return ApiResponse::route_not_found().with_correlation(context.correlation_id);
        };
        let response = match route {
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
                &self.control,
                self.topology.as_ref(),
                self.generations.as_ref(),
            ),
            ApiRoute::Rtmp(route) => rtmp::handle(route, method, &self.control, None),
            ApiRoute::Stream(route) => streams::handle(route, method, &self.control, now_unix_ms),
            ApiRoute::Vod | ApiRoute::Media => ApiResponse::method_not_allowed("GET"),
        };
        response.with_correlation(context.correlation_id)
    }

    fn handle_at_system_time(&self, method: &str, path: &str) -> ApiResponse {
        match system_time_ms() {
            Ok(now_unix_ms) => self.handle(method, path, now_unix_ms),
            Err(response) => response,
        }
    }

    async fn vod_response(&self, route: VodRoute<'_>, session: &ServerSession) -> ApiResponse {
        if session.req_header().method.as_str() != "GET" {
            return ApiResponse::method_not_allowed("GET");
        }
        let active = self
            .generations
            .as_ref()
            .and_then(GenerationManager::active);
        let catalog = active.as_ref().map_or_else(
            || self.control.vod_catalog(),
            |generation| generation.rtmp_vod_catalog(),
        );
        let service = route.service.to_owned();
        let application = route.application.to_owned();
        let source = route.source.to_owned();
        let path = route.path.to_owned();
        let content_path = path.clone();
        let object = match tokio::task::spawn_blocking(move || {
            let _generation = active;
            catalog.open(&service, &application, &source, &path)
        })
        .await
        {
            Ok(Ok(object)) => object,
            Ok(Err(error)) => return vod_error_response(error),
            Err(_) => return ApiResponse::error(503, "vod_unavailable", "VOD worker failed"),
        };
        let total = object.len();
        let range_header = session
            .req_header()
            .headers
            .get("range")
            .and_then(|value| value.to_str().ok());
        let range = match VodRange::parse(range_header, total) {
            Ok(range) => range,
            Err(VodError::InvalidRange) => {
                return ApiResponse::error(
                    416,
                    "invalid_range",
                    "the requested byte range is invalid",
                )
                .with_range(Some(format!("bytes */{total}")));
            }
            Err(error) => return vod_error_response(error),
        };
        let body = match range {
            Some(range) => match object.range(range) {
                Ok(body) => body,
                Err(error) => return vod_error_response(error),
            },
            None => Vec::new(),
        };
        let content_range = if range_header.is_some() {
            range.map(|range| format!("bytes {}-{}/{}", range.start, range.end, total))
        } else {
            None
        };
        ApiResponse::bytes(
            if range_header.is_some() { 206 } else { 200 },
            body,
            vod_content_type(&content_path),
        )
        .with_range(content_range)
    }

    async fn media_response(&self, route: MediaRoute<'_>, session: &ServerSession) -> ApiResponse {
        if session.req_header().method.as_str() != "GET" {
            return ApiResponse::method_not_allowed("GET");
        }
        let active = self
            .generations
            .as_ref()
            .and_then(GenerationManager::active);
        let catalog = active.as_ref().map_or_else(
            || self.control.media_catalog(),
            |generation| generation.rtmp_media_catalog(),
        );
        let service = route.service.to_owned();
        let application = route.application.to_owned();
        let stream = route.stream.to_owned();
        let object = route.object.to_owned();
        let object = match tokio::task::spawn_blocking(move || {
            let _generation = active;
            catalog.read_object(&service, &application, &stream, &object)
        })
        .await
        {
            Ok(Ok(object)) => object,
            Ok(Err(error)) => return media_error_response(error),
            Err(_) => return ApiResponse::error(503, "media_unavailable", "media worker failed"),
        };
        let total = u64::try_from(object.body.len()).unwrap_or(u64::MAX);
        let range_header = session
            .req_header()
            .headers
            .get("range")
            .and_then(|value| value.to_str().ok());
        let Ok(range) = VodRange::parse(range_header, total) else {
            return ApiResponse::error(
                416,
                "media_invalid_range",
                "the requested byte range is invalid",
            )
            .with_range(Some(format!("bytes */{total}")));
        };
        let body = match range {
            Some(range) => {
                let start = usize::try_from(range.start).expect("media range starts in object");
                let end = usize::try_from(range.end).expect("media range ends in object");
                object.body[start..=end].to_vec()
            }
            None => Vec::new(),
        };
        let content_range = if range_header.is_some() {
            range.map(|range| format!("bytes {}-{}/{}", range.start, range.end, total))
        } else {
            None
        };
        ApiResponse::bytes(
            if range_header.is_some() { 206 } else { 200 },
            if range_header.is_some() {
                body
            } else {
                object.body
            },
            object.content_type,
        )
        .with_range(content_range)
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

    #[allow(clippy::too_many_lines)]
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

        let context = match request_context(session) {
            Ok(context) => context,
            Err(response) => {
                return response.with_correlation(AuditContext::generated().correlation_id);
            }
        };
        if let Some(response) = self.authentication_error(&method, &path, &path_and_query, session)
        {
            Self::audit_api_operation(&method, &path, &context, &response);
            return response.with_correlation(context.correlation_id);
        }
        if let Some(route) = management::match_route(&path_and_query) {
            if let (Some(config), Some(management)) = (&self.config, &self.management) {
                let _ = config;
                let response = management.handle(route, &method, session, &context).await;
                Self::audit_api_operation(&method, &path, &context, &response);
                return response.with_correlation(context.correlation_id);
            }
            return ApiResponse::route_not_found().with_correlation(context.correlation_id);
        }
        if let Some(route) = config::match_route(&path) {
            if let Some(config) = &self.config {
                let response = config.handle_http(route, &method, session).await;
                Self::audit_api_operation(&method, &path, &context, &response);
                return response.with_correlation(context.correlation_id);
            }
            return self
                .handle_at_system_time(&method, &path)
                .with_correlation(context.correlation_id);
        }
        if let Some(route) = rtmp::match_route(&path) {
            let response = rtmp::handle(route, &method, &self.control, Some(session));
            Self::audit_api_operation(&method, &path, &context, &response);
            return response.with_correlation(context.correlation_id);
        }
        if method != "GET" && streams::match_route(&path).is_some() {
            let route = streams::match_route(&path).expect("matched stream route");
            let Some(generations) = &self.generations else {
                return ApiResponse::error(
                    503,
                    "generation_unavailable",
                    "generation state is unavailable",
                )
                .with_correlation(context.correlation_id);
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
                )
                .with_correlation(context.correlation_id);
            };
            let Ok(revision) = revision.parse::<EffectiveRevision>() else {
                return ApiResponse::error(
                    409,
                    "generation_conflict",
                    "the active generation revision changed",
                )
                .with_correlation(context.correlation_id);
            };
            let mutation = match generations.begin_mutation(&revision) {
                Ok(mutation) => mutation,
                Err(error) => {
                    return ApiResponse::error(
                        409,
                        error.code(),
                        "the active generation revision changed",
                    )
                    .with_correlation(context.correlation_id);
                }
            };
            match system_time_ms() {
                Ok(now_unix_ms) => {
                    let response = streams::handle(
                        route,
                        &method,
                        &mutation.generation().rtmp_control(),
                        now_unix_ms,
                    );
                    Self::audit_api_operation(&method, &path, &context, &response);
                    return response.with_correlation(context.correlation_id);
                }
                Err(response) => {
                    return response.with_correlation(context.correlation_id);
                }
            }
        }
        let response = self.handle_at_system_time(&method, &path);
        Self::audit_api_operation(&method, &path, &context, &response);
        response.with_correlation(context.correlation_id)
    }

    fn audit_api_operation(
        method: &str,
        path: &str,
        context: &AuditContext,
        response: &ApiResponse,
    ) {
        let Some((operation, category)) = audited_api_operation(method, path) else {
            return;
        };
        operational_event::emit_api_operation(
            operation,
            category,
            audit_result_for_status(response.status),
            None,
            context,
        );
    }
}

impl RtmpManagementHttpApp {
    async fn stream_events(
        &self,
        session: &mut ServerSession,
        version: management::EventApiVersion,
        cursor: EventStreamCursor,
        shutdown: &ShutdownWatch,
        context: &AuditContext,
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
        header
            .insert_header("x-correlation-id", context.correlation_id.as_str())
            .expect("valid SSE correlation ID");
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
                        .write_sse_frame(
                            session,
                            match version {
                                management::EventApiVersion::V1 => operational_frame(&event),
                                management::EventApiVersion::V2 => operational_frame_v2(&event),
                            },
                        )
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
    let data = serde_json::to_string(&super::dto::SseReadyDto::new(cursor))
        .expect("SSE ready DTO serializes");
    format!("event: ready\ndata: {data}\n\n").into_bytes()
}

fn operational_frame(event: &crate::operational_event::OperationalEvent) -> Vec<u8> {
    let data = crate::operational_event::v1_event_value(event).to_string();
    let mut frame = String::new();
    let _ = writeln!(frame, "id: {}", event.cursor);
    let _ = writeln!(
        frame,
        "event: {}",
        crate::operational_event::v1_sse_event_name(event.event)
    );
    let _ = writeln!(frame, "data: {data}\n");
    frame.into_bytes()
}

fn operational_frame_v2(event: &crate::operational_event::OperationalEvent) -> Vec<u8> {
    let data = crate::operational_event::v2_event_value(event).to_string();
    let mut frame = String::new();
    let _ = writeln!(frame, "id: {}", event.cursor);
    let _ = writeln!(frame, "event: {}", event.event.as_str());
    let _ = writeln!(frame, "data: {data}\n");
    frame.into_bytes()
}

fn resync_frame(requested: u64, oldest: Option<u64>, latest: u64) -> Vec<u8> {
    let data = serde_json::to_string(&super::dto::SseResyncDto::new(requested, oldest, latest))
        .expect("SSE resync DTO serializes");
    format!("event: resync_required\ndata: {data}\n\n").into_bytes()
}

fn heartbeat_frame() -> Vec<u8> {
    b": heartbeat\n\n".to_vec()
}

fn shutdown_frame() -> Vec<u8> {
    let data = serde_json::to_string(&super::dto::SseShutdownDto::server_shutdown())
        .expect("SSE shutdown DTO serializes");
    format!("event: shutdown\ndata: {data}\n\n").into_bytes()
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

fn request_context(session: &ServerSession) -> Result<AuditContext, ApiResponse> {
    match single_header(&session.req_header().headers, &CORRELATION_ID) {
        HeaderCardinality::Missing => {
            let mut context = AuditContext::generated();
            context.actor = "management_bearer".into();
            context.source = "management_api".into();
            Ok(context)
        }
        HeaderCardinality::Duplicate => Err(ApiResponse::error(
            400,
            "duplicate_correlation_id",
            "multiple correlation IDs are not accepted",
        )),
        HeaderCardinality::Single(value) => value
            .to_str()
            .ok()
            .and_then(AuditContext::from_external)
            .ok_or_else(|| {
                ApiResponse::error(
                    400,
                    "invalid_correlation_id",
                    "correlation ID must be 1 to 64 safe ASCII characters",
                )
            }),
    }
}

fn audited_api_operation(method: &str, path: &str) -> Option<(&'static str, AuditCategory)> {
    if method == "GET" {
        return None;
    }
    let (operation, category) = match path {
        "/api/v1/config" => ("configuration_reload", AuditCategory::Reload),
        "/api/v1/generations/reload" => ("generation_reload", AuditCategory::Reload),
        "/api/v1/generations/rollback" => ("generation_rollback", AuditCategory::Reload),
        "/api/v1/generations/drain" => ("generation_drain", AuditCategory::Reload),
        "/api/v1/tls/reconcile" => ("certificate_reconcile", AuditCategory::Certificate),
        "/api/v1/tls/renew" => ("certificate_renew", AuditCategory::Certificate),
        "/api/v1/tls/revoke" => ("certificate_revoke", AuditCategory::Certificate),
        "/api/v1/tls/delete" => ("certificate_delete", AuditCategory::Certificate),
        "/api/v1/tls/account/rollover" => ("account_key_rollover", AuditCategory::Certificate),
        "/api/v1/tls/jobs/cancel" | "/api/v1/tls/jobs/pause" | "/api/v1/tls/jobs/resume" => {
            ("certificate_job_control", AuditCategory::Certificate)
        }
        "/api/v1/process/drain" => ("process_drain", AuditCategory::Control),
        "/api/v1/process/shutdown" => ("process_shutdown", AuditCategory::Control),
        "/api/v1/listeners/administrative-state" => ("listener_control", AuditCategory::Control),
        "/api/v1/pools/administrative-state" => ("pool_control", AuditCategory::Control),
        "/api/v1/servers/administrative-state"
        | "/api/v1/servers/health-override"
        | "/api/v1/servers/checks"
        | "/api/v1/servers/max-connections" => ("server_control", AuditCategory::Control),
        "/api/v1/servers/refresh-dns" => ("server_dns_refresh", AuditCategory::Control),
        _ => return None,
    };
    Some((operation, category))
}

#[allow(clippy::needless_pass_by_value)]
fn vod_error_response(error: VodError) -> ApiResponse {
    match error {
        VodError::SourceNotFound | VodError::NotFound => {
            ApiResponse::error(404, "vod_not_found", "the VOD object does not exist")
        }
        VodError::InvalidPath | VodError::InvalidRange => {
            ApiResponse::error(400, "vod_invalid_path", "the VOD path or range is invalid")
        }
        VodError::SessionLimit => {
            ApiResponse::error(429, "vod_session_limit", "the VOD session limit is reached")
        }
        VodError::TooLarge => ApiResponse::error(
            413,
            "vod_too_large",
            "the VOD object exceeds its configured bound",
        ),
        VodError::RootOpen | VodError::OriginDenied | VodError::Fetch => ApiResponse::error(
            503,
            "vod_source_unavailable",
            "the VOD source is unavailable",
        ),
        VodError::InvalidFlv | VodError::InvalidMedia => ApiResponse::error(
            422,
            "vod_invalid_media",
            "the VOD object is not valid media",
        ),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn media_error_response(error: MediaStoreError) -> ApiResponse {
    match error {
        MediaStoreError::NotFound | MediaStoreError::StaleIncarnation => {
            ApiResponse::error(404, "media_not_found", "the media object does not exist")
        }
        MediaStoreError::InvalidPath => ApiResponse::error(
            400,
            "media_invalid_path",
            "the media object path is invalid",
        ),
        MediaStoreError::FileTooLarge => ApiResponse::error(
            413,
            "media_too_large",
            "the media object exceeds its configured bound",
        ),
        MediaStoreError::ManifestMalformed => ApiResponse::error(
            422,
            "media_manifest_invalid",
            "the persisted media manifest is malformed",
        ),
        MediaStoreError::Read(_) => {
            ApiResponse::error(503, "media_unavailable", "the media object cannot be read")
        }
        MediaStoreError::RootOpen(_)
        | MediaStoreError::RootNotExclusive
        | MediaStoreError::RootScan(_)
        | MediaStoreError::ExistingUsageExceedsQuota
        | MediaStoreError::ActiveStreamLimit
        | MediaStoreError::Quota
        | MediaStoreError::Publish(_)
        | MediaStoreError::Cleanup(_) => {
            ApiResponse::error(503, "media_unavailable", "the media store is unavailable")
        }
    }
}

#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn vod_content_type(path: &str) -> &'static str {
    if path.ends_with(".mp4") {
        "video/mp4"
    } else if path.ends_with(".m4v") {
        "video/x-m4v"
    } else {
        "video/x-flv"
    }
}

const fn audit_result_for_status(status: u16) -> AuditResult {
    match status {
        202 => AuditResult::Requested,
        207 => AuditResult::Partial,
        200..=299 => AuditResult::Succeeded,
        409 => AuditResult::Conflict,
        400..=499 => AuditResult::Rejected,
        _ => AuditResult::Failed,
    }
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
    #[allow(clippy::too_many_lines)]
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
                let context = match request_context(&session) {
                    Ok(context) => context,
                    Err(response) => {
                        if write_buffered_response(
                            &mut session,
                            response.with_correlation(AuditContext::generated().correlation_id),
                        )
                        .await
                        {
                            return finish_http_session(session).await;
                        }
                        return None;
                    }
                };
                if let Some(response) =
                    self.api
                        .authentication_error(&method, &path, &path_and_query, &session)
                {
                    if write_buffered_response(
                        &mut session,
                        response.with_correlation(context.correlation_id.clone()),
                    )
                    .await
                    {
                        return finish_http_session(session).await;
                    }
                    return None;
                }
                let last_event_id = match last_event_id(&session) {
                    Ok(last_event_id) => last_event_id,
                    Err(response) => {
                        if write_buffered_response(
                            &mut session,
                            response.with_correlation(context.correlation_id.clone()),
                        )
                        .await
                        {
                            return finish_http_session(session).await;
                        }
                        return None;
                    }
                };
                let cursor =
                    match parse_event_stream_cursor(stream_route.query, last_event_id.as_deref()) {
                        Ok(cursor) => cursor,
                        Err(response) => {
                            if write_buffered_response(
                                &mut session,
                                response.with_correlation(context.correlation_id.clone()),
                            )
                            .await
                            {
                                return finish_http_session(session).await;
                            }
                            return None;
                        }
                    };
                self.stream_events(
                    &mut session,
                    stream_route.version,
                    cursor,
                    shutdown,
                    &context,
                )
                .await;
                return finish_http_session(session).await;
            }
            if let Some(route) = vod::match_route(&path) {
                let context = match request_context(&session) {
                    Ok(context) => context,
                    Err(response) => {
                        if write_buffered_response(
                            &mut session,
                            response.with_correlation(AuditContext::generated().correlation_id),
                        )
                        .await
                        {
                            return finish_http_session(session).await;
                        }
                        return None;
                    }
                };
                if let Some(response) =
                    self.api
                        .authentication_error(&method, &path, &path_and_query, &session)
                {
                    if write_buffered_response(
                        &mut session,
                        response.with_correlation(context.correlation_id.clone()),
                    )
                    .await
                    {
                        return finish_http_session(session).await;
                    }
                    return None;
                }
                let response = self
                    .api
                    .vod_response(route, &session)
                    .await
                    .with_correlation(context.correlation_id.clone());
                RtmpManagementApi::audit_api_operation(&method, &path, &context, &response);
                if !write_buffered_response(&mut session, response).await {
                    return None;
                }
                return finish_http_session(session).await;
            }
            if let Some(route) = media::match_route(&path) {
                let context = match request_context(&session) {
                    Ok(context) => context,
                    Err(response) => {
                        if write_buffered_response(
                            &mut session,
                            response.with_correlation(AuditContext::generated().correlation_id),
                        )
                        .await
                        {
                            return finish_http_session(session).await;
                        }
                        return None;
                    }
                };
                if let Some(response) =
                    self.api
                        .authentication_error(&method, &path, &path_and_query, &session)
                {
                    if write_buffered_response(
                        &mut session,
                        response.with_correlation(context.correlation_id.clone()),
                    )
                    .await
                    {
                        return finish_http_session(session).await;
                    }
                    return None;
                }
                let response = self
                    .api
                    .media_response(route, &session)
                    .await
                    .with_correlation(context.correlation_id.clone());
                RtmpManagementApi::audit_api_operation(&method, &path, &context, &response);
                if !write_buffered_response(&mut session, response).await {
                    return None;
                }
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
        .or_else(|| rtmp::match_route(path).map(ApiRoute::Rtmp))
        .or_else(|| streams::match_route(path).map(ApiRoute::Stream))
        .or_else(|| vod::match_route(path).map(|_| ApiRoute::Vod))
        .or_else(|| media::match_route(path).map(|_| ApiRoute::Media))
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
            certificate: None,
            correlation_id: None,
            actor: None,
            source: None,
            operation: None,
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
            certificate: None,
            correlation_id: None,
            actor: None,
            source: None,
            operation: None,
        };
        let frame = operational_frame(&event);
        let frame = String::from_utf8(frame).expect("SSE frame");

        assert!(frame.contains(r#""event":"unknown""#));
        assert!(!frame.contains("private-key-secret"));
        assert!(!frame.contains("session-secret"));
    }

    #[test]
    fn version_one_sse_keeps_the_shipped_certificate_activation_spellings() {
        let event = OperationalEvent {
            cursor: 12,
            timestamp_unix_ms: None,
            event: crate::operational_event::EventName::CertificateActivation,
            outcome: crate::operational_event::EventOutcome::Activated,
            revision: None,
            certificate: Some("edge".into()),
            correlation_id: None,
            actor: None,
            source: None,
            operation: None,
        };
        let frame = String::from_utf8(operational_frame(&event)).expect("SSE frame");

        assert!(frame.contains("event: certificate_activated"));
        assert!(frame.contains(r#""event":"certificate_activation""#));

        let event = OperationalEvent {
            event: crate::operational_event::EventName::CertificateRevocation,
            ..event
        };
        let frame = String::from_utf8(operational_frame(&event)).expect("SSE frame");
        assert!(frame.contains("event: unknown"));
        assert!(frame.contains(r#""event":"unknown""#));
    }

    #[test]
    fn version_two_sse_uses_the_complete_matching_event_vocabulary() {
        let event = OperationalEvent {
            cursor: 13,
            timestamp_unix_ms: None,
            event: crate::operational_event::EventName::CertificateRevocation,
            outcome: crate::operational_event::EventOutcome::Requested,
            revision: None,
            certificate: Some("edge".into()),
            correlation_id: None,
            actor: None,
            source: None,
            operation: None,
        };
        let frame = String::from_utf8(operational_frame_v2(&event)).expect("SSE frame");

        assert!(frame.contains("event: certificate_revocation"));
        assert!(frame.contains(r#""event":"certificate_revocation""#));
        assert!(frame.contains(r#""outcome":"requested""#));
    }
}
