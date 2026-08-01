use std::{io, path::Path, sync::Arc};

use async_trait::async_trait;
use http::{Response, header::AUTHORIZATION};
use pingora::{apps::http_app::ServeHttp, protocols::http::ServerSession};
use serde::Deserialize;

use crate::{
    ApiResponse, GenerationManager, RoundRobinPool, RuntimeMetrics,
    prometheus::render_prometheus,
    rtmp_api::response::to_http_response,
    secure_bearer::{HeaderCardinality, SecureBearerToken, single_header},
};
use oxiroute_rtmp::RtmpRegistry;

pub struct HaproxyStatsApi {
    admin_token: Option<SecureBearerToken>,
    generations: GenerationManager,
    metrics: RuntimeMetrics,
    pools: Vec<Arc<RoundRobinPool>>,
    registry: Arc<RtmpRegistry>,
}

impl HaproxyStatsApi {
    /// Builds one shared stats application for every configured IPv4/IPv6 stats bind.
    ///
    /// # Errors
    ///
    /// Returns a redacted error when the optional admin token cannot be loaded securely.
    pub fn new(
        metrics: RuntimeMetrics,
        pools: Vec<Arc<RoundRobinPool>>,
        registry: Arc<RtmpRegistry>,
        generations: GenerationManager,
        admin_token_file: Option<&Path>,
    ) -> io::Result<Self> {
        Ok(Self {
            admin_token: admin_token_file
                .map(SecureBearerToken::load)
                .transpose()
                .map_err(|_| token_error())?,
            generations,
            metrics,
            pools,
            registry,
        })
    }

    #[must_use]
    pub fn handle(
        &self,
        method: &str,
        path: &str,
        peer_is_loopback: bool,
        authorization: Option<&[u8]>,
        _generation_revision: Option<&str>,
    ) -> ApiResponse {
        match (method, path) {
            ("GET", "/" | "/stats") => {
                if self.authorized(peer_is_loopback, authorization) {
                    self.stats_response()
                } else {
                    ApiResponse::unauthorized()
                }
            }
            ("GET", "/metrics") => self.metrics_response(),
            ("GET", "/ready") => self.readiness_response(),
            ("GET", "/api/v1/status") => {
                if self.authorized(peer_is_loopback, authorization) {
                    self.status_response()
                } else {
                    ApiResponse::unauthorized()
                }
            }
            ("GET" | "HEAD", "/stats/admin") => ApiResponse::method_not_allowed("POST"),
            ("POST", "/stats/admin") => {
                ApiResponse::error(400, "invalid_json", "a JSON admin target is required")
            }
            (_, "/" | "/stats" | "/metrics" | "/ready" | "/api/v1/status") => {
                ApiResponse::method_not_allowed("GET")
            }
            _ => ApiResponse::route_not_found(),
        }
    }

    fn authorized(&self, peer_is_loopback: bool, authorization: Option<&[u8]>) -> bool {
        peer_is_loopback
            && self
                .admin_token
                .as_ref()
                .is_some_and(|token| authorization.is_some_and(|value| token.authorizes(value)))
    }

    fn stats_response(&self) -> ApiResponse {
        let mut body = String::from(
            "<!doctype html><meta charset=utf-8><title>OxiRoute statistics</title>\
             <h1>OxiRoute statistics</h1><table><thead><tr><th>Pool</th><th>Server</th>\
             <th>Administrative state</th><th>Health</th><th>Active</th></tr></thead><tbody>",
        );
        for pool in &self.pools {
            let snapshot = pool.health_snapshot();
            for server in snapshot.endpoints {
                body.push_str("<tr><td>");
                escape_html(&mut body, &snapshot.name);
                body.push_str("</td><td>");
                escape_html(&mut body, &server.name);
                body.push_str("</td><td>");
                body.push_str(match server.administrative_state {
                    crate::AdministrativeState::Ready => "ready",
                    crate::AdministrativeState::Drain => "drain",
                    crate::AdministrativeState::Maintenance => "maintenance",
                });
                body.push_str("</td><td>");
                body.push_str(match server.state {
                    crate::EndpointHealthState::Unchecked => "unchecked",
                    crate::EndpointHealthState::Unknown => "unknown",
                    crate::EndpointHealthState::Healthy => "healthy",
                    crate::EndpointHealthState::Unhealthy => "unhealthy",
                });
                body.push_str("</td><td>");
                body.push_str(&server.active_connections.to_string());
                body.push_str("</td></tr>");
            }
        }
        body.push_str("</tbody></table>");
        ApiResponse::bytes(200, body.into_bytes(), "text/html; charset=utf-8")
    }

