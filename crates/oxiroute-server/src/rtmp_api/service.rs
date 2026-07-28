use std::{
    io,
    path::Path,
    sync::{Arc, atomic::AtomicBool},
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
};
use async_trait::async_trait;
use http::Response;
use oxiroute_rtmp::RtmpRegistry;
use pingora::{apps::http_app::ServeHttp, protocols::http::ServerSession};

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
}

#[async_trait]
impl ServeHttp for RtmpManagementApi {
    async fn response(&self, session: &mut ServerSession) -> Response<Vec<u8>> {
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

        let public_read = method == "GET" && matches!(path.as_str(), "/ready" | "/metrics");
        let api_request =
            management::match_route(&path_and_query).is_some() || match_api_route(&path).is_some();
        let authorization_count = session
            .req_header()
            .headers
            .get_all(http::header::AUTHORIZATION)
            .iter()
            .count();
        let authorized = self
            .config
            .as_ref()
            .is_some_and(|config| config.authorized(session));
        let response = if api_request && !public_read && authorization_count > 1 {
            ApiResponse::error(
                400,
                "duplicate_authorization",
                "multiple Authorization headers are not accepted",
            )
        } else if api_request && !public_read && !authorized {
            ApiResponse::unauthorized()
        } else if let Some(route) = management::match_route(&path_and_query) {
            if let (Some(config), Some(management)) = (&self.config, &self.management) {
                let _ = config;
                management.handle(route, &method, session).await
            } else {
                ApiResponse::route_not_found()
            }
        } else if let Some(route) = config::match_route(&path) {
            if let Some(config) = &self.config {
                config.handle_http(route, &method, session).await
            } else {
                self.handle_at_system_time(&method, &path)
            }
        } else if method != "GET" && streams::match_route(&path).is_some() {
            let route = streams::match_route(&path).expect("matched stream route");
            let Some(generations) = &self.generations else {
                return to_http_response(ApiResponse::error(
                    503,
                    "generation_unavailable",
                    "generation state is unavailable",
                ));
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
                return to_http_response(ApiResponse::error(
                    428,
                    "precondition_required",
                    "If-Generation-Revision is required",
                ));
            };
            let mutation = match generations.begin_mutation(revision) {
                Ok(mutation) => mutation,
                Err(error) => {
                    return to_http_response(ApiResponse::error(
                        409,
                        error.code(),
                        "the active generation revision changed",
                    ));
                }
            };
            match system_time_ms() {
                Ok(now_unix_ms) => streams::handle(
                    route,
                    &method,
                    mutation.generation().registry(),
                    now_unix_ms,
                ),
                Err(response) => response,
            }
        } else {
            self.handle_at_system_time(&method, &path)
        };

        to_http_response(response)
    }
}

fn match_api_route(path: &str) -> Option<ApiRoute<'_>> {
    config::match_route(path)
        .map(ApiRoute::Config)
        .or_else(|| observability::match_route(path).map(ApiRoute::Observability))
        .or_else(|| streams::match_route(path).map(ApiRoute::Stream))
}
