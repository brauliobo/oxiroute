use std::{
    fmt::Write as _,
    io,
    path::Path,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

use super::{
    ApiResponse,
    auth::ManagementAuth,
    config::{self, ConfigApiState, Route as ConfigRoute},
    endpoint_registry::{self, EndpointId},
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
    config::coordinator::{CanonicalConfigCoordinator, EffectiveRevision},
    lifecycle_control::{DirectLifecycleControl, LifecyclePort},
    observability::operational_event::{
        self, AuditCategory, AuditContext, AuditLimits, AuditResult, AuditStore,
    },
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
    auth: Option<ManagementAuth>,
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
            auth: None,
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
            auth: None,
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
        let auth = ManagementAuth::new(token)?;
        let config = ConfigApiState::new(coordinator.clone(), active_revision, mode);
        self.auth = Some(auth);
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
        let auth = ManagementAuth::from_token_file(token_file)?;
        let config = ConfigApiState::new(coordinator.clone(), active_revision, mode);
        self.auth = Some(auth);
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

    fn authentication_error(
        &self,
        method: &str,
        path: &str,
        path_and_query: &str,
        session: &ServerSession,
    ) -> Option<ApiResponse> {
        let public_read = method == "GET" && matches!(path, "/ready" | "/metrics" | "/status.json");
        let api_request = endpoint_registry::match_path_and_query(path_and_query).is_some()
            || management::match_route(path_and_query).is_some()
            || match_api_route(path).is_some();
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
            .auth
            .as_ref()
            .is_some_and(|auth| auth.authorized(session))
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
        if let Some(route) = crate::registry::http::match_route(&path) {
            let response = self
                .registry_response(route, &method, &path_and_query, session)
                .await;
            Self::audit_api_operation(&method, &path, &context, &response);
            return response.with_correlation(context.correlation_id);
        }
        if let Some(response) = self.authentication_error(&method, &path, &path_and_query, session)
        {
            Self::audit_api_operation(&method, &path, &context, &response);
            return response.with_correlation(context.correlation_id);
        }
        if let Some(endpoint) = endpoint_registry::match_path_and_query(&path_and_query) {
            let response = self
                .handle_registered_endpoint(endpoint, &method, session, &context)
                .await;
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
                let auth = self
                    .auth
                    .as_ref()
                    .expect("configuration routes require management authentication");
                let response = config.handle_http(auth, route, &method, session).await;
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

    /// Registry routes accept the root management token (admin) or a scoped registry token.
    async fn registry_response(
        &self,
        route: crate::registry::http::Route<'_>,
        method: &str,
        path_and_query: &str,
        session: &mut ServerSession,
    ) -> ApiResponse {
        use crate::registry::{auth::Principal, http};
        let Some(service) = self
            .generations
            .as_ref()
            .and_then(GenerationManager::registry)
        else {
            return ApiResponse::error(
                404,
                "registry_disabled",
                "server registration is not configured",
            );
        };
        let authorization = match single_header(&session.req_header().headers, &AUTHORIZATION) {
            HeaderCardinality::Single(value) => Some(value.as_bytes().to_vec()),
            HeaderCardinality::Missing => None,
            HeaderCardinality::Duplicate => {
                return ApiResponse::error(
                    400,
                    "duplicate_authorization",
                    "multiple Authorization headers are not accepted",
                );
            }
        };
        let principal = if self
            .auth
            .as_ref()
            .is_some_and(|auth| auth.authorized(session))
        {
            Principal::admin("root")
        } else if let Some(principal) = authorization
            .as_deref()
            .and_then(http::bearer)
            .and_then(|secret| service.authenticate(secret))
        {
            principal
        } else {
            return ApiResponse::unauthorized();
        };
        if let Err(error) = service.check_rate(&principal.id) {
            return registry_reply(&http::Reply::error(&error));
        }
        let body = match config::read_config_body(session).await {
            Ok(body) => body,
            Err(response) => return response,
        };
        let query = path_and_query.split_once('?').map(|(_, query)| query);
        let request = http::Request {
            method,
            query,
            body: &body,
            principal: &principal,
            peer: None,
        };
        registry_reply(&http::handle(&service, route, &request))
    }

    async fn handle_registered_endpoint(
        &self,
        endpoint: EndpointId,
        method: &str,
        session: &mut ServerSession,
        context: &AuditContext,
    ) -> ApiResponse {
        match endpoint {
            EndpointId::Status => observability::handle(
                observability::Route::Status,
                method,
                &self.metrics,
                &self.control,
                self.topology.as_ref(),
                self.generations.as_ref(),
            ),
            EndpointId::Listeners => {
                let Some(management) = &self.management else {
                    return ApiResponse::route_not_found();
                };
                management
                    .handle(management::Route::Listeners, method, session, context)
                    .await
            }
            EndpointId::Pools => {
                let Some(management) = &self.management else {
                    return ApiResponse::route_not_found();
                };
                management
                    .handle(management::Route::Pools, method, session, context)
                    .await
            }
            EndpointId::Servers => {
                let Some(management) = &self.management else {
                    return ApiResponse::route_not_found();
                };
                management
                    .handle(management::Route::Servers, method, session, context)
                    .await
            }
            EndpointId::Generations => {
                let Some(management) = &self.management else {
                    return ApiResponse::route_not_found();
                };
                management
                    .handle(management::Route::Generations, method, session, context)
                    .await
            }
            EndpointId::Requests => {
                observability::requests(method, session.req_header().uri.query())
            }
        }
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

impl RtmpManagementHttpApp {}

fn registry_reply(reply: &crate::registry::http::Reply) -> ApiResponse {
    let mut response = match &reply.body {
        Some(body) => ApiResponse::json(reply.status, body),
        None => ApiResponse::bytes(reply.status, Vec::new(), "application/json"),
    };
    response.allow = reply.allow;
    response
}

fn match_api_route(path: &str) -> Option<ApiRoute<'_>> {
    if endpoint_registry::match_path(path) == Some(EndpointId::Status) {
        return Some(ApiRoute::Observability(ObservabilityRoute::Status));
    }
    config::match_route(path)
        .map(ApiRoute::Config)
        .or_else(|| observability::match_route(path).map(ApiRoute::Observability))
        .or_else(|| rtmp::match_route(path).map(ApiRoute::Rtmp))
        .or_else(|| streams::match_route(path).map(ApiRoute::Stream))
        .or_else(|| vod::match_route(path).map(|_| ApiRoute::Vod))
        .or_else(|| media::match_route(path).map(|_| ApiRoute::Media))
}

mod events;
mod http_app;
mod request;
mod vod_media;

use events::{accepts_event_stream, last_event_id, parse_event_stream_cursor};
#[cfg(test)]
use events::{heartbeat_frame, operational_frame, operational_frame_v2, ready_frame, resync_frame};
use request::{audit_result_for_status, audited_api_operation, request_context, request_parts};

#[cfg(test)]
mod tests;
