#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn stable_id(kind: &str, components: &[&str]) -> String {
    let capacity = kind.len()
        + components
            .iter()
            .map(|component| component.len() + 24)
            .sum::<usize>();
    let mut id = String::with_capacity(capacity);
    id.push_str(kind);
    for component in components {
        write!(id, ":{}:{component}", component.len()).expect("writing to a String cannot fail");
    }
    id
}

pub(super) fn listener_id(name: &str, kind: TopologyNodeKind) -> String {
    let prefix = if kind == TopologyNodeKind::RtmpListener {
        "rtmp_listener"
    } else {
        "listener"
    };
    stable_id(prefix, &[name])
}

pub(super) fn tls_profile_id(name: &str) -> String {
    stable_id("tls_profile", &[name])
}

pub(super) fn certificate_id(name: &str) -> String {
    stable_id("certificate", &[name])
}

pub(super) fn http_service_id(name: &str) -> String {
    stable_id("http_service", &[name])
}

pub(super) fn forward_proxy_service_id(name: &str) -> String {
    stable_id("forward_proxy_service", &[name])
}

pub(super) fn http_route_id(service: &str, route: &HttpRoute) -> String {
    let mut methods = route.methods.clone();
    methods.sort_unstable();
    let host = route
        .host
        .as_ref()
        .map_or_else(String::new, |host| match host {
            HttpHostSelector::NormalizedHost { value } => format!("normalized_host:{value}"),
            HttpHostSelector::ExactAuthority { value } => format!("exact_authority:{value}"),
            HttpHostSelector::AsciiCaseInsensitiveExactAuthority { value } => {
                format!("ascii_case_insensitive_exact_authority:{value}")
            }
            HttpHostSelector::NginxLeadingWildcard { value } => {
                format!("nginx_leading_wildcard:{value}")
            }
            HttpHostSelector::NginxLeadingDot { value } => {
                format!("nginx_leading_dot:{value}")
            }
        });
    let path = match &route.path {
        HttpPathSelector::SegmentPrefix { value } => format!("segment_prefix:{value}"),
        HttpPathSelector::RawPrefix { value } => format!("raw_prefix:{value}"),
        HttpPathSelector::Exact { value } => format!("exact:{value}"),
        HttpPathSelector::AsciiCaseInsensitiveExact { value } => {
            format!("ascii_case_insensitive_exact:{value}")
        }
    };
    stable_id("http_route", &[service, &host, &path, &methods.join(",")])
}

pub(super) fn l4_service_id(name: &str) -> String {
    stable_id("l4_service", &[name])
}

pub(super) fn pool_id(name: &str) -> String {
    stable_id("upstream_pool", &[name])
}

pub(super) fn endpoint_id(pool: &str, server: &str) -> String {
    stable_id("upstream_server", &[pool, server])
}

pub(super) const fn edge_kind_name(kind: TopologyEdgeKind) -> &'static str {
    match kind {
        TopologyEdgeKind::DispatchService => "dispatch_service",
        TopologyEdgeKind::ServiceRoute => "service_route",
        TopologyEdgeKind::RoutePool => "route_pool",
        TopologyEdgeKind::ServicePool => "service_pool",
        TopologyEdgeKind::PoolEndpoint => "pool_endpoint",
        TopologyEdgeKind::ListenerTls => "listener_tls",
        TopologyEdgeKind::TlsCertificate => "tls_certificate",
    }
}

pub(super) fn route_name(route: &HttpRoute) -> String {
    let host = route.host.as_ref().map_or("*", |selector| match selector {
        HttpHostSelector::NormalizedHost { value }
        | HttpHostSelector::ExactAuthority { value }
        | HttpHostSelector::AsciiCaseInsensitiveExactAuthority { value }
        | HttpHostSelector::NginxLeadingWildcard { value }
        | HttpHostSelector::NginxLeadingDot { value } => value,
    });
    let path = match &route.path {
        HttpPathSelector::SegmentPrefix { value }
        | HttpPathSelector::RawPrefix { value }
        | HttpPathSelector::Exact { value }
        | HttpPathSelector::AsciiCaseInsensitiveExact { value } => value,
    };
    format!("{host} {path}")
}
