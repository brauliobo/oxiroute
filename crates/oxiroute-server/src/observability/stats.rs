use std::{io, path::Path, sync::Arc};

use async_trait::async_trait;
use http::{Response, header::AUTHORIZATION};
use pingora::{apps::http_app::ServeHttp, protocols::http::ServerSession};
use serde::Deserialize;
use serde_json::Value;

use crate::generation::health::generation_component_status;
use crate::html::escape_html;
use crate::{
    ApiResponse, GenerationManager, RoundRobinPool, RuntimeMetrics,
    config::coordinator::EffectiveRevision,
    observability::prometheus::render_prometheus_control,
    rtmp::api::response::to_http_response,
    secure_bearer::{HeaderCardinality, SecureBearerToken, single_header},
};
use oxiroute_rtmp::RtmpControlHandle;

pub struct HaproxyStatsApi {
    admin_token: Option<SecureBearerToken>,
    generations: GenerationManager,
    metrics: RuntimeMetrics,
    pools: Vec<Arc<RoundRobinPool>>,
    control: RtmpControlHandle,
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
        control: RtmpControlHandle,
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
            control,
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
            ("GET", "/status.json") => crate::observability::status_summary::status_json_response(
                &self.metrics,
                Some(&self.generations),
            ),
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
            (_, "/" | "/stats" | "/metrics" | "/ready" | "/status.json" | "/api/v1/status") => {
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
        match render_prometheus_control(&self.metrics, &self.control, &self.generations) {
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
        let ready = status.serving() && listeners_ready;
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
        let runtime = self.metrics.snapshot().ok();
        let listeners = runtime
            .as_ref()
            .map_or_else(Vec::new, |runtime| runtime.listeners.clone());
        let audit = crate::observability::operational_event::audit_status();
        let components = runtime.as_ref().map_or_else(
            || {
                serde_json::json!({
                    "process": { "state": "degraded", "reason": "runtime_sampling_failed" },
                    "host": { "state": "degraded", "reason": "runtime_sampling_failed" },
                    "generation": generation_component_status(Some(&status)),
                    "audit": audit.clone(),
                })
            },
            |runtime| {
                serde_json::json!({
                    "process": runtime.process.status,
                    "host": runtime.host.status,
                    "generation": generation_component_status(Some(&status)),
                    "audit": audit.clone(),
                })
            },
        );
        let certificates = runtime.as_ref().map_or_else(
            || {
                serde_json::json!({
                    "certbot": [],
                    "acmeManaged": [],
                    "directFiles": [],
                })
            },
            |runtime| {
                serde_json::json!({
                    "certbot": runtime.certbot_certificates,
                    "acmeManaged": runtime.acme_managed_certificates,
                    "directFiles": runtime.direct_file_certificates,
                })
            },
        );
        let active_generation_age_ms = runtime.as_ref().map_or(Value::Null, |runtime| {
            serde_json::json!(runtime.generation_age_ms)
        });
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
                "activeGenerationAgeMs": active_generation_age_ms,
                "components": components,
                "certificates": certificates,
                "audit": audit,
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
        let mutation = match begin_stats_mutation(&self.generations, generation_revision) {
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
            .pools()
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
            .is_some_and(|address| transport_ip_is_loopback(address.ip()));
        let response = if method == "POST" && path == "/stats/admin" {
            if content_type.as_deref() == Some("application/json") {
                match crate::rtmp::api::read_config_body(session).await {
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

pub(super) fn transport_ip_is_loopback(address: std::net::IpAddr) -> bool {
    address.is_loopback()
        || match address {
            std::net::IpAddr::V6(address) => address
                .to_ipv4_mapped()
                .is_some_and(|address| address.is_loopback()),
            std::net::IpAddr::V4(_) => false,
        }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatsAdminTarget {
    pool: String,
    server: String,
    action: String,
}

pub(super) fn begin_stats_mutation(
    generations: &GenerationManager,
    expected_revision: &str,
) -> Result<crate::GenerationMutation, crate::GenerationError> {
    let expected_revision: EffectiveRevision = expected_revision
        .parse()
        .map_err(|_| crate::GenerationError::RevisionConflict)?;
    generations.begin_mutation(&expected_revision)
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

#[cfg(test)]
mod tests;
