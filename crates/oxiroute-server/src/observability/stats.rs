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

const MAX_STATS_FORM_BYTES: usize = 8 * 1024;

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
        let mutation = match begin_stats_mutation(&self.generations, &target.generation_revision) {
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
                    Err(response) => *response,
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

async fn read_stats_form_body(session: &mut ServerSession) -> Result<Vec<u8>, Box<ApiResponse>> {
    let mut content_lengths = session.req_header().headers.get_all(CONTENT_LENGTH).iter();
    let declared_length = content_lengths
        .next()
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok());
    if content_lengths.next().is_some() || declared_length.is_none() {
        return Err(Box::new(ApiResponse::error(
            400,
            "invalid_content_length",
            "exactly one decimal Content-Length value is required",
        )));
    }
    if declared_length.is_some_and(|length| length > MAX_STATS_FORM_BYTES) {
        return Err(stats_form_too_large().into());
    }
    let mut body = Vec::with_capacity(declared_length.unwrap_or_default());
    while let Some(chunk) = session.read_request_body().await.map_err(|_| {
        Box::new(ApiResponse::error(
            400,
            "invalid_request_body",
            "request body could not be read",
        ))
    })? {
        if chunk.len() > MAX_STATS_FORM_BYTES.saturating_sub(body.len()) {
            return Err(stats_form_too_large().into());
        }
        body.extend_from_slice(&chunk);
    }
    if body.len() != declared_length.unwrap_or_default() {
        return Err(Box::new(ApiResponse::error(
            400,
            "invalid_request_body",
            "request body length does not match Content-Length",
        )));
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

fn begin_stats_mutation(
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
