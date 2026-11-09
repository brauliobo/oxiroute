//! The page listener application: a fixed route table under the configured prefix, a read-only
//! snapshot for everyone who can reach it, and the form administration for loopback peers.

use std::time::Duration;

use async_trait::async_trait;
use http::{HeaderValue, Response};
use oxiroute_config::{StatsPage, StatsPageAdminPolicy};
use pingora::{apps::http_app::ServeHttp, protocols::http::ServerSession};

use super::{
    admin_form::{
        StatsPageAdminTarget, form_content_type, parse_stats_form, read_stats_form_body,
        request_is_same_origin,
    },
    builder::{MIN_INTERVAL_MS, build_snapshot},
    cache::{Entry, Rendering, SnapshotCache},
};
use crate::{
    ApiResponse, GenerationManager,
    observability::stats::{begin_stats_mutation, transport_ip_is_loopback},
    rtmp::api::response::to_http_response,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PageRoute {
    Page,
    Snapshot,
    Csv,
}

pub struct HaproxyStatsPage {
    admin: StatsPageAdminPolicy,
    cache: SnapshotCache,
    generations: GenerationManager,
    refresh_ms: u64,
    uri_prefix: String,
}

impl HaproxyStatsPage {
    #[must_use]
    pub fn new(page: &StatsPage, generations: GenerationManager) -> Self {
        Self {
            admin: page.admin,
            cache: SnapshotCache::new(Duration::from_millis(MIN_INTERVAL_MS)),
            generations,
            refresh_ms: page.refresh_ms,
            uri_prefix: page.uri_prefix.clone(),
        }
    }

    /// The page, `snapshot.json` and `stats.csv` answer under the prefix; nothing else does.
    fn route(&self, path: &str) -> Option<PageRoute> {
        match path.strip_prefix(self.uri_prefix.trim_end_matches('/'))? {
            "" | "/" => Some(PageRoute::Page),
            "/snapshot.json" => Some(PageRoute::Snapshot),
            "/stats.csv" => Some(PageRoute::Csv),
            _ => None,
        }
    }

    #[must_use]
    pub async fn handle(&self, method: &str, path: &str, peer_is_loopback: bool) -> ApiResponse {
        let Some(route) = self.route(path) else {
            return ApiResponse::route_not_found();
        };
        match method {
            "GET" => self.read(route, peer_is_loopback).await,
            "HEAD" => {
                let mut response = self.read(route, peer_is_loopback).await;
                response.body.clear();
                response
            }
            "POST" if route == PageRoute::Page && self.admin == StatsPageAdminPolicy::Localhost => {
                ApiResponse::error(
                    400,
                    "invalid_form",
                    "a stats page administration form is required",
                )
            }
            _ => ApiResponse::method_not_allowed("GET, HEAD"),
        }
    }

    async fn read(&self, route: PageRoute, peer_is_loopback: bool) -> ApiResponse {
        let Ok(entry) = self
            .cache
            .get(|| build_snapshot(&self.generations, self.refresh_ms))
            .await
        else {
            return ApiResponse::error(
                503,
                "stats_unavailable",
                "runtime statistics could not be sampled",
            );
        };
        let admin = self.admin_offered(&entry, peer_is_loopback);
        let (rendering, content_type) = match route {
            PageRoute::Page => (Rendering::Html { admin }, "text/html; charset=utf-8"),
            PageRoute::Snapshot => (Rendering::Json { admin }, "application/json"),
            PageRoute::Csv => (Rendering::Csv, "text/csv; charset=utf-8"),
        };
        let body = entry.body(rendering, self.refresh_ms, &self.uri_prefix);
        ApiResponse::bytes(200, body.to_vec(), content_type)
    }

    /// Administration is for loopback peers of a page that enables it, and needs a revision to post.
    fn admin_offered(&self, entry: &Entry, peer_is_loopback: bool) -> bool {
        self.admin == StatsPageAdminPolicy::Localhost
            && peer_is_loopback
            && entry.snapshot.generation.active_revision.is_some()
    }

    pub(super) fn admin_response(
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
        self.cache.invalidate();
        ApiResponse::bytes(204, Vec::new(), "text/plain; charset=utf-8")
    }

    async fn post_form(&self, session: &mut ServerSession, peer_is_loopback: bool) -> ApiResponse {
        if self.admin != StatsPageAdminPolicy::Localhost {
            return ApiResponse::method_not_allowed("GET, HEAD");
        }
        let headers = &session.req_header().headers;
        if !form_content_type(headers) {
            return ApiResponse::error(
                415,
                "unsupported_media_type",
                "Content-Type must be application/x-www-form-urlencoded",
            );
        }
        if !request_is_same_origin(headers) {
            return ApiResponse::error(
                403,
                "admin_forbidden",
                "stats page administration requires a same-origin loopback request",
            );
        }
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
        let route = self.route(&path);
        if method == "HEAD" && route.is_some() {
            return secure_head_page_response(self.handle("GET", &path, peer_is_loopback).await);
        }
        let response = if method == "POST" && route == Some(PageRoute::Page) {
            self.post_form(session, peer_is_loopback).await
        } else {
            self.handle(&method, &path, peer_is_loopback).await
        };
        secure_page_response(response)
    }
}

/// No script or style source except the page's own origin; nothing is ever inline.
const PAGE_CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; \
connect-src 'self'; img-src 'self' data:; font-src 'self'; form-action 'self'; base-uri 'none'; \
frame-ancestors 'none'";

pub(super) fn secure_page_response(response: ApiResponse) -> Response<Vec<u8>> {
    let mut response = to_http_response(response);
    for (name, value) in [
        ("cache-control", "no-store"),
        ("content-security-policy", PAGE_CONTENT_SECURITY_POLICY),
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
