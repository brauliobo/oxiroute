#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone)]
pub(super) struct LoweredRoute {
    pub(super) route: HttpRoute,
    pub(super) matcher: RouteMatcher,
    pub(super) target: Option<SectionId>,
    pub(super) sources: Vec<ProvenanceSpan>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RouteMatcher {
    pub(super) host: Option<HttpHostSelector>,
    pub(super) path_prefix: String,
}

impl RouteMatcher {
    pub(super) fn score(&self) -> (u8, usize) {
        (u8::from(self.host.is_some()), self.path_prefix.len())
    }
}

pub(super) fn matchers_overlap(left: &RouteMatcher, right: &RouteMatcher) -> bool {
    let hosts_overlap = match (&left.host, &right.host) {
        (Some(left), Some(right)) => match (host_value(left), host_value(right)) {
            (Some(left), Some(right)) => left == right,
            _ => true,
        },
        _ => true,
    };
    hosts_overlap && raw_prefixes_overlap(&left.path_prefix, &right.path_prefix)
}

pub(super) fn merge_route_matchers(left: &RouteMatcher, right: &RouteMatcher) -> RouteMatcher {
    RouteMatcher {
        host: left.host.clone().or_else(|| right.host.clone()),
        path_prefix: if left.path_prefix == "/" {
            right.path_prefix.clone()
        } else {
            left.path_prefix.clone()
        },
    }
}

fn host_value(selector: &HttpHostSelector) -> Option<&str> {
    match selector {
        HttpHostSelector::NormalizedHost { value } | HttpHostSelector::ExactAuthority { value } => {
            Some(value)
        }
        HttpHostSelector::AsciiCaseInsensitiveExactAuthority { value } => Some(value),
        HttpHostSelector::NginxLeadingWildcard { .. }
        | HttpHostSelector::NginxLeadingDot { .. } => None,
    }
}

fn raw_prefixes_overlap(left: &str, right: &str) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

pub(super) fn deduplicate_routes(routes: Vec<LoweredRoute>) -> Vec<LoweredRoute> {
    let mut deduplicated: Vec<LoweredRoute> = Vec::new();
    for route in routes {
        if let Some(existing) = deduplicated.iter_mut().find(|existing| {
            existing.matcher == route.matcher
                && route_pool(&existing.route) == route_pool(&route.route)
        }) {
            existing.sources.extend(route.sources);
        } else {
            deduplicated.push(route);
        }
    }
    deduplicated
}

pub(super) fn proxy_action(upstream_pool: String, policy: HttpProxyPolicy) -> HttpRouteAction {
    HttpRouteAction::Proxy {
        upstream_pool,
        policy: policy.into(),
    }
}

pub(super) fn is_terminal_rule(rule: &HttpRequestRule) -> bool {
    matches!(
        rule,
        HttpRequestRule::Redirect { .. } | HttpRequestRule::FixedResponse { .. }
    )
}

pub(super) fn has_conditional_terminal_rule(rules: &[EffectiveValue<HttpRequestRule>]) -> bool {
    rules.iter().any(|rule| {
        matches!(
            rule.value,
            HttpRequestRule::FixedResponse {
                condition: Some(_),
                ..
            }
        )
    })
}

pub(super) fn route_pool(route: &HttpRoute) -> Option<&str> {
    match &route.action {
        HttpRouteAction::Proxy { upstream_pool, .. } => Some(upstream_pool),
        _ => None,
    }
}
