use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
};

use oxiroute_config::{
    AccessLogPolicy, DnsResolutionPolicy, DownstreamTimeoutPolicy, HttpAccessPolicy,
    HttpCookiePathRewrite, HttpHostSelector, HttpLiteralHeader, HttpMimeType, HttpPathSelector,
    HttpProxyPolicy, HttpRedirectLocation, HttpRequestHeaderMutation, HttpRequestHeaderValue,
    HttpResponseHeaderMutation, HttpRetryPolicy, HttpRetryTrigger, HttpRoute, HttpRouteAction,
    HttpRoutePolicy, HttpService, HttpStaticErrorResponse, HttpStaticMimePolicy,
    HttpStaticPathMapping, HttpStaticTryFile, HttpUpstreamHost, HttpVersionPolicy, Listener,
    ListenerBind, Protocol, UpstreamConnectionReuse, UpstreamEndpoint, UpstreamPool,
    UpstreamServer, UpstreamTls, canonicalize_http_path,
};

use crate::{E_INVALID_VALUE, E_SEMANTICS_NOT_REPRESENTABLE, E_UNSUPPORTED_FEATURE};

use crate::nginx::{
    DirectiveOrigin, EffectiveBind, EffectiveHttp, EffectiveLocation, EffectiveServer,
    ListenEndpoint, LocationKind, OccurrenceId, ProxyPassScheme, ServerNameKind, StaticEndpoint,
};

use super::{
    BindBlock, BindCandidate, LowerIssue, Lowerer, PoolCandidate,
    provenance::{PolicyValue, collect_result, issue, utf8},
    upstream::canonical_endpoint,
};

const NGINX_DEFAULT_BODY_BYTES: u64 = 1024 * 1024;
const NGINX_DEFAULT_PROXY_TIMEOUT_MS: u64 = 60_000;
const NGINX_DEFAULT_HIDDEN_RESPONSE_HEADERS: [&str; 8] = [
    "Date",
    "Server",
    "X-Pad",
    "X-Accel-Expires",
    "X-Accel-Redirect",
    "X-Accel-Limit-Rate",
    "X-Accel-Buffering",
    "X-Accel-Charset",
];
const NGINX_RESPONSE_CONTROL_HEADERS: [&[u8]; 5] = [
    b"X-Accel-Redirect",
    b"X-Accel-Expires",
    b"X-Accel-Limit-Rate",
    b"X-Accel-Buffering",
    b"X-Accel-Charset",
];

fn socket_addr(endpoint: &ListenEndpoint) -> Option<SocketAddr> {
    let ListenEndpoint::Socket { address, port } = endpoint else {
        return None;
    };
    let address = address
        .strip_prefix(b"[")
        .and_then(|address| address.strip_suffix(b"]"))
        .unwrap_or(address);
    let ip = utf8(address)?.parse::<IpAddr>().ok()?;
    Some(SocketAddr::new(ip, *port))
}

fn listener_bind(bind: &EffectiveBind, _servers: &[&EffectiveServer]) -> Option<ListenerBind> {
    match &bind.endpoint {
        ListenEndpoint::Unix { path } => Some(ListenerBind::Unix {
            path: path.clone(),
            mode: None,
        }),
        ListenEndpoint::Socket { address, port } => socket_addr(&bind.endpoint)
            .or_else(|| {
                (address == b"*").then(|| SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), *port))
            })
            .map(|address| ListenerBind::Socket { address }),
    }
}

pub(super) fn matching_listen<'a>(
    server: &'a EffectiveServer,
    endpoint: &ListenEndpoint,
) -> &'a crate::nginx::EffectiveListen {
    server
        .listens
        .iter()
        .find(|listen| listen.endpoint.as_ref() == Some(endpoint))
        .expect("effective bind retains each contributing listen")
}

pub(super) fn canonical_exact_host(host: &str) -> Option<String> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Some(ip.to_string());
    }
    canonical_dns_name(host).then(|| host.to_ascii_lowercase())
}

pub(super) fn canonical_wildcard_host(host: &str) -> bool {
    host.strip_prefix("*.").is_some_and(canonical_dns_name)
}

pub(super) fn canonical_dns_name(name: &str) -> bool {
    name.is_ascii()
        && !name.is_empty()
        && name.len() <= 253
        && !name.ends_with('.')
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        })
}

fn parse_size(value: &[u8]) -> Option<u64> {
    let (digits, multiplier) = match value.last().copied() {
        Some(b'k' | b'K') => (&value[..value.len() - 1], 1024_u64),
        Some(b'm' | b'M') => (&value[..value.len() - 1], 1024_u64.pow(2)),
        Some(b'g' | b'G') => (&value[..value.len() - 1], 1024_u64.pow(3)),
        Some(_) => (value, 1),
        None => return None,
    };
    let amount = utf8(digits)?.parse::<u64>().ok()?;
    let bytes = amount.checked_mul(multiplier)?;
    (bytes > 0 && bytes <= 9_007_199_254_740_991).then_some(bytes)
}

fn parse_duration_ms(value: &[u8]) -> Option<u64> {
    let (digits, multiplier) = if let Some(digits) = value.strip_suffix(b"ms") {
        (digits, 1_u64)
    } else {
        match value.last().copied() {
            Some(b's') => (&value[..value.len() - 1], 1_000),
            Some(b'm') => (&value[..value.len() - 1], 60_000),
            Some(b'h') => (&value[..value.len() - 1], 3_600_000),
            Some(b'd') => (&value[..value.len() - 1], 86_400_000),
            Some(b'w') => (&value[..value.len() - 1], 604_800_000),
            Some(_) => (value, 1),
            None => return None,
        }
    };
    let amount = utf8(digits)?.parse::<u64>().ok()?;
    let milliseconds = amount.checked_mul(multiplier)?;
    (milliseconds > 0 && milliseconds <= 9_007_199_254_740_991).then_some(milliseconds)
}