    fn metrics_response(&self) -> ApiResponse {
        match render_prometheus(&self.metrics, &self.registry, &self.generations) {
            Ok(body) => ApiResponse::bytes(
                200,
                body.into_bytes(),
                "text/plain; version=0.0.4; charset=utf-8",
            ),
            Err(_) => ApiResponse::error(
                503,
                "metrics_unavailable",
                "runtime metrics could not be sampled",
            ),
        }
    }

    fn readiness_response(&self) -> ApiResponse {
        let status = self.generations.status();
        let listeners_ready = self.metrics.snapshot().is_ok_and(|runtime| {
            runtime.process.administrative_state == crate::AdministrativeState::Ready
                && runtime.listeners.iter().all(|listener| {
                    listener.state == crate::ListenerRuntimeState::Listening
                        && listener.administrative_state == crate::AdministrativeState::Ready
                })
        });
        let ready = status.active_revision.is_some() && !status.degraded && listeners_ready;
        ApiResponse::json(
            if ready { 200 } else { 503 },
            &serde_json::json!({
                "ready": ready,
                "buildVersion": crate::cli::BUILD_VERSION,
                "activeRevision": status.active_revision,
            }),
        )
    }

    fn status_response(&self) -> ApiResponse {
        let status = self.generations.status();
        let listeners = self
            .metrics
            .snapshot()
            .map_or_else(|_| Vec::new(), |runtime| runtime.listeners);
        ApiResponse::json(
            200,
            &serde_json::json!({
                "schemaVersion": 1,
                "buildVersion": status.build_version,
                "diskRevision": status.disk_revision,
                "candidateRevision": status.candidate_revision,
                "activeRevision": status.active_revision,
                "previousRevision": status.previous_revision,
                "degraded": status.degraded,
                "listeners": listeners,
            }),
        )
    }

    fn admin_response(
        &self,
        target: &StatsAdminTarget,
        peer_is_loopback: bool,
        authorization: Option<&[u8]>,
        generation_revision: Option<&str>,
    ) -> ApiResponse {
        if !self.authorized(peer_is_loopback, authorization) {
            return ApiResponse::unauthorized();
        }
        let Some(generation_revision) = generation_revision else {
            return ApiResponse::error(
                428,
                "precondition_required",
                "If-Generation-Revision is required",
            );
        };
        let mutation = match self.generations.begin_mutation(generation_revision) {
            Ok(mutation) => mutation,
            Err(error) => {
                return ApiResponse::error(
                    409,
                    error.code(),
                    "the active generation revision changed",
                );
            }
        };
        let state = match target.action.as_str() {
            "enable" => crate::AdministrativeState::Ready,
            "disable" => crate::AdministrativeState::Maintenance,
            _ => return ApiResponse::error(400, "invalid_admin_action", "invalid admin action"),
        };
        let Some(pool) = mutation
            .generation()
            .plan()
            .pools
            .iter()
            .find(|pool| pool.health_snapshot().name == target.pool)
        else {
            return ApiResponse::error(404, "pool_not_found", "upstream pool was not found");
        };
        if pool
            .set_server_administrative_state(&target.server, state)
            .is_err()
        {
            return ApiResponse::error(404, "server_not_found", "upstream server was not found");
        }
        ApiResponse::bytes(204, Vec::new(), "text/plain; charset=utf-8")
    }
}

