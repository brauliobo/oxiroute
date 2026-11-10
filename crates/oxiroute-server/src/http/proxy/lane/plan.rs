//! What a route compiles to for the fast lane: the lane serves a request only when its route is plain enough for the
//! lane to reproduce the classic path exactly, and the route says so once, when its generation is built.

use crate::http::action::{
    HttpActionPlan, ProxyActionPlan, ProxyPolicyPlan, RouteAccess, RoutePolicyPlan,
};

/// How a request that matched the route is served.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LaneRoute {
    /// The classic path: Pingora's session and the proxy hooks.
    Classic,
    /// A fixed response written from a rendered head.
    Direct,
    /// A request without a body forwarded as it is to a plain HTTP/1.1 server of a pool: no header rules, no
    /// retries, no cache, no rewriting.
    Proxy,
}

impl LaneRoute {
    pub(crate) fn compile(
        action: &HttpActionPlan,
        policy: &RoutePolicyPlan,
        access: Option<&RouteAccess>,
    ) -> Self {
        if access.is_some() {
            return Self::Classic;
        }
        match action {
            HttpActionPlan::Fixed(response) if response.rendered_head.is_some() => Self::Direct,
            HttpActionPlan::Proxy(proxy) if forwards_plainly(proxy, policy) => Self::Proxy,
            HttpActionPlan::Fixed(_)
            | HttpActionPlan::Proxy(_)
            | HttpActionPlan::Redirect(_)
            | HttpActionPlan::Static(_) => Self::Classic,
        }
    }
}

fn forwards_plainly(proxy: &ProxyActionPlan, route: &RoutePolicyPlan) -> bool {
    let policy: &ProxyPolicyPlan = &proxy.policy;
    proxy.pool.is_plain_http1()
        && proxy.select.is_none()
        && proxy.rescue_pool.is_none()
        && !route.request_buffering
        && !route.response_buffering
        && policy.upstream_path_rewrite.is_none()
        && policy.request_headers.is_empty()
        && policy.response_headers.is_empty()
        && policy.cookie_path_rewrites.is_empty()
        && policy.cookie_attributes.is_empty()
        && policy.max_retries == 0
        && policy.retry_response_statuses.is_empty()
        && policy.rescue_statuses.is_empty()
        && policy.retry_budget.is_none()
        && policy.cache.is_none()
        && policy.affinity.is_none()
        && policy.queue_class.is_none()
        && !policy.queue_headers
        && policy.nginx_error_server.is_none()
}