impl Lowerer {
    pub(super) fn lower_bind(
        &mut self,
        http: &EffectiveHttp,
        bind: &EffectiveBind,
        http_index: usize,
        bind_index: usize,
    ) -> BindBlock {
        let servers = bind
            .servers
            .iter()
            .filter_map(|occurrence| {
                http.servers
                    .iter()
                    .find(|server| server.origin.occurrence == *occurrence)
            })
            .collect::<Vec<_>>();
        let listener_bind = listener_bind(bind, &servers);
        let mut issues = self.semantic_bind_issues(http, &servers);
        let mut uses_default_access_log = false;
        let mut disables_access_log = false;
        for server in &servers {
            if let Some(gzip) = self.effective_policy(server.origin.occurrence, b"gzip") {
                if gzip.arguments.as_slice() != [b"off".to_vec()] {
                    issues.push(issue(
                        gzip.origins.last().unwrap_or(&server.origin),
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "enabled nginx gzip policy is broader than runtime response compression semantics",
                    ));
                }
            }
            match self.effective_policy(server.origin.occurrence, b"access_log") {
                None if self.default_access_log_path.is_some() => uses_default_access_log = true,
                None => issues.push(issue(
                    &server.origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "omitted nginx access_log enables the unrepresented default combined log",
                )),
                Some(access_log)
                    if access_log.arguments.first().is_some_and(|value| value == b"off") =>
                {
                    disables_access_log = true;
                }
                Some(access_log) => issues.push(issue(
                    access_log.origins.last().unwrap_or(&server.origin),
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "nginx formatted access_log output is not equivalent to canonical JSON access logging",
                )),
            }
        }
        if uses_default_access_log && disables_access_log {
            issues.push(issue(
                servers.first().map_or(&http.origin, |server| &server.origin),
                E_SEMANTICS_NOT_REPRESENTABLE,
                "one canonical HTTP service cannot mix migrated default and disabled nginx access logs",
            ));
        }
        if listener_bind.is_none() {
            issues.push(issue(
                servers
                    .first()
                    .map_or(&http.origin, |server| &server.origin),
                E_SEMANTICS_NOT_REPRESENTABLE,
                "nginx listener is not an explicit socket or canonical Unix address",
            ));
        }
        if !issues.is_empty() {
            return BindBlock {
                bind: listener_bind,
                issues,
                candidate: None,
            };
        }
        let listener_bind = listener_bind.expect("checked explicit bind");

        let service_name = format!("nginx-http-service-{http_index}-{bind_index}");
        let downstream_tls = servers.iter().any(|server| {
            matching_listen(server, &bind.endpoint)
                .options
                .iter()
                .any(|option| option.value == b"ssl")
        });
        let candidate = self.lower_bind_routes(
            http,
            bind,
            &servers,
            listener_bind.clone(),
            service_name,
            http_index,
            bind_index,
            downstream_tls,
            uses_default_access_log,
            &mut issues,
        );
        BindBlock {
            bind: Some(listener_bind),
            issues,
            candidate,
        }
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "one atomic bind lowering keeps partial canonical services from escaping"
    )]
    fn lower_bind_routes(
        &mut self,
        http: &EffectiveHttp,
        bind: &EffectiveBind,
        servers: &[&EffectiveServer],
        listener_bind: ListenerBind,
        service_name: String,
        http_index: usize,
        bind_index: usize,
        downstream_tls: bool,
        uses_default_access_log: bool,
        issues: &mut Vec<LowerIssue>,
    ) -> Option<BindCandidate> {
        let mut routes = Vec::new();
        let mut pools = Vec::new();
        let mut pool_names = HashSet::new();
        let mut route_origins = Vec::new();
        let mut all_origins = Vec::new();
        for server in servers {
            if server.origin.occurrence != bind.default_server
                && !bind
                    .names
                    .iter()
                    .any(|name| name.server == server.origin.occurrence)
            {
                continue;
            }
            let hosts = match Self::route_hosts(server, bind) {
                Ok(hosts) => hosts,
                Err(server_issues) => {
                    issues.extend(server_issues);
                    continue;
                }
            };
            let nginx_host_fallback = server.server_names.iter().find_map(|name| {
                (name.kind == ServerNameKind::Exact)
                    .then(|| utf8(&name.normalized))
                    .flatten()
                    .and_then(canonical_exact_host)
            });
            all_origins.push(server.origin.clone());
            let mut has_local_catch_all = false;
            for location in &server.locations {
                let top_level_catch_all = location.kind == LocationKind::Prefix
                    && location
                        .path
                        .as_ref()
                        .is_some_and(|path| path.value == b"/");
                has_local_catch_all |= top_level_catch_all;
                match self.lower_location(
                    http,
                    location,
                    &hosts,
                    &service_name,
                    http_index,
                    routes.len(),
                    downstream_tls,
                    nginx_host_fallback.as_deref(),
                ) {
                    Ok(lowered) => {
                        for mut route in lowered.routes {
                            route.origins.push(server.origin.clone());
                            route.origins.extend(
                                bind.names
                                    .iter()
                                    .filter(|name| name.server == server.origin.occurrence)
                                    .map(|name| name.name.origin.clone()),
                            );
                            route_origins.push(route.origins.clone());
                            all_origins.extend(route.origins.clone());
                            if let Some(pool) = route.pool {
                                if pool_names.insert(pool.pool.name.clone()) {
                                    pools.push(pool);
                                }
                            }
                            routes.push(route.route);
                        }
                    }
                    Err(location_issues) => issues.extend(location_issues),
                }
            }
            if !has_local_catch_all {
                let server_kind = if server.origin.occurrence == bind.default_server {
                    "default server"
                } else {
                    "non-default server"
                };
                issues.push(issue(
                    &server.origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    format!(
                        "nginx {server_kind} static or implicit fallback requires a representable location / catch-all"
                    ),
                ));
            }
        }
        let tls = match self.lower_tls(
            http,
            bind,
            servers,
            format!("nginx-tls-profile-{http_index}-{bind_index}"),
        ) {
            Ok(tls) => tls,
            Err(tls_issues) => {
                issues.extend(tls_issues);
                None
            }
        };
        if !issues.is_empty() || routes.is_empty() {
            return None;
        }
        let tls_profile = tls.as_ref().map(|tls| tls.profile.clone());
        let certificates = tls
            .as_ref()
            .map_or_else(Vec::new, |tls| tls.certificates.clone());
        if let Some(tls) = tls {
            all_origins.extend(tls.origins);
        }
        let access_log = if uses_default_access_log {
            self.used_default_access_log_overlay = true;
            Some(AccessLogPolicy::File {
                path: self
                    .default_access_log_path
                    .clone()
                    .expect("default access-log migration path"),
            })
        } else {
            Some(AccessLogPolicy::Disabled)
        };
        Some(BindCandidate {
            listener: Listener {
                name: format!("nginx-http-listener-{http_index}-{bind_index}"),
                bind: listener_bind,
                protocol: Protocol::Http,
                service: Some(service_name.clone()),
                tls_profile: tls_profile.as_ref().map(|profile| profile.name.clone()),
                max_connections: None,
                downstream_timeouts: DownstreamTimeoutPolicy::default(),
            },
            service: HttpService {
                name: service_name,
                routes,
                upstream_io_timeout_ms: NGINX_DEFAULT_PROXY_TIMEOUT_MS,
                max_request_body_bytes: Some(NGINX_DEFAULT_BODY_BYTES),
                gzip: None,
                access_log,
            },
            pools,
            certificates,
            tls_profile,
            origins: all_origins,
            route_origins,
        })
    }

    fn semantic_bind_issues(
        &self,
        http: &EffectiveHttp,
        servers: &[&EffectiveServer],
    ) -> Vec<LowerIssue> {
        let server_ids = servers
            .iter()
            .map(|server| server.origin.occurrence)
            .collect::<HashSet<_>>();
        self.blocking_decisions()
            .filter_map(|(decision, code)| {
                let affects_server = server_ids
                    .iter()
                    .any(|server| self.is_descendant(decision.occurrence, *server));
                let global = self.is_global_http_occurrence(decision.occurrence, http);
                (affects_server || global).then(|| LowerIssue {
                    origin: self.origin(decision.occurrence),
                    code,
                    message: "blocking nginx directive affects this service".into(),
                    emit: false,
                })
            })
            .collect()
    }

    fn is_global_http_occurrence(&self, occurrence: OccurrenceId, http: &EffectiveHttp) -> bool {
        if occurrence == http.origin.occurrence {
            return true;
        }
        let Some(top) = self.child_below(occurrence, http.origin.occurrence) else {
            return false;
        };
        let Some(expanded) = self.occurrence(top) else {
            return false;
        };
        !matches!(
            expanded.directive.name.value.as_slice(),
            b"server" | b"upstream"
        )
    }

    fn route_hosts(
        server: &EffectiveServer,
        bind: &EffectiveBind,
    ) -> Result<Vec<Option<HttpHostSelector>>, Vec<LowerIssue>> {
        if server.origin.occurrence == bind.default_server {
            return Ok(vec![None]);
        }
        let mut hosts = Vec::new();
        let mut issues = Vec::new();
        for name in bind
            .names
            .iter()
            .filter(|name| name.server == server.origin.occurrence)
            .map(|name| &name.name)
        {
            if name.normalized == b"_" {
                continue;
            }
            let host = match name.kind {
                ServerNameKind::Exact => utf8(&name.normalized)
                    .and_then(canonical_exact_host)
                    .map(|value| HttpHostSelector::NormalizedHost { value }),
                ServerNameKind::LeadingWildcard => utf8(&name.normalized)
                    .and_then(|name| name.strip_prefix("*."))
                    .filter(|suffix| canonical_dns_name(suffix))
                    .map(|value| HttpHostSelector::NginxLeadingWildcard {
                        value: value.into(),
                    }),
                ServerNameKind::LeadingWildcardAndExact => utf8(&name.normalized)
                    .and_then(|name| name.strip_prefix('.'))
                    .filter(|suffix| canonical_dns_name(suffix))
                    .map(|value| HttpHostSelector::NginxLeadingDot {
                        value: value.into(),
                    }),
                ServerNameKind::TrailingWildcard => {
                    issues.push(issue(
                        &name.origin,
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "nginx trailing wildcard has no canonical host selector",
                    ));
                    None
                }
                ServerNameKind::Regex | ServerNameKind::Variable | ServerNameKind::Invalid => None,
            };
            if let Some(host) = host {
                if !hosts.contains(&host) {
                    hosts.push(host);
                }
            } else if issues.is_empty() {
                issues.push(issue(
                    &name.origin,
                    E_INVALID_VALUE,
                    "server_name is not a canonical exact or one-label wildcard host",
                ));
            }
        }
        if hosts.is_empty() && issues.is_empty() {
            issues.push(issue(
                &server.origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "non-default nginx server has no canonical host identity",
            ));
        }
        if issues.is_empty() {
            Ok(hosts.into_iter().map(Some).collect())
        } else {
            Err(issues)
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "matcher, action, and provenance must be accepted or rejected as one route"
    )]
    #[allow(clippy::too_many_arguments)]
    fn lower_location(
        &self,
        http: &EffectiveHttp,
        location: &EffectiveLocation,
        hosts: &[Option<HttpHostSelector>],
        service_name: &str,
        http_index: usize,
        route_ordinal: usize,
        downstream_tls: bool,
        nginx_host_fallback: Option<&str>,
    ) -> Result<LoweredLocation, Vec<LowerIssue>> {
        let mut issues = Vec::new();
        if !matches!(location.kind, LocationKind::Exact | LocationKind::Prefix) {
            issues.push(issue(
                &location.origin,
                E_UNSUPPORTED_FEATURE,
                "nginx location kind is unsupported by canonical routing",
            ));
        }
        let path_prefix = location.path.as_ref().and_then(|path| utf8(&path.value));
        let canonical_path = path_prefix.and_then(canonicalize_http_path);
        if path_prefix.is_none()
            || canonical_path
                .as_ref()
                .is_none_or(|canonical| canonical.as_ref() != path_prefix.expect("checked path"))
        {
            issues.push(issue(
                &location.origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "nginx location path has ambiguous canonical normalization",
            ));
        }
        if !location.children.is_empty() {
            issues.push(issue(
                &location.origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "nested nginx location selection is not one canonical route precedence domain",
            ));
        }
        let body_bytes =
            self.body_policy(location.origin.occurrence, &location.origin, &mut issues);
        let access_policy = self.lower_access_policy(location, &mut issues);
        let selector = match location.kind {
            LocationKind::Exact => HttpPathSelector::Exact {
                value: path_prefix.unwrap_or("/").into(),
            },
            LocationKind::Prefix => HttpPathSelector::RawPrefix {
                value: path_prefix.unwrap_or("/").into(),
            },
            _ => HttpPathSelector::RawPrefix { value: "/".into() },
        };
        let return_policy = self.effective_policy(location.origin.occurrence, b"return");
        let (action, pool, timeouts, mut origins) = if let Some(value) = return_policy {
            let return_status = match value.arguments.as_slice() {
                [payload]
                    if payload.starts_with(b"http://") || payload.starts_with(b"https://") =>
                {
                    Some(302)
                }
                [status] | [status, _] => utf8(status).and_then(|status| status.parse().ok()),
                _ => None,
            };
            if return_status.is_some_and(|status| {
                self.error_page_matches_status(location.origin.occurrence, status)
            }) {
                issues.push(issue(
                    value.origins.last().unwrap_or(&location.origin),
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "nginx return status triggers an error_page redirect outside local action semantics",
                ));
            }
            let mut header_origins = Vec::new();
            let headers = self.lower_literal_headers(location, &mut header_origins, &mut issues);
            let action = Self::lower_return(
                &value,
                &location.origin,
                nginx_host_fallback,
                headers,
                &mut issues,
            );
            let mut origins = value.origins;
            origins.extend(header_origins);
            (
                action.unwrap_or(HttpRouteAction::FixedResponse {
                    status: 500,
                    body: String::new(),
                    headers: Vec::new(),
                }),
                None,
                None,
                origins,
            )
        } else if let Some(proxy) = &location.proxy_pass {
            match self.lower_proxy(
                http,
                location,
                proxy,
                service_name,
                http_index,
                route_ordinal,
                downstream_tls,
                nginx_host_fallback,
                &mut issues,
            ) {
                Some(proxy) => (
                    HttpRouteAction::Proxy {
                        upstream_pool: proxy.pool.pool.name.clone(),
                        policy: proxy.policy,
                    },
                    Some(proxy.pool),
                    Some(proxy.timeouts),
                    proxy.origins,
                ),
                None => (
                    HttpRouteAction::FixedResponse {
                        status: 500,
                        body: String::new(),
                        headers: Vec::new(),
                    },
                    None,
                    None,
                    Vec::new(),
                ),
            }
        } else if let Some((action, origins)) = self.lower_static(location, &mut issues) {
            (action, None, None, origins)
        } else {
            (
                HttpRouteAction::FixedResponse {
                    status: 500,
                    body: String::new(),
                    headers: Vec::new(),
                },
                None,
                None,
                Vec::new(),
            )
        };
        origins.push(location.origin.clone());
        if !issues.is_empty() {
            return Err(issues);
        }
        Ok(LoweredLocation {
            routes: hosts
                .iter()
                .cloned()
                .enumerate()
                .map(|(index, host)| LoweredRoute {
                    route: HttpRoute {
                        host,
                        path: selector.clone(),
                        methods: Vec::new(),
                        access_policy: access_policy.clone(),
                        policy: HttpRoutePolicy {
                            max_request_body_bytes: body_bytes,
                            connect_timeout_ms: timeouts
                                .map_or(NGINX_DEFAULT_PROXY_TIMEOUT_MS, |value| value.connect),
                            read_timeout_ms: timeouts
                                .map_or(NGINX_DEFAULT_PROXY_TIMEOUT_MS, |value| value.read),
                            write_timeout_ms: timeouts
                                .map_or(NGINX_DEFAULT_PROXY_TIMEOUT_MS, |value| value.write),
                            request_buffering: false,
                            response_buffering: false,
                        },
                        action: action.clone(),
                    },
                    pool: (index == 0).then(|| pool.clone()).flatten(),
                    origins: origins.clone(),
                })
                .collect(),
        })
    }

    fn validate_route_policy(
        &self,
        scope: OccurrenceId,
        fallback_origin: &DirectiveOrigin,
    ) -> Result<(), Vec<LowerIssue>> {
        let mut issues = Vec::new();
        let version = self.effective_policy(scope, b"proxy_http_version");
        if version
            .as_ref()
            .is_none_or(|value| value.arguments.as_slice() != [b"1.1".to_vec()])
        {
            let origin = version
                .as_ref()
                .and_then(|value| value.origins.last())
                .unwrap_or(fallback_origin);
            issues.push(issue(
                origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "proxy_http_version must explicitly be 1.1 because nginx defaults to 1.0",
            ));
        }
        for name in [b"proxy_buffering".as_slice(), b"proxy_request_buffering"] {
            let policy = self.effective_policy(scope, name);
            if policy
                .as_ref()
                .is_none_or(|value| value.arguments.as_slice() != [b"off".to_vec()])
            {
                issues.push(issue(
                    policy
                        .as_ref()
                        .and_then(|value| value.origins.last())
                        .unwrap_or(fallback_origin),
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    format!(
                        "{} must explicitly be off to match unbuffered runtime semantics",
                        String::from_utf8_lossy(name)
                    ),
                ));
            }
        }
        if issues.is_empty() {
            Ok(())
        } else {
            Err(issues)
        }
    }

    fn body_policy(
        &self,
        scope: OccurrenceId,
        fallback_origin: &DirectiveOrigin,
        issues: &mut Vec<LowerIssue>,
    ) -> Option<u64> {
        let Some(value) = self.effective_policy(scope, b"client_max_body_size") else {
            return Some(NGINX_DEFAULT_BODY_BYTES);
        };
        if value
            .arguments
            .first()
            .is_some_and(|argument| argument == b"0")
        {
            return None;
        }
        if let Some(bytes) = value
            .arguments
            .first()
            .and_then(|argument| parse_size(argument))
        {
            Some(bytes)
        } else {
            issues.push(issue(
                value.origins.last().unwrap_or(fallback_origin),
                E_INVALID_VALUE,
                "client_max_body_size is not a canonical byte limit",
            ));
            Some(NGINX_DEFAULT_BODY_BYTES)
        }
    }

    fn lower_access_policy(
        &self,
        location: &EffectiveLocation,
        issues: &mut Vec<LowerIssue>,
    ) -> Option<oxiroute_config::HttpAccessPolicy> {
        if let Some(conditional) = self.graph.expanded_occurrences.iter().find(|occurrence| {
            occurrence.parent == Some(location.origin.occurrence)
                && occurrence.directive.name.value == b"if"
                && occurrence.directive.arguments.iter().any(|argument| {
                    argument
                        .value
                        .windows(b"<redacted>".len())
                        .any(|window| window == b"<redacted>")
                })
        }) {
            let server = self
                .resolution
                .http_blocks
                .iter()
                .flat_map(|http| &http.servers)
                .find(|server| {
                    self.is_descendant(location.origin.occurrence, server.origin.occurrence)
                });
            let overlay = server.and_then(|server| {
                server.server_names.iter().find_map(|name| {
                    self.bearer_token_overlays
                        .get(&name.normalized)
                        .cloned()
                        .map(|path| (name.normalized.clone(), path))
                })
            });
            let Some((overlay_name, token_file_path)) = overlay else {
                issues.push(issue(
                    &self.origin(conditional.id),
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "redacted nginx authorization rule requires an explicit bearer token file overlay",
                ));
                return None;
            };
            if !token_file_path.is_absolute() {
                issues.push(issue(
                    &self.origin(conditional.id),
                    E_INVALID_VALUE,
                    "bearer token overlay path must be absolute",
                ));
                return None;
            }
            self.used_bearer_token_overlays
                .borrow_mut()
                .insert(overlay_name);
            return Some(HttpAccessPolicy::BearerTokenFile {
                token_file_path,
                header_name: "authorization".into(),
                realm: None,
            });
        }
        let basic = self.effective_policy(location.origin.occurrence, b"auth_basic")?;
        if basic.arguments.as_slice() == [b"off".to_vec()] {
            return None;
        }
        let Some(file) = self.effective_policy(location.origin.occurrence, b"auth_basic_user_file")
        else {
            issues.push(issue(
                basic.origins.last().unwrap_or(&location.origin),
                E_INVALID_VALUE,
                "nginx Basic authentication requires an htpasswd file",
            ));
            return None;
        };
        let Some(realm) = basic.arguments.first().and_then(|value| utf8(value)) else {
            issues.push(issue(
                basic.origins.last().unwrap_or(&location.origin),
                E_INVALID_VALUE,
                "nginx Basic authentication realm is not UTF-8",
            ));
            return None;
        };
        let Some(path) = file
            .arguments
            .first()
            .and_then(|value| canonical_file_path(value))
        else {
            issues.push(issue(
                file.origins.last().unwrap_or(&location.origin),
                E_INVALID_VALUE,
                "nginx htpasswd path is not a canonical absolute file",
            ));
            return None;
        };
        self.used_htpasswd_overlays
            .borrow_mut()
            .extend(file.origins.iter().map(|origin| origin.occurrence));
        Some(HttpAccessPolicy::BasicHtpasswdFile {
            htpasswd_file_path: path,
            realm: realm.into(),
        })
    }

    fn lower_return(
        value: &PolicyValue,
        fallback: &DirectiveOrigin,
        nginx_host_fallback: Option<&str>,
        headers: Vec<HttpLiteralHeader>,
        issues: &mut Vec<LowerIssue>,
    ) -> Option<HttpRouteAction> {
        let origin = value.origins.last().unwrap_or(fallback);
        let (status, payload) = match value.arguments.as_slice() {
            [payload] if payload.starts_with(b"http://") || payload.starts_with(b"https://") => {
                (302, payload.as_slice())
            }
            [status] => (utf8(status)?.parse::<u16>().ok()?, b"".as_slice()),
            [status, payload] => (utf8(status)?.parse::<u16>().ok()?, payload.as_slice()),
            _ => return None,
        };
        let Some(payload) = utf8(payload) else {
            issues.push(issue(
                origin,
                E_INVALID_VALUE,
                "nginx return payload is not UTF-8",
            ));
            return None;
        };
        if matches!(status, 301 | 302 | 307 | 308) {
            if payload.is_empty() {
                issues.push(issue(
                    origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "nginx redirect return requires an explicit location",
                ));
                return None;
            }
            let location = if payload.contains('$') {
                HttpRedirectLocation::RequestTemplate {
                    value: payload.into(),
                    nginx_host_fallback: nginx_host_fallback.map(str::to_owned),
                }
            } else {
                HttpRedirectLocation::Literal {
                    value: payload.into(),
                }
            };
            return Some(HttpRouteAction::Redirect {
                status,
                location,
                headers,
            });
        }
        Some(HttpRouteAction::FixedResponse {
            status,
            body: payload.into(),
            headers,
        })
    }

    fn lower_static(
        &self,
        location: &EffectiveLocation,
        issues: &mut Vec<LowerIssue>,
    ) -> Option<(HttpRouteAction, Vec<DirectiveOrigin>)> {
        let root = self.effective_policy(location.origin.occurrence, b"root");
        let alias = self.effective_policy(location.origin.occurrence, b"alias");
        let (directory, path_mapping, mut origins) = match (root, alias) {
            (Some(root), None) => (root, HttpStaticPathMapping::Root, Vec::new()),
            (None, Some(alias)) => (alias, HttpStaticPathMapping::Alias, Vec::new()),
            (Some(root), Some(alias)) => {
                issues.push(issue(
                    alias.origins.last().unwrap_or(&location.origin),
                    E_INVALID_VALUE,
                    "nginx static location cannot combine root and alias",
                ));
                (root, HttpStaticPathMapping::Root, Vec::new())
            }
            (None, None) => {
                issues.push(issue(
                    &location.origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "nginx location has no proxy, return, or static root action",
                ));
                return None;
            }
        };
        origins.extend(directory.origins.clone());
        let Some(root_directory) = directory
            .arguments
            .first()
            .and_then(|value| canonical_directory(value))
        else {
            issues.push(issue(
                directory.origins.last().unwrap_or(&location.origin),
                E_INVALID_VALUE,
                "nginx static root is not a canonical absolute directory",
            ));
            return None;
        };

        let mut index_files = Vec::new();
        for policy in self.effective_list_policy_chain(location.origin.occurrence, b"index") {
            origins.extend(policy.origins.clone());
            for value in policy.arguments {
                if let Some(value) = utf8(&value) {
                    index_files.push(value.into());
                }
            }
        }
        if index_files.is_empty() {
            index_files.push("index.html".into());
        }

        let try_files = self.lower_try_files(location, &mut origins, issues);
        let mime = self.lower_static_mime(location, &mut origins, issues);
        let headers = self.lower_literal_headers(location, &mut origins, issues);
        let mut error_responses = self.lower_error_responses(location, &mut origins, issues);
        if let Some(server) = self.default_error_server.clone() {
            if !self.error_page_matches_status(location.origin.occurrence, 404) {
                error_responses.push(nginx_default_404(&server));
                self.used_default_error_overlay.set(true);
            }
        }
        let autoindex = self.policy_enabled(location.origin.occurrence, b"autoindex", false);
        let autoindex_exact_size =
            self.policy_enabled(location.origin.occurrence, b"autoindex_exact_size", true);
        let autoindex_local_time =
            self.policy_enabled(location.origin.occurrence, b"autoindex_localtime", false);

        Some((
            HttpRouteAction::StaticFiles {
                root_directory,
                path_mapping,
                index_files,
                internal_index_redirects: true,
                directory_redirects: true,
                spa_fallback: None,
                try_files,
                autoindex,
                autoindex_exact_size,
                autoindex_local_time,
                mime,
                headers,
                error_responses,
            },
            origins,
        ))
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "proxy admission, policy, pool, and provenance form one atomic route action"
    )]
    fn lower_proxy(
        &self,
        http: &EffectiveHttp,
        location: &EffectiveLocation,
        proxy: &crate::nginx::EffectiveProxyPass,
        service_name: &str,
        http_index: usize,
        route_ordinal: usize,
        downstream_tls: bool,
        nginx_host_fallback: Option<&str>,
        issues: &mut Vec<LowerIssue>,
    ) -> Option<LoweredProxy> {
        if let Some(error_page) = self
            .effective_list_policy_chain(location.origin.occurrence, b"error_page")
            .last()
        {
            issues.push(issue(
                error_page.origins.last().unwrap_or(&location.origin),
                E_SEMANTICS_NOT_REPRESENTABLE,
                "nginx proxy error_page handling is not represented for generated proxy errors",
            ));
        }
        if proxy.replacement_uri.is_some() {
            issues.push(issue(
                &proxy.origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "proxy_pass URI replacement is not represented by canonical routes",
            ));
        }
        let upstream_tls = match self.validate_proxy_origin(http, proxy, downstream_tls) {
            Ok(tls) => tls,
            Err(origin_issues) => {
                issues.extend(origin_issues);
                None
            }
        };
        if let Err(policy_issues) =
            self.validate_route_policy(location.origin.occurrence, &location.origin)
        {
            issues.extend(policy_issues);
        }
        if let Some(policy) = self.effective_policy(location.origin.occurrence, b"proxy_buffering")
        {
            if policy.arguments.as_slice() != [b"off".to_vec()] {
                issues.push(issue(
                    policy.origins.last().unwrap_or(&proxy.origin),
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "proxy_buffering must be disabled to match streaming canonical proxy semantics",
                ));
            }
        }
        let pool = Self::proxy_pool(
            http,
            proxy,
            service_name,
            http_index,
            route_ordinal,
            upstream_tls,
            issues,
        )?;
        let (upstream_host, request_headers) =
            self.lower_proxy_headers(location, proxy, nginx_host_fallback, issues);
        self.validate_response_controls(location, proxy, issues);
        let mut response_header_origins = Vec::new();
        let response_headers =
            self.lower_response_headers(location, &mut response_header_origins, issues);
        let response_cookie_path_rewrites = self.lower_cookie_rewrites(location, issues);
        let retry = self.lower_proxy_retry(location, pool.pool.servers.len(), issues);
        let timeouts = self.proxy_timeouts(location.origin.occurrence, &location.origin, issues);
        if !issues.is_empty() {
            return None;
        }
        let mut origins = vec![proxy.origin.clone(), location.origin.clone()];
        origins.extend(response_header_origins);
        for name in [
            b"client_max_body_size".as_slice(),
            b"proxy_connect_timeout".as_slice(),
            b"proxy_read_timeout".as_slice(),
            b"proxy_send_timeout".as_slice(),
            b"proxy_http_version".as_slice(),
            b"proxy_buffering".as_slice(),
            b"proxy_request_buffering".as_slice(),
            b"proxy_set_header".as_slice(),
            b"proxy_hide_header".as_slice(),
            b"proxy_pass_header".as_slice(),
            b"proxy_ignore_headers".as_slice(),
            b"proxy_cookie_path".as_slice(),
            b"proxy_next_upstream".as_slice(),
            b"proxy_next_upstream_tries".as_slice(),
        ] {
            for value in self.effective_list_policy_chain(location.origin.occurrence, name) {
                origins.extend(value.origins);
            }
        }
        match proxy.upstream {
            crate::nginx::UpstreamReference::Resolved(occurrence) => {
                if let Some(upstream) = http
                    .upstreams
                    .iter()
                    .find(|upstream| upstream.origin.occurrence == occurrence)
                {
                    origins.push(upstream.origin.clone());
                    origins.extend(upstream.servers.iter().map(|server| server.origin.clone()));
                }
            }
            crate::nginx::UpstreamReference::Direct
            | crate::nginx::UpstreamReference::Unresolved
            | crate::nginx::UpstreamReference::Variable => {}
        }
        Some(LoweredProxy {
            pool,
            policy: HttpProxyPolicy {
                upstream_host,
                request_headers,
                response_headers,
                response_cookie_path_rewrites,
                response_cookie_attributes: Vec::new(),
                retry,
                cache: None,
            },
            timeouts,
            origins,
        })
    }

    fn proxy_pool(
        http: &EffectiveHttp,
        proxy: &crate::nginx::EffectiveProxyPass,
        service_name: &str,
        http_index: usize,
        route_ordinal: usize,
        upstream_tls: Option<UpstreamTls>,
        issues: &mut Vec<LowerIssue>,
    ) -> Option<PoolCandidate> {
        let (endpoints, origin, endpoint_origins) = match proxy.upstream {
            crate::nginx::UpstreamReference::Direct => (
                proxy
                    .direct_endpoint
                    .as_ref()
                    .map(|endpoint| {
                        canonical_proxy_endpoint(endpoint, proxy, upstream_tls.is_some())
                    })
                    .into_iter()
                    .collect(),
                proxy.origin.clone(),
                vec![proxy.origin.clone()],
            ),
            crate::nginx::UpstreamReference::Resolved(occurrence) => {
                let upstream = http
                    .upstreams
                    .iter()
                    .find(|upstream| upstream.origin.occurrence == occurrence)
                    .expect("resolved upstream occurrence is retained");
                let (endpoints, endpoint_origins) = upstream
                    .servers
                    .iter()
                    .filter_map(|server| {
                        server
                            .endpoint
                            .as_ref()
                            .map(|endpoint| (canonical_endpoint(endpoint), server.origin.clone()))
                    })
                    .unzip();
                (endpoints, upstream.origin.clone(), endpoint_origins)
            }
            crate::nginx::UpstreamReference::Unresolved
            | crate::nginx::UpstreamReference::Variable => {
                (Vec::new(), proxy.origin.clone(), Vec::new())
            }
        };
        if endpoints.is_empty() {
            issues.push(issue(
                &proxy.origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "nginx proxy origin has no canonical endpoint set",
            ));
            return None;
        }
        let name = match proxy.upstream {
            crate::nginx::UpstreamReference::Resolved(occurrence) => {
                let upstream_index = http
                    .upstreams
                    .iter()
                    .position(|upstream| upstream.origin.occurrence == occurrence)
                    .expect("resolved upstream occurrence is retained");
                format!("nginx-http-upstream-{http_index}-{upstream_index}")
            }
            crate::nginx::UpstreamReference::Direct
            | crate::nginx::UpstreamReference::Unresolved
            | crate::nginx::UpstreamReference::Variable => {
                format!("{service_name}-pool-{route_ordinal}")
            }
        };
        let servers = endpoints
            .into_iter()
            .enumerate()
            .map(|(index, endpoint)| {
                let dns_resolution = if matches!(endpoint, UpstreamEndpoint::Dns { .. }) {
                    DnsResolutionPolicy::Startup
                } else {
                    DnsResolutionPolicy::OnConnect
                };
                UpstreamServer {
                    name: format!("endpoint-{}", index + 1),
                    endpoint,
                    max_connections: None,
                    dns_resolution,
                }
            })
            .collect();
        Some(PoolCandidate {
            pool: UpstreamPool {
                name,
                servers,
                endpoints: Vec::new(),
                algorithm: oxiroute_config::UpstreamAlgorithm::RoundRobin,
                health_check: None,
                tls: upstream_tls,
                http_versions: HttpVersionPolicy::default(),
                queue_timeout_ms: None,
                connect_timeout_ms: None,
                server_timeout_ms: None,
                connection_reuse: UpstreamConnectionReuse::Safe,
            },
            origin,
            endpoint_origins,
        })
    }

    fn lower_proxy_headers(
        &self,
        location: &EffectiveLocation,
        proxy: &crate::nginx::EffectiveProxyPass,
        nginx_host_fallback: Option<&str>,
        issues: &mut Vec<LowerIssue>,
    ) -> (HttpUpstreamHost, Vec<HttpRequestHeaderMutation>) {
        let policies =
            self.effective_list_policy_chain(location.origin.occurrence, b"proxy_set_header");
        let mut host = None;
        let mut headers = Vec::new();
        for policy in policies {
            let [name, value] = policy.arguments.as_slice() else {
                continue;
            };
            let origin = policy.origins.last().unwrap_or(&location.origin);
            let Some(name) = utf8(name) else {
                issues.push(issue(
                    origin,
                    E_INVALID_VALUE,
                    "proxy_set_header name is not UTF-8",
                ));
                continue;
            };
            if name.eq_ignore_ascii_case("host") {
                if host.is_some() {
                    issues.push(issue(
                        origin,
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "proxy_set_header defines Host more than once",
                    ));
                    continue;
                }
                host = Self::proxy_host_policy(value, proxy, nginx_host_fallback);
                continue;
            }
            let mutation = if value.is_empty() {
                Some(HttpRequestHeaderMutation::Remove { name: name.into() })
            } else {
                Self::request_header_value(value, proxy, nginx_host_fallback).map(|value| {
                    HttpRequestHeaderMutation::Set {
                        name: name.into(),
                        value,
                    }
                })
            };
            if let Some(mutation) = mutation {
                headers.push(mutation);
            } else {
                issues.push(issue(
                    origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "proxy_set_header value uses an unsupported nginx variable or template",
                ));
            }
        }
        (host.unwrap_or(HttpUpstreamHost::PreserveIncoming), headers)
    }

    fn proxy_host_policy(
        value: &[u8],
        proxy: &crate::nginx::EffectiveProxyPass,
        nginx_host_fallback: Option<&str>,
    ) -> Option<HttpUpstreamHost> {
        match value {
            b"$http_host" => Some(HttpUpstreamHost::PreserveIncoming),
            b"$host" => nginx_host_fallback.map(|fallback| HttpUpstreamHost::NginxHost {
                fallback: fallback.to_owned(),
            }),
            b"$proxy_host" => utf8(&proxy.authority).map(|value| HttpUpstreamHost::Literal {
                value: value.into(),
            }),
            value if !value.contains(&b'$') => utf8(value).map(|value| HttpUpstreamHost::Literal {
                value: value.into(),
            }),
            _ => None,
        }
    }

    fn request_header_value(
        value: &[u8],
        proxy: &crate::nginx::EffectiveProxyPass,
        nginx_host_fallback: Option<&str>,
    ) -> Option<HttpRequestHeaderValue> {
        match value {
            b"$http_host" => Some(HttpRequestHeaderValue::IncomingAuthority),
            b"$host" => nginx_host_fallback.map(|fallback| HttpRequestHeaderValue::NginxHost {
                fallback: fallback.to_owned(),
            }),
            b"$remote_addr" => Some(HttpRequestHeaderValue::ClientIp),
            b"$proxy_add_x_forwarded_for" => Some(HttpRequestHeaderValue::AppendedXForwardedFor {
                max_bytes: 8_192,
                except_source_cidrs: Vec::new(),
            }),
            b"$scheme" => Some(HttpRequestHeaderValue::DownstreamScheme),
            b"$http_upgrade" => Some(HttpRequestHeaderValue::IncomingHeader {
                name: "upgrade".into(),
                max_bytes: 8_192,
            }),
            b"$proxy_host" => utf8(&proxy.authority).map(|value| HttpRequestHeaderValue::Literal {
                value: value.into(),
            }),
            value if !value.contains(&b'$') => {
                utf8(value).map(|value| HttpRequestHeaderValue::Literal {
                    value: value.into(),
                })
            }
            _ => None,
        }
    }

    fn lower_response_headers(
        &self,
        location: &EffectiveLocation,
        origins: &mut Vec<DirectiveOrigin>,
        issues: &mut Vec<LowerIssue>,
    ) -> Vec<HttpResponseHeaderMutation> {
        let mut hidden = NGINX_DEFAULT_HIDDEN_RESPONSE_HEADERS
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        for policy in
            self.effective_list_policy_chain(location.origin.occurrence, b"proxy_hide_header")
        {
            let Some(name) = policy.arguments.first().and_then(|value| utf8(value)) else {
                issues.push(issue(
                    policy.origins.last().unwrap_or(&location.origin),
                    E_INVALID_VALUE,
                    "proxy_hide_header name is not UTF-8",
                ));
                continue;
            };
            if !hidden
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(name))
            {
                hidden.push(name.to_owned());
            }
        }
        for policy in
            self.effective_list_policy_chain(location.origin.occurrence, b"proxy_pass_header")
        {
            let Some(name) = policy.arguments.first().and_then(|value| utf8(value)) else {
                issues.push(issue(
                    policy.origins.last().unwrap_or(&location.origin),
                    E_INVALID_VALUE,
                    "proxy_pass_header name is not UTF-8",
                ));
                continue;
            };
            if name.eq_ignore_ascii_case("date") {
                issues.push(issue(
                    policy.origins.last().unwrap_or(&location.origin),
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "proxy_pass_header Date cannot be represented because Pingora replaces every downstream Date header",
                ));
                continue;
            }
            hidden.retain(|candidate| !candidate.eq_ignore_ascii_case(name));
        }
        let mut mutations = hidden
            .into_iter()
            .map(|name| HttpResponseHeaderMutation::Remove { name })
            .collect::<Vec<_>>();
        for header in self.lower_literal_headers(location, origins, issues) {
            mutations.push(HttpResponseHeaderMutation::Add {
                name: header.name,
                value: header.value,
                always: header.always,
            });
        }
        mutations
    }

    fn validate_response_controls(
        &self,
        location: &EffectiveLocation,
        proxy: &crate::nginx::EffectiveProxyPass,
        issues: &mut Vec<LowerIssue>,
    ) {
        let policies =
            self.effective_list_policy_chain(location.origin.occurrence, b"proxy_ignore_headers");
        if policies.is_empty() {
            issues.push(issue(
                &proxy.origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "proxy_ignore_headers must explicitly disable every nginx X-Accel response control",
            ));
            return;
        }
        let ignored = policies
            .iter()
            .flat_map(|policy| &policy.arguments)
            .collect::<Vec<_>>();
        if !NGINX_RESPONSE_CONTROL_HEADERS.iter().all(|required| {
            ignored
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(required))
        }) {
            issues.push(issue(
                policies
                    .last()
                    .and_then(|policy| policy.origins.last())
                    .unwrap_or(&proxy.origin),
                E_SEMANTICS_NOT_REPRESENTABLE,
                "explicit proxy_ignore_headers omits an nginx X-Accel response control",
            ));
        }
    }

    fn lower_cookie_rewrites(
        &self,
        location: &EffectiveLocation,
        issues: &mut Vec<LowerIssue>,
    ) -> Vec<HttpCookiePathRewrite> {
        self.effective_list_policy_chain(location.origin.occurrence, b"proxy_cookie_path")
            .into_iter()
            .filter_map(|policy| {
                let [from, to] = policy.arguments.as_slice() else { return None };
                let literal = !from.contains(&b'$') && !to.contains(&b'$');
                let pair = utf8(from).zip(utf8(to));
                if !literal || pair.is_none() {
                    issues.push(issue(policy.origins.last().unwrap_or(&location.origin), E_SEMANTICS_NOT_REPRESENTABLE, "proxy_cookie_path regex, variables, and non-UTF-8 values are not canonical rewrites"));
                    return None;
                }
                let (from, to) = pair.expect("checked cookie paths");
                let to = to.split(';').next().unwrap_or(to).trim_end();
                Some(HttpCookiePathRewrite { from: from.into(), to: to.into() })
            })
            .collect()
    }

    fn lower_proxy_retry(
        &self,
        location: &EffectiveLocation,
        endpoint_count: usize,
        issues: &mut Vec<LowerIssue>,
    ) -> HttpRetryPolicy {
        let Some(policy) =
            self.effective_policy(location.origin.occurrence, b"proxy_next_upstream")
        else {
            if endpoint_count > 1 {
                issues.push(issue(
                    &location.origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "nginx defaults proxy_next_upstream to error timeout, whose request/write/read failure breadth is wider than canonical connect retries",
                ));
            }
            return HttpRetryPolicy {
                max_retries: 0,
                ..HttpRetryPolicy::default()
            };
        };
        if policy.arguments.as_slice() == [b"off".to_vec()] {
            return HttpRetryPolicy {
                max_retries: 0,
                ..HttpRetryPolicy::default()
            };
        }
        let mut triggers = Vec::new();
        for trigger in &policy.arguments {
            match trigger.as_slice() {
                b"error" | b"timeout" => issues.push(issue(
                    policy.origins.last().unwrap_or(&location.origin),
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "nginx proxy_next_upstream error/timeout includes post-connect request, response-header, and I/O failures beyond canonical connect retry triggers",
                )),
                _ => issues.push(issue(
                    policy.origins.last().unwrap_or(&location.origin),
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "proxy_next_upstream trigger is broader than the canonical safe subset",
                )),
            }
        }
        triggers.sort_unstable_by_key(|trigger| match trigger {
            HttpRetryTrigger::ConnectFailure => 0,
            HttpRetryTrigger::ConnectTimeout => 1,
            HttpRetryTrigger::RefusedStream => 2,
        });
        triggers.dedup();
        let configured_tries =
            self.effective_policy(location.origin.occurrence, b"proxy_next_upstream_tries");
        let tries = configured_tries
            .as_ref()
            .and_then(|value| value.arguments.first())
            .and_then(|value| utf8(value))
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(endpoint_count);
        if tries == 0 || tries > 3 {
            issues.push(issue(
                configured_tries
                    .as_ref()
                    .and_then(|value| value.origins.last())
                    .unwrap_or(&location.origin),
                E_SEMANTICS_NOT_REPRESENTABLE,
                "proxy_next_upstream attempts exceed the canonical two-retry bound",
            ));
        }
        HttpRetryPolicy {
            max_retries: u8::try_from(tries.saturating_sub(1).min(2))
                .expect("retry count is bounded to two"),
            triggers,
            ..HttpRetryPolicy::default()
        }
    }

    fn proxy_timeouts(
        &self,
        scope: OccurrenceId,
        origin: &DirectiveOrigin,
        issues: &mut Vec<LowerIssue>,
    ) -> ProxyTimeouts {
        let connect = collect_result(
            self.proxy_timeout(scope, b"proxy_connect_timeout", origin),
            issues,
        )
        .unwrap_or(NGINX_DEFAULT_PROXY_TIMEOUT_MS);
        let read = collect_result(
            self.proxy_timeout(scope, b"proxy_read_timeout", origin),
            issues,
        )
        .unwrap_or(NGINX_DEFAULT_PROXY_TIMEOUT_MS);
        let send = collect_result(
            self.proxy_timeout(scope, b"proxy_send_timeout", origin),
            issues,
        )
        .unwrap_or(NGINX_DEFAULT_PROXY_TIMEOUT_MS);
        ProxyTimeouts {
            connect,
            read,
            write: send,
        }
    }

    fn proxy_timeout(
        &self,
        scope: OccurrenceId,
        name: &[u8],
        fallback_origin: &DirectiveOrigin,
    ) -> Result<u64, LowerIssue> {
        self.effective_policy(scope, name)
            .map_or(Ok(NGINX_DEFAULT_PROXY_TIMEOUT_MS), |value| {
                parse_duration_ms(&value.arguments[0]).ok_or_else(|| {
                    issue(
                        value.origins.last().unwrap_or(fallback_origin),
                        E_INVALID_VALUE,
                        "nginx timeout is not a finite canonical millisecond value",
                    )
                })
            })
    }

    fn lower_try_files(
        &self,
        location: &EffectiveLocation,
        origins: &mut Vec<DirectiveOrigin>,
        issues: &mut Vec<LowerIssue>,
    ) -> Vec<HttpStaticTryFile> {
        let mut lowered = Vec::new();
        for policy in self.effective_list_policy_chain(location.origin.occurrence, b"try_files") {
            origins.extend(policy.origins.clone());
            let last = policy.arguments.len().saturating_sub(1);
            for (index, value) in policy.arguments.into_iter().enumerate() {
                if index == last && !value.starts_with(b"=") {
                    issues.push(issue(
                        policy.origins.last().unwrap_or(&location.origin),
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "nginx try_files URI fallback requires authenticated internal rerouting",
                    ));
                    continue;
                }
                let item = match value.as_slice() {
                    b"$uri" => Some(HttpStaticTryFile::RequestPath),
                    b"$uri/" => Some(HttpStaticTryFile::RequestPathDirectory),
                    value if value.starts_with(b"=") => utf8(&value[1..])
                        .and_then(|status| status.parse::<u16>().ok())
                        .map(|status| HttpStaticTryFile::Status { status }),
                    value if !value.contains(&b'$') => {
                        utf8(value).map(|path| HttpStaticTryFile::Relative {
                            path: PathBuf::from(path.trim_start_matches('/')),
                        })
                    }
                    _ => None,
                };
                if let Some(item) = item {
                    lowered.push(item);
                } else {
                    issues.push(issue(
                        policy.origins.last().unwrap_or(&location.origin),
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "nginx try_files entry is outside the canonical static lookup subset",
                    ));
                }
            }
        }
        lowered
    }

    fn lower_static_mime(
        &self,
        location: &EffectiveLocation,
        origins: &mut Vec<DirectiveOrigin>,
        issues: &mut Vec<LowerIssue>,
    ) -> HttpStaticMimePolicy {
        let default_type = self
            .effective_policy(location.origin.occurrence, b"default_type")
            .and_then(|policy| {
                origins.extend(policy.origins.clone());
                policy
                    .arguments
                    .first()
                    .and_then(|value| utf8(value))
                    .map(str::to_owned)
            })
            .or_else(|| Some("text/plain".to_owned()));
        let mut types = Vec::<HttpMimeType>::new();
        let Some(types_occurrence) = self.effective_types_occurrence(location.origin.occurrence)
        else {
            return HttpStaticMimePolicy {
                default_type,
                types,
            };
        };
        origins.push(self.origin(types_occurrence));
        for mapping in self
            .graph
            .expanded_occurrences
            .iter()
            .filter(|occurrence| occurrence.parent == Some(types_occurrence))
        {
            let Some(content_type) = utf8(&mapping.directive.name.value) else {
                issues.push(issue(
                    &self.origin(mapping.id),
                    E_INVALID_VALUE,
                    "nginx MIME content type is not UTF-8",
                ));
                continue;
            };
            origins.push(self.origin(mapping.id));
            for extension in &mapping.directive.arguments {
                let Some(extension) = utf8(&extension.value) else {
                    issues.push(issue(
                        &self.origin(mapping.id),
                        E_INVALID_VALUE,
                        "nginx MIME extension is not UTF-8",
                    ));
                    continue;
                };
                let entry = HttpMimeType {
                    extension: extension.to_ascii_lowercase(),
                    content_type: content_type.to_owned(),
                };
                if let Some(existing) = types
                    .iter_mut()
                    .find(|candidate| candidate.extension == entry.extension)
                {
                    *existing = entry;
                } else {
                    types.push(entry);
                }
            }
        }
        HttpStaticMimePolicy {
            default_type,
            types,
        }
    }

    fn effective_types_occurrence(&self, scope: OccurrenceId) -> Option<OccurrenceId> {
        let mut current = Some(scope);
        while let Some(scope) = current {
            if let Some(types) = self
                .graph
                .expanded_occurrences
                .iter()
                .rev()
                .find(|occurrence| {
                    occurrence.parent == Some(scope) && occurrence.directive.name.value == b"types"
                })
            {
                return Some(types.id);
            }
            current = self
                .occurrence(scope)
                .and_then(|occurrence| occurrence.parent);
        }
        None
    }

    fn lower_literal_headers(
        &self,
        location: &EffectiveLocation,
        origins: &mut Vec<DirectiveOrigin>,
        issues: &mut Vec<LowerIssue>,
    ) -> Vec<HttpLiteralHeader> {
        let mut headers = Vec::new();
        for policy in self.effective_list_policy_chain(location.origin.occurrence, b"add_header") {
            origins.extend(policy.origins.clone());
            let ([name, value] | [name, value, _]) = policy.arguments.as_slice() else {
                continue;
            };
            let always = policy
                .arguments
                .get(2)
                .is_some_and(|value| value == b"always");
            if policy.arguments.len() == 3 && !always {
                issues.push(issue(
                    policy.origins.last().unwrap_or(&location.origin),
                    E_INVALID_VALUE,
                    "nginx add_header third argument must be always",
                ));
                continue;
            }
            let (Some(name), Some(value)) = (utf8(name), utf8(value)) else {
                issues.push(issue(
                    policy.origins.last().unwrap_or(&location.origin),
                    E_INVALID_VALUE,
                    "nginx static response header is not UTF-8",
                ));
                continue;
            };
            headers.push(HttpLiteralHeader {
                name: name.into(),
                value: value.into(),
                always,
            });
        }
        headers
    }

    fn lower_error_responses(
        &self,
        location: &EffectiveLocation,
        origins: &mut Vec<DirectiveOrigin>,
        issues: &mut Vec<LowerIssue>,
    ) -> Vec<HttpStaticErrorResponse> {
        let mut responses = Vec::new();
        for policy in self.effective_list_policy_chain(location.origin.occurrence, b"error_page") {
            origins.extend(policy.origins.clone());
            let Some(file) = policy.arguments.last().and_then(|value| utf8(value)) else {
                continue;
            };
            let statuses = policy.arguments[..policy.arguments.len().saturating_sub(1)]
                .iter()
                .map(|value| {
                    utf8(value)
                        .and_then(|value| value.parse::<u16>().ok())
                        .filter(|status| (400..=599).contains(status))
                })
                .collect::<Option<Vec<_>>>();
            let canonical_file = canonicalize_http_path(file)
                .filter(|canonical| canonical.as_ref() == file)
                .filter(|_| file.starts_with('/'));
            let (Some(statuses), Some(canonical_file)) = (statuses, canonical_file) else {
                issues.push(issue(
                    policy.origins.last().unwrap_or(&location.origin),
                    E_INVALID_VALUE,
                    "nginx error_page requires 400..=599 statuses and an absolute canonical URI target",
                ));
                continue;
            };
            responses.push(HttpStaticErrorResponse {
                statuses,
                file: Some(PathBuf::from(canonical_file.trim_start_matches('/'))),
                body: None,
                headers: Vec::new(),
                internal_redirect: Some(canonical_file.into_owned()),
            });
        }
        responses
    }

    fn error_page_matches_status(&self, scope: OccurrenceId, status: u16) -> bool {
        self.effective_list_policy_chain(scope, b"error_page")
            .iter()
            .any(|policy| {
                policy.arguments[..policy.arguments.len().saturating_sub(1)]
                    .iter()
                    .any(|value| {
                        utf8(value).and_then(|value| value.parse::<u16>().ok()) == Some(status)
                    })
            })
    }

    fn policy_enabled(&self, scope: OccurrenceId, name: &[u8], default: bool) -> bool {
        self.effective_policy(scope, name)
            .and_then(|policy| policy.arguments.first().cloned())
            .map_or(default, |value| value == b"on")
    }
}