#[async_trait]
impl ServeHttp for HaproxyStatsApi {
    async fn response(&self, session: &mut ServerSession) -> Response<Vec<u8>> {
        let request = session.req_header();
        let method = request.method.as_str().to_owned();
        let path = request.uri.path().to_owned();
        let authorization = match single_header(&request.headers, &AUTHORIZATION) {
            HeaderCardinality::Missing => None,
            HeaderCardinality::Single(value) => Some(value.as_bytes().to_vec()),
            HeaderCardinality::Duplicate => {
                return to_http_response(ApiResponse::error(
                    400,
                    "duplicate_authorization",
                    "multiple Authorization headers are not accepted",
                ));
            }
        };
        let mut revisions = request.headers.get_all("if-generation-revision").iter();
        let generation_revision = revisions
            .next()
            .filter(|_| revisions.next().is_none())
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let content_type = request
            .headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim)
            .map(str::to_owned);
        let peer_is_loopback = session
            .client_addr()
            .and_then(|address| address.as_inet())
            .is_some_and(|address| address.ip().is_loopback());
        let response = if method == "POST" && path == "/stats/admin" {
            if content_type.as_deref() == Some("application/json") {
                match crate::rtmp_api::read_config_body(session).await {
                    Ok(body) => match serde_json::from_slice::<StatsAdminTarget>(&body) {
                        Ok(target) => self.admin_response(
                            &target,
                            peer_is_loopback,
                            authorization.as_deref(),
                            generation_revision.as_deref(),
                        ),
                        Err(_) => ApiResponse::error(
                            400,
                            "invalid_json",
                            "the stats admin target is invalid",
                        ),
                    },
                    Err(response) => response,
                }
            } else {
                ApiResponse::error(
                    415,
                    "unsupported_media_type",
                    "application/json is required",
                )
            }
        } else {
            self.handle(
                &method,
                &path,
                peer_is_loopback,
                authorization.as_deref(),
                generation_revision.as_deref(),
            )
        };
        to_http_response(response)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatsAdminTarget {
    pool: String,
    server: String,
    action: String,
}

pub(crate) fn preflight_admin_token(path: Option<&Path>) -> io::Result<()> {
    path.map(SecureBearerToken::load)
        .transpose()
        .map(drop)
        .map_err(|_| token_error())
}

fn token_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "statistics admin token could not be loaded securely",
    )
}

