use std::{io, path::Path, sync::Arc};

use super::{
    ApiResponse,
    config::{self, ConfigApiState, Route as ConfigRoute},
    observability::{self, Route as ObservabilityRoute},
    response::{system_time_ms, to_http_response},
    streams::{self, Route as StreamRoute},
    ui::UiAssets,
};
use crate::{
    RuntimeMetrics, TopologySnapshot,
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
            metrics,
            registry,
            topology,
            ui: Some(UiAssets::load(directory.as_ref())?),
        })
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
        let (method, path) = {
            let request = session.req_header();
            (
                request.method.as_str().to_owned(),
                request.uri.path().to_owned(),
            )
        };

        let response = if let Some(route) = config::match_route(&path) {
            if let Some(config) = &self.config {
                config.handle_http(route, &method, session).await
            } else {
                self.handle_at_system_time(&method, &path)
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