fn nginx_default_404(server: &str) -> HttpStaticErrorResponse {
    HttpStaticErrorResponse {
        statuses: vec![404],
        file: None,
        body: Some(format!(
            "<html>\r\n<head><title>404 Not Found</title></head>\r\n<body>\r\n<center><h1>404 Not Found</h1></center>\r\n<hr><center>{server}</center>\r\n</body>\r\n</html>\r\n"
        )),
        headers: vec![
            HttpLiteralHeader {
                name: "server".into(),
                value: server.into(),
                always: true,
            },
            HttpLiteralHeader {
                name: "content-type".into(),
                value: "text/html".into(),
                always: true,
            },
        ],
        internal_redirect: None,
    }
}

fn canonical_proxy_endpoint(
    endpoint: &StaticEndpoint,
    proxy: &crate::nginx::EffectiveProxyPass,
    secure: bool,
) -> oxiroute_config::UpstreamEndpoint {
    let mut endpoint = canonical_endpoint(endpoint);
    if proxy.scheme == ProxyPassScheme::Downstream
        && secure
        && !authority_has_explicit_port(&proxy.authority)
    {
        match &mut endpoint {
            oxiroute_config::UpstreamEndpoint::Socket { address } => address.set_port(443),
            oxiroute_config::UpstreamEndpoint::Dns { port, .. } => *port = 443,
            oxiroute_config::UpstreamEndpoint::Unix { .. } => {}
        }
    }
    endpoint
}

