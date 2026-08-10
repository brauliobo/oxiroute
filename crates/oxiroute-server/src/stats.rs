use std::{collections::HashMap, io, net::IpAddr, path::Path, sync::Arc};

use async_trait::async_trait;
use http::{
    HeaderMap, HeaderValue, Response, Uri,
    header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HOST, ORIGIN, REFERER},
    uri::Authority,
};
use oxiroute_config::{StatsPage, StatsPageAdminPolicy};
use pingora::{apps::http_app::ServeHttp, protocols::http::ServerSession};
use serde::Deserialize;
use serde_json::Value;

use crate::html::escape_html;
use crate::{
    ApiResponse, GenerationManager, RoundRobinPool, RuntimeMetrics,
    prometheus::render_prometheus,
    rtmp_api::response::to_http_response,
    secure_bearer::{HeaderCardinality, SecureBearerToken, single_header},
};
use oxiroute_rtmp::RtmpRegistry;

const MAX_STATS_FORM_BYTES: usize = 8 * 1024;

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
        let runtime = self.metrics.snapshot().ok();
        let listeners = runtime
            .as_ref()
            .map_or_else(Vec::new, |runtime| runtime.listeners.clone());
        let audit = crate::operational_event::audit_status();
        let components = runtime.as_ref().map_or_else(
            || {
                serde_json::json!({
                    "process": { "state": "degraded", "reason": "runtime_sampling_failed" },
                    "host": { "state": "degraded", "reason": "runtime_sampling_failed" },
                    "generation": generation_component_status(&status),
                    "audit": audit.clone(),
                })
            },
            |runtime| {
                serde_json::json!({
                    "process": runtime.process.status,
                    "host": runtime.host.status,
                    "generation": generation_component_status(&status),
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

fn generation_component_status(status: &crate::GenerationStatus) -> Value {
    if status.degraded {
        serde_json::json!({
            "state": "degraded",
            "reason": status.last_failure,
        })
    } else if status.active_revision.is_some() {
        serde_json::json!({ "state": "healthy" })
    } else {
        serde_json::json!({
            "state": "degraded",
            "reason": "active_generation_unavailable",
        })
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

pub struct HaproxyStatsPage {
    admin: StatsPageAdminPolicy,
    generations: GenerationManager,
    pools: Vec<Arc<RoundRobinPool>>,
    refresh_ms: u64,
    uri_prefix: String,
}

impl HaproxyStatsPage {
    #[must_use]
    pub fn new(
        page: &StatsPage,
        pools: Vec<Arc<RoundRobinPool>>,
        generations: GenerationManager,
    ) -> Self {
        Self {
            admin: page.admin,
            generations,
            pools,
            refresh_ms: page.refresh_ms,
            uri_prefix: page.uri_prefix.clone(),
        }
    }

    #[must_use]
    pub fn handle(&self, method: &str, path: &str, peer_is_loopback: bool) -> ApiResponse {
        if !path.starts_with(&self.uri_prefix) {
            return ApiResponse::route_not_found();
        }
        match method {
            "GET" => self.page_response(peer_is_loopback, false),
            "HEAD" => self.page_response(peer_is_loopback, true),
            "POST" if self.admin == StatsPageAdminPolicy::Localhost => ApiResponse::error(
                400,
                "invalid_form",
                "a stats page administration form is required",
            ),
            _ => ApiResponse::method_not_allowed("GET, HEAD"),
        }
    }

    fn page_response(&self, peer_is_loopback: bool, head: bool) -> ApiResponse {
        let revision = self
            .generations
            .status()
            .active_revision
            .map(|revision| revision.as_str().to_owned());
        let show_admin =
            self.admin == StatsPageAdminPolicy::Localhost && peer_is_loopback && revision.is_some();
        let mut body = String::from(
            "<!doctype html><html><head><meta charset=utf-8><title>OxiRoute statistics</title>",
        );
        body.push_str("<meta name=oxiroute-refresh-ms content=\"");
        body.push_str(&self.refresh_ms.to_string());
        body.push_str("\"><meta http-equiv=refresh content=\"");
        body.push_str(&format_refresh_seconds(self.refresh_ms));
        body.push_str("\"></head><body><h1>OxiRoute statistics</h1><table><thead><tr><th>Pool</th><th>Server</th><th>Administrative state</th><th>Health</th><th>Active</th>");
        if show_admin {
            body.push_str("<th>Administration</th>");
        }
        body.push_str("</tr></thead><tbody>");
        for pool in &self.pools {
            let snapshot = pool.health_snapshot();
            for server in snapshot.endpoints {
                body.push_str("<tr><td>");
                escape_html(&mut body, &snapshot.name);
                body.push_str("</td><td>");
                escape_html(&mut body, &server.name);
                body.push_str("</td><td>");
                body.push_str(administrative_state_name(server.administrative_state));
                body.push_str("</td><td>");
                body.push_str(health_state_name(server.state));
                body.push_str("</td><td>");
                body.push_str(&server.active_connections.to_string());
                body.push_str("</td>");
                if let Some(revision) = revision.as_deref().filter(|_| show_admin) {
                    body.push_str("<td><form method=post action=\"");
                    escape_html(&mut body, &self.uri_prefix);
                    body.push_str("\"><input type=hidden name=generation_revision value=\"");
                    escape_html(&mut body, revision);
                    body.push_str("\"><input type=hidden name=pool value=\"");
                    escape_html(&mut body, &snapshot.name);
                    body.push_str("\"><input type=hidden name=server value=\"");
                    escape_html(&mut body, &server.name);
                    body.push_str("\"><button name=state value=ready>Ready</button><button name=state value=drain>Drain</button><button name=state value=maintenance>Maintenance</button></form></td>");
                }
                body.push_str("</tr>");
            }
        }
        body.push_str("</tbody></table></body></html>");
        ApiResponse::bytes(
            200,
            if head { Vec::new() } else { body.into_bytes() },
            "text/html; charset=utf-8",
        )
    }

    fn admin_response(
        &self,
        target: &StatsPageAdminTarget,
        peer_is_loopback: bool,
        same_origin: bool,
    ) -> ApiResponse {
        if self.admin != StatsPageAdminPolicy::Localhost || !peer_is_loopback || !same_origin {
            return ApiResponse::error(
                403,
                "admin_forbidden",
                "stats page administration requires a same-origin loopback request",
            );
        }
        let mutation = match self.generations.begin_mutation(&target.generation_revision) {
            Ok(mutation) => mutation,
            Err(error) => {
                return ApiResponse::error(
                    409,
                    error.code(),
                    "the active generation revision changed",
                );
            }
        };
        let state = match target.state.as_str() {
            "ready" => crate::AdministrativeState::Ready,
            "drain" => crate::AdministrativeState::Drain,
            "maintenance" => crate::AdministrativeState::Maintenance,
            _ => return ApiResponse::error(400, "invalid_admin_state", "invalid admin state"),
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
impl ServeHttp for HaproxyStatsPage {
    async fn response(&self, session: &mut ServerSession) -> Response<Vec<u8>> {
        let request = session.req_header();
        let method = request.method.as_str().to_owned();
        let path = request.uri.path().to_owned();
        let peer_is_loopback = session
            .client_addr()
            .and_then(|address| address.as_inet())
            .is_some_and(|address| transport_ip_is_loopback(address.ip()));
        if method == "HEAD" && path.starts_with(&self.uri_prefix) {
            return secure_head_page_response(self.page_response(peer_is_loopback, false));
        }
        let response = if method == "POST" && path.starts_with(&self.uri_prefix) {
            if self.admin != StatsPageAdminPolicy::Localhost {
                ApiResponse::method_not_allowed("GET, HEAD")
            } else if !form_content_type(&request.headers) {
                ApiResponse::error(
                    415,
                    "unsupported_media_type",
                    "Content-Type must be application/x-www-form-urlencoded",
                )
            } else if !request_is_same_origin(&request.headers) {
                ApiResponse::error(
                    403,
                    "admin_forbidden",
                    "stats page administration requires a same-origin loopback request",
                )
            } else {
                match read_stats_form_body(session).await {
                    Ok(body) => match parse_stats_form(&body) {
                        Ok(target) => self.admin_response(&target, peer_is_loopback, true),
                        Err(()) => ApiResponse::error(
                            400,
                            "invalid_form",
                            "the stats page administration form is invalid",
                        ),
                    },
                    Err(response) => response,
                }
            }
        } else {
            self.handle(&method, &path, peer_is_loopback)
        };
        secure_page_response(response)
    }
}

struct StatsPageAdminTarget {
    generation_revision: String,
    pool: String,
    server: String,
    state: String,
}

fn administrative_state_name(state: crate::AdministrativeState) -> &'static str {
    match state {
        crate::AdministrativeState::Ready => "ready",
        crate::AdministrativeState::Drain => "drain",
        crate::AdministrativeState::Maintenance => "maintenance",
    }
}

fn transport_ip_is_loopback(address: std::net::IpAddr) -> bool {
    address.is_loopback()
        || match address {
            std::net::IpAddr::V6(address) => address
                .to_ipv4_mapped()
                .is_some_and(|address| address.is_loopback()),
            std::net::IpAddr::V4(_) => false,
        }
}

fn health_state_name(state: crate::EndpointHealthState) -> &'static str {
    match state {
        crate::EndpointHealthState::Unchecked => "unchecked",
        crate::EndpointHealthState::Unknown => "unknown",
        crate::EndpointHealthState::Healthy => "healthy",
        crate::EndpointHealthState::Unhealthy => "unhealthy",
    }
}

fn format_refresh_seconds(refresh_ms: u64) -> String {
    if refresh_ms.is_multiple_of(1_000) {
        (refresh_ms / 1_000).to_string()
    } else {
        format!("{}.{:03}", refresh_ms / 1_000, refresh_ms % 1_000)
    }
}

fn form_content_type(headers: &HeaderMap) -> bool {
    matches!(
        single_header(headers, &CONTENT_TYPE),
        HeaderCardinality::Single(value)
            if value
                .to_str()
                .ok()
                .and_then(|value| value.split(';').next())
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/x-www-form-urlencoded"))
    )
}

fn request_is_same_origin(headers: &HeaderMap) -> bool {
    if [
        "forwarded",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
        "x-real-ip",
        "x-client-ip",
    ]
    .iter()
    .any(|name| headers.contains_key(*name))
    {
        return false;
    }
    let HeaderCardinality::Single(host) = single_header(headers, &HOST) else {
        return false;
    };
    let Ok(host) = host.to_str() else {
        return false;
    };
    let Ok(host) = host.parse::<Authority>() else {
        return false;
    };
    if !authority_is_loopback(&host) {
        return false;
    }
    match single_header(headers, &ORIGIN) {
        HeaderCardinality::Single(origin) => origin
            .to_str()
            .ok()
            .and_then(|origin| origin.parse::<Uri>().ok())
            .is_some_and(|origin| {
                origin.scheme_str() == Some("http")
                    && origin.query().is_none()
                    && origin.path() == "/"
                    && origin
                        .authority()
                        .is_some_and(|origin| origin.as_str().eq_ignore_ascii_case(host.as_str()))
            }),
        HeaderCardinality::Missing => matches!(
            single_header(headers, &REFERER),
            HeaderCardinality::Single(referer)
                if referer
                    .to_str()
                    .ok()
                    .and_then(|referer| referer.parse::<Uri>().ok())
                    .is_some_and(|referer| {
                        referer.scheme_str() == Some("http")
                            && referer.authority().is_some_and(|referer| {
                                referer.as_str().eq_ignore_ascii_case(host.as_str())
                            })
                    })
        ),
        HeaderCardinality::Duplicate => false,
    }
}

fn authority_is_loopback(authority: &Authority) -> bool {
    if authority.as_str().contains('@') || !authority_has_valid_optional_port(authority.as_str()) {
        return false;
    }
    let host = authority.host();
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(transport_ip_is_loopback)
}

fn authority_has_valid_optional_port(authority: &str) -> bool {
    let port = if let Some(bracket) = authority.strip_prefix('[') {
        let Some(end) = bracket.find(']') else {
            return false;
        };
        let remainder = &bracket[end + 1..];
        if remainder.is_empty() {
            return true;
        }
        remainder.strip_prefix(':')
    } else {
        match authority.matches(':').count() {
            0 => return true,
            1 => authority.rsplit_once(':').map(|(_, port)| port),
            _ => return false,
        }
    };
    port.is_some_and(|port| !port.is_empty() && port.parse::<u16>().is_ok())
}

async fn read_stats_form_body(session: &mut ServerSession) -> Result<Vec<u8>, ApiResponse> {
    let mut content_lengths = session.req_header().headers.get_all(CONTENT_LENGTH).iter();
    let declared_length = content_lengths
        .next()
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok());
    if content_lengths.next().is_some() || declared_length.is_none() {
        return Err(ApiResponse::error(
            400,
            "invalid_content_length",
            "exactly one decimal Content-Length value is required",
        ));
    }
    if declared_length.is_some_and(|length| length > MAX_STATS_FORM_BYTES) {
        return Err(stats_form_too_large());
    }
    let mut body = Vec::with_capacity(declared_length.unwrap_or_default());
    while let Some(chunk) = session.read_request_body().await.map_err(|_| {
        ApiResponse::error(
            400,
            "invalid_request_body",
            "request body could not be read",
        )
    })? {
        if chunk.len() > MAX_STATS_FORM_BYTES.saturating_sub(body.len()) {
            return Err(stats_form_too_large());
        }
        body.extend_from_slice(&chunk);
    }
    if body.len() != declared_length.unwrap_or_default() {
        return Err(ApiResponse::error(
            400,
            "invalid_request_body",
            "request body length does not match Content-Length",
        ));
    }
    Ok(body)
}

fn stats_form_too_large() -> ApiResponse {
    ApiResponse::error(
        413,
        "form_too_large",
        format!("stats administration form exceeds the {MAX_STATS_FORM_BYTES}-byte limit"),
    )
}

fn parse_stats_form(body: &[u8]) -> Result<StatsPageAdminTarget, ()> {
    let mut fields = HashMap::new();
    for pair in body.split(|byte| *byte == b'&') {
        let separator = pair.iter().position(|byte| *byte == b'=').ok_or(())?;
        let (name, value) = (&pair[..separator], &pair[separator + 1..]);
        let name = decode_form_component(name)?;
        if !matches!(
            name.as_str(),
            "generation_revision" | "pool" | "server" | "state"
        ) || fields.insert(name, decode_form_component(value)?).is_some()
        {
            return Err(());
        }
    }
    Ok(StatsPageAdminTarget {
        generation_revision: fields
            .remove("generation_revision")
            .filter(|v| !v.is_empty())
            .ok_or(())?,
        pool: fields.remove("pool").filter(|v| !v.is_empty()).ok_or(())?,
        server: fields
            .remove("server")
            .filter(|v| !v.is_empty())
            .ok_or(())?,
        state: fields.remove("state").filter(|v| !v.is_empty()).ok_or(())?,
    })
}

fn decode_form_component(value: &[u8]) -> Result<String, ()> {
    let mut decoded = Vec::with_capacity(value.len());
    let mut index = 0;
    while index < value.len() {
        match value[index] {
            b'+' => decoded.push(b' '),
            b'%' => {
                let high = *value.get(index + 1).ok_or(())?;
                let low = *value.get(index + 2).ok_or(())?;
                decoded.push(form_hex(high)? << 4 | form_hex(low)?);
                index += 2;
            }
            byte => decoded.push(byte),
        }
        index += 1;
    }
    let decoded = String::from_utf8(decoded).map_err(|_| ())?;
    if decoded.chars().any(char::is_control) {
        return Err(());
    }
    Ok(decoded)
}

const fn form_hex(value: u8) -> Result<u8, ()> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(()),
    }
}

fn secure_page_response(response: ApiResponse) -> Response<Vec<u8>> {
    let mut response = to_http_response(response);
    for (name, value) in [
        ("cache-control", "no-store"),
        (
            "content-security-policy",
            "default-src 'none'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'",
        ),
        ("referrer-policy", "no-referrer"),
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
    ] {
        response
            .headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }
    response
}

fn secure_head_page_response(response: ApiResponse) -> Response<Vec<u8>> {
    let mut response = secure_page_response(response);
    response.body_mut().clear();
    response
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
                passive_health: None,
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

    #[test]
    fn page_routes_are_public_read_only_and_isolated_from_protected_apis() {
        let (api, _pool, _directory, _token, _revision) = api();
        let config = StatsPage {
            bind: "127.0.0.1:8404".parse().expect("bind"),
            uri_prefix: "/haproxy".into(),
            refresh_ms: 2_500,
            admin: StatsPageAdminPolicy::Localhost,
            max_connections: None,
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        };
        let page = HaproxyStatsPage::new(&config, api.pools.clone(), api.generations.clone());

        let remote = page.handle("GET", "/haproxy", false);
        let remote_body = String::from_utf8(remote.body).expect("HTML");
        assert_eq!(remote.status, 200);
        assert!(remote_body.contains("name=oxiroute-refresh-ms content=\"2500\""));
        assert!(remote_body.contains("<td>pool</td><td>origin</td>"));
        assert!(!remote_body.contains("<form"));
        assert_eq!(page.handle("GET", "/haproxy/servers", false).status, 200);
        assert_eq!(page.handle("GET", "/haprox", false).status, 404);

        let local = page.handle("GET", "/haproxy", true);
        assert!(
            String::from_utf8(local.body)
                .unwrap()
                .contains("<form method=post")
        );
        assert!(page.handle("HEAD", "/haproxy", true).body.is_empty());
        for path in ["/metrics", "/ready", "/api/v1/status", "/stats"] {
            assert_eq!(page.handle("GET", path, true).status, 404, "{path}");
        }
        assert_eq!(page.handle("PUT", "/haproxy", true).status, 405);

        let secured = secure_page_response(page.handle("GET", "/haproxy", false));
        assert_eq!(secured.headers()["cache-control"], "no-store");
        assert_eq!(secured.headers()["x-content-type-options"], "nosniff");
        assert!(secured.headers().contains_key("content-security-policy"));
    }

    #[test]
    fn localhost_page_admin_requires_origin_and_revision_and_supports_all_states() {
        let (api, pool, _directory, _token, revision) = api();
        let config = StatsPage {
            bind: "127.0.0.1:8404".parse().expect("bind"),
            uri_prefix: "/haproxy".into(),
            refresh_ms: 10_000,
            admin: StatsPageAdminPolicy::Localhost,
            max_connections: None,
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        };
        let page = HaproxyStatsPage::new(&config, api.pools.clone(), api.generations.clone());
        let mut target = StatsPageAdminTarget {
            generation_revision: revision,
            pool: "pool".into(),
            server: "origin".into(),
            state: "drain".into(),
        };

        assert_eq!(page.admin_response(&target, false, true).status, 403);
        assert_eq!(page.admin_response(&target, true, false).status, 403);
        assert_eq!(page.admin_response(&target, true, true).status, 204);
        assert_eq!(
            pool.health_snapshot().endpoints[0].administrative_state,
            crate::AdministrativeState::Drain
        );
        target.state = "maintenance".into();
        assert_eq!(page.admin_response(&target, true, true).status, 204);
        target.state = "ready".into();
        assert_eq!(page.admin_response(&target, true, true).status, 204);
        assert_eq!(
            pool.health_snapshot().endpoints[0].administrative_state,
            crate::AdministrativeState::Ready
        );
        target.generation_revision = "stale".into();
        assert_eq!(page.admin_response(&target, true, true).status, 409);
    }

    #[test]
    fn stats_page_form_and_same_origin_checks_fail_closed() {
        let target = parse_stats_form(
            b"generation_revision=abc&pool=pool%20one&server=node%2B1&state=maintenance",
        )
        .expect("form");
        assert_eq!(target.pool, "pool one");
        assert_eq!(target.server, "node+1");
        assert!(parse_stats_form(b"pool=one&pool=two").is_err());
        assert!(
            parse_stats_form(b"generation_revision=abc&pool=one&server=two&state=ready&extra=no")
                .is_err()
        );

        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static("localhost:8404"));
        headers.insert(ORIGIN, HeaderValue::from_static("http://localhost:8404"));
        assert!(request_is_same_origin(&headers));
        headers.remove(ORIGIN);
        headers.insert(
            REFERER,
            HeaderValue::from_static("http://localhost:8404/stats"),
        );
        assert!(request_is_same_origin(&headers));
        headers.insert(ORIGIN, HeaderValue::from_static("http://localhost:8404"));
        headers.insert("forwarded", HeaderValue::from_static("for=127.0.0.1"));
        assert!(!request_is_same_origin(&headers));
        headers.remove("forwarded");
        headers.insert(ORIGIN, HeaderValue::from_static("http://attacker.test"));
        assert!(!request_is_same_origin(&headers));
        headers.insert(HOST, HeaderValue::from_static("attacker.test:8404"));
        headers.insert(
            ORIGIN,
            HeaderValue::from_static("http://attacker.test:8404"),
        );
        assert!(!request_is_same_origin(&headers));
        headers.insert(HOST, HeaderValue::from_static("127.0.0.1:8404"));
        headers.insert(ORIGIN, HeaderValue::from_static("http://127.0.0.1:8404"));
        assert!(request_is_same_origin(&headers));
        headers.insert(HOST, HeaderValue::from_static("localhost:not-a-port"));
        headers.insert(
            ORIGIN,
            HeaderValue::from_static("http://localhost:not-a-port"),
        );
        assert!(!request_is_same_origin(&headers));
        assert!(authority_is_loopback(
            &"[::1]:8404".parse().expect("IPv6 loopback authority")
        ));
        assert!(!authority_is_loopback(
            &"attacker@localhost:8404"
                .parse()
                .expect("userinfo authority")
        ));
    }

    #[test]
    fn html_escaping_covers_text_and_attribute_delimiters() {
        let mut output = String::new();
        escape_html(&mut output, "<&>\"'");
        assert_eq!(output, "&lt;&amp;&gt;&quot;&#39;");
    }

    #[test]
    fn stats_transport_loopback_accepts_ipv4_mapped_loopback_only() {
        assert!(transport_ip_is_loopback(
            "::ffff:127.0.0.1".parse().unwrap()
        ));
        assert!(!transport_ip_is_loopback(
            "::ffff:192.0.2.1".parse().unwrap()
        ));

        let (api, _pool, _directory, token, _revision) = api();
        let authorization = format!("Bearer {token}");
        assert!(api.authorized(
            transport_ip_is_loopback("::ffff:127.0.0.1".parse().unwrap()),
            Some(authorization.as_bytes())
        ));
    }
}