fn escape_html(output: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&#39;"),
            character => output.push(character),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt as _};

    use oxiroute_config::{
        Config, DnsResolutionPolicy, HttpVersionPolicy, UpstreamAlgorithm, UpstreamConnectionReuse,
        UpstreamEndpoint, UpstreamPool, UpstreamServer, render_lua,
    };
    use oxiroute_rtmp::RtmpCapabilities;
    use tempfile::TempDir;

    use super::*;

    fn api() -> (
        HaproxyStatsApi,
        Arc<RoundRobinPool>,
        TempDir,
        String,
        String,
    ) {
        let directory = TempDir::new().expect("directory");
        let token = "a".repeat(64);
        let token_path = directory.path().join("stats.token");
        fs::write(&token_path, format!("{token}\n")).expect("token");
        fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600)).expect("mode");
        let config_path = directory.path().join("oxiroute.lua");
        let config = Config {
            version: 1,
            max_connections: None,
            management: None,
            stats: None,
            certificates: Vec::new(),
            tls_profiles: Vec::new(),
            listeners: Vec::new(),
            cache_stores: Vec::new(),
            upstream_pools: vec![UpstreamPool {
                name: "pool".into(),
                servers: vec![UpstreamServer {
                    name: "origin".into(),
                    endpoint: UpstreamEndpoint::Socket {
                        address: "127.0.0.1:3000".parse().expect("address"),
                    },
                    max_connections: None,
                    dns_resolution: DnsResolutionPolicy::OnConnect,
                }],
                endpoints: Vec::new(),
                algorithm: UpstreamAlgorithm::RoundRobin,
                health_check: None,
                tls: None,
                http_versions: HttpVersionPolicy::default(),
                queue_timeout_ms: None,
                connect_timeout_ms: None,
                server_timeout_ms: None,
                connection_reuse: UpstreamConnectionReuse::Safe,
            }],
            http_services: Vec::new(),
            forward_proxy_services: Vec::new(),
            rtmp_services: Vec::new(),
            l4_services: Vec::new(),
        };
        fs::write(&config_path, render_lua(&config).expect("render")).expect("config");
        let coordinator = crate::config_coordinator::CanonicalConfigCoordinator::new(config_path)
            .expect("coordinator");
        let crate::config_coordinator::ConfigLoadOutcome::Loaded(document) = coordinator.load()
        else {
            panic!("load config")
        };
        let generations = GenerationManager::new();
        let candidate = generations.prepare(*document).expect("prepare");
        let active = generations.activate(&candidate).expect("activate");
        let revision = active.revision().candidate.as_str().to_owned();
        let pool = Arc::clone(&active.plan().pools[0]);
        let api = HaproxyStatsApi::new(
            active.metrics().clone(),
            vec![Arc::clone(&pool)],
            Arc::clone(active.registry()),
            generations,
            Some(&token_path),
        )
        .expect("stats API");
        (api, pool, directory, token, revision)
    }

    #[test]
    fn sensitive_reads_and_admin_require_loopback_token_and_revision() {
        let (api, pool, _directory, token, revision) = api();
        let authorization = format!("Bearer {token}");
        assert_eq!(api.handle("GET", "/stats", false, None, None).status, 401);
        assert_eq!(
            api.handle("GET", "/stats", true, Some(authorization.as_bytes()), None,)
                .status,
            200
        );
        assert_eq!(api.handle("GET", "/metrics", false, None, None).status, 200);
        assert_eq!(api.handle("GET", "/ready", false, None, None).status, 200);
        assert_eq!(
            api.handle("GET", "/api/v1/status", false, None, None)
                .status,
            401
        );
        assert_eq!(
            api.handle(
                "GET",
                "/api/v1/status",
                true,
                Some(authorization.as_bytes()),
                None,
            )
            .status,
            200
        );
        let target = StatsAdminTarget {
            pool: "pool".into(),
            server: "origin".into(),
            action: "disable".into(),
        };
        assert_eq!(
            api.handle("GET", "/stats/admin", true, None, None).status,
            405
        );
        assert_eq!(api.admin_response(&target, false, None, None).status, 401);
        assert_eq!(
            api.admin_response(&target, true, Some(authorization.as_bytes()), None,)
                .status,
            428
        );
        assert_eq!(
            api.admin_response(
                &target,
                true,
                Some(authorization.as_bytes()),
                Some(&revision),
            )
            .status,
            204
        );
        assert_eq!(
            pool.health_snapshot().endpoints[0].administrative_state,
            crate::AdministrativeState::Maintenance
        );
    }

    #[test]
    fn absent_admin_token_keeps_restricted_stats_routes_closed() {
        let registry = Arc::new(RtmpRegistry::new(RtmpCapabilities {
            live_ingest: false,
            manual_recording: false,
        }));
        let api = HaproxyStatsApi::new(
            RuntimeMetrics::new(),
            Vec::new(),
            registry,
            GenerationManager::new(),
            None,
        )
        .expect("stats API without token");

        assert_eq!(api.handle("GET", "/metrics", false, None, None).status, 200);
        assert_eq!(api.handle("GET", "/ready", false, None, None).status, 503);
        for path in ["/", "/stats", "/api/v1/status"] {
            assert_eq!(
                api.handle("GET", path, true, Some(b"Bearer ignored"), None)
                    .status,
                401,
                "{path} must remain closed without a configured token"
            );
        }
    }
}