#[allow(clippy::naive_bytecount)]
fn authority_has_explicit_port(authority: &[u8]) -> bool {
    if authority.starts_with(b"[") {
        return authority
            .iter()
            .position(|byte| *byte == b']')
            .is_some_and(|end| authority.get(end + 1) == Some(&b':'));
    }
    authority.iter().filter(|byte| **byte == b':').count() == 1
}

#[derive(Clone)]
struct LoweredRoute {
    route: HttpRoute,
    pool: Option<PoolCandidate>,
    origins: Vec<DirectiveOrigin>,
}

struct LoweredLocation {
    routes: Vec<LoweredRoute>,
}

struct LoweredProxy {
    pool: PoolCandidate,
    policy: HttpProxyPolicy,
    timeouts: ProxyTimeouts,
    origins: Vec<DirectiveOrigin>,
}

#[derive(Clone, Copy)]
struct ProxyTimeouts {
    connect: u64,
    read: u64,
    write: u64,
}

fn canonical_directory(value: &[u8]) -> Option<PathBuf> {
    let value = utf8(value)?;
    let normalized = if value == "/" {
        value
    } else {
        value.trim_end_matches('/')
    };
    let path = Path::new(normalized);
    (path.is_absolute()
        && !value.contains("//")
        && !value.split('/').any(|part| matches!(part, "." | "..")))
    .then(|| path.to_path_buf())
}

fn canonical_file_path(value: &[u8]) -> Option<PathBuf> {
    let value = utf8(value)?;
    let path = Path::new(value);
    (path.is_absolute()
        && !value.contains("//")
        && !value.ends_with('/')
        && !value.split('/').any(|part| matches!(part, "." | "..")))
    .then(|| path.to_path_buf())
}
