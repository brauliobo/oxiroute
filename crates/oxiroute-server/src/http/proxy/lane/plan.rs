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
            _ if action
                .direct_response()
                .is_some_and(|response| response.rendered_head.is_some()) =>
            {
                Self::Direct
            }
            HttpActionPlan::Proxy(proxy) if forwards_plainly(proxy, policy) => Self::Proxy,
            _ => Self::Classic,
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

#[cfg(test)]
mod tests {
    use oxiroute_config::{HttpLiteralHeader, HttpRedirectLocation, HttpRoutePolicy};

    use super::*;
    use crate::http::action::{FixedResponsePlan, RedirectPlan};

    fn header(name: &str, value: &str) -> HttpLiteralHeader {
        HttpLiteralHeader {
            name: name.into(),
            value: value.into(),
            always: true,
        }
    }

    fn literal(value: &str) -> HttpRedirectLocation {
        HttpRedirectLocation::Literal {
            value: value.into(),
        }
    }

    fn lane_of(action: &HttpActionPlan) -> LaneRoute {
        LaneRoute::compile(
            action,
            &RoutePolicyPlan::compile(HttpRoutePolicy::default()),
            None,
        )
    }

    fn redirect(location: &HttpRedirectLocation, headers: &[HttpLiteralHeader]) -> HttpActionPlan {
        HttpActionPlan::Redirect(RedirectPlan::compile(301, location, headers))
    }

    #[test]
    fn a_literal_redirect_is_served_like_the_fixed_response_it_is() {
        let action = redirect(&literal("https://example.com/"), &[header("x-a", "1")]);
        assert_eq!(lane_of(&action), LaneRoute::Direct);
        let fixed = FixedResponsePlan::compile(
            301,
            "",
            &[
                header("x-a", "1"),
                header("location", "https://example.com/"),
            ],
        );
        let served = action.direct_response().expect("a direct response");
        assert_eq!(served.rendered_head, fixed.rendered_head);
        assert!(served.body.is_empty());
        assert_eq!(served.status, 301);
    }

    #[test]
    fn a_redirect_that_depends_on_the_request_stays_on_the_classic_path() {
        let template = HttpRedirectLocation::RequestTemplate {
            value: "https://$host$request_uri".into(),
            nginx_host_fallback: None,
        };
        assert_eq!(lane_of(&redirect(&template, &[])), LaneRoute::Classic);
    }

    #[test]
    fn a_redirect_with_an_nginx_error_page_body_stays_on_the_classic_path() {
        let headers = [
            header("content-type", "text/html"),
            header("server", "nginx"),
        ];
        let action = redirect(&literal("https://example.com/"), &headers);
        assert_eq!(lane_of(&action), LaneRoute::Classic);
    }
}
