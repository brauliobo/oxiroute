use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
};

use oxiroute_config::{
    HttpCookiePathRewrite, HttpHostSelector, HttpLiteralHeader, HttpPathSelector, HttpProxyPolicy,
    HttpRedirectLocation, HttpRequestHeaderMutation, HttpRequestHeaderValue,
    HttpResponseHeaderMutation, HttpRetryPolicy, HttpRetryTrigger, HttpRoute, HttpRouteAction,
    HttpService, HttpUpstreamHost, HttpVersionPolicy, Listener, ListenerBind, Protocol,
    UpstreamPool, canonicalize_http_path,
};

use crate::{E_INVALID_VALUE, E_SEMANTICS_NOT_REPRESENTABLE, E_UNSUPPORTED_FEATURE};

use crate::nginx::{
    DirectiveOrigin, EffectiveBind, EffectiveHttp, EffectiveLocation, EffectiveServer,
    ListenEndpoint, LocationKind, OccurrenceId, ServerNameKind,
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

fn listener_bind(bind: &EffectiveBind, servers: &[&EffectiveServer]) -> Option<ListenerBind> {
    match &bind.endpoint {
        ListenEndpoint::Unix { path } => Some(ListenerBind::Unix { path: path.clone() }),
        ListenEndpoint::Socket { address, port } => socket_addr(&bind.endpoint)
            .or_else(|| {
                (address == b"*"
                    && servers.iter().all(|server| {
                        matching_listen(server, &bind.endpoint)
                            .value
                            .as_ref()
                            .is_some_and(|value| explicit_ipv4_wildcard(&value.value, *port))
                    }))
                .then(|| SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), *port))
            })
            .map(|address| ListenerBind::Socket { address }),
    }
}

fn explicit_ipv4_wildcard(value: &[u8], expected_port: u16) -> bool {
    value == b"0.0.0.0"
        || value
            .strip_prefix(b"0.0.0.0:")
            .and_then(utf8)
            .and_then(|port| port.parse::<u16>().ok())
            == Some(expected_port)
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
        let candidate = self.lower_bind_routes(
            http,
            bind,
            &servers,
            listener_bind.clone(),
            service_name,
            http_index,
            bind_index,
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
        issues: &mut Vec<LowerIssue>,
    ) -> Option<BindCandidate> {
        let mut routes = Vec::new();
        let mut pools = Vec::new();
        let mut pool_names = HashSet::new();
        let mut route_origins = Vec::new();
        let mut body_policy = None;
        let mut timeout_policy = None;
        let mut all_origins = Vec::new();
        for server in servers {
            let hosts = match Self::route_hosts(server, bind.default_server) {
                Ok(hosts) => hosts,
                Err(server_issues) => {
                    issues.extend(server_issues);
                    continue;
                }
            };
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
                ) {
                    Ok(lowered) => {
                        if body_policy.is_some_and(|policy| policy != lowered.body_bytes) {
                            issues.push(issue(
                                &location.origin,
                                E_SEMANTICS_NOT_REPRESENTABLE,
                                "nginx locations use different request-body limits, but canonical HTTP limits are per service",
                            ));
                        } else {
                            body_policy = Some(lowered.body_bytes);
                        }
                        if let Some(timeout) = lowered.timeout_ms {
                            if timeout_policy.is_some_and(|policy| policy != timeout) {
                                issues.push(issue(
                                    &location.origin,
                                    E_SEMANTICS_NOT_REPRESENTABLE,
                                    "nginx locations use different proxy timeout policies, but canonical HTTP timeouts are per service",
                                ));
                            } else {
                                timeout_policy = Some(timeout);
                            }
                        }
                        for mut route in lowered.routes {
                            route.origins.push(server.origin.clone());
                            route
                                .origins
                                .extend(server.server_names.iter().map(|name| name.origin.clone()));
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
        Some(BindCandidate {
            listener: Listener {
                name: format!("nginx-http-listener-{http_index}-{bind_index}"),
                bind: listener_bind,
                protocol: Protocol::Http,
                service: Some(service_name.clone()),
                tls_profile: tls_profile.as_ref().map(|profile| profile.name.clone()),
                max_connections: None,
            },
            service: HttpService {
                name: service_name,
                routes,
                upstream_io_timeout_ms: timeout_policy.unwrap_or(NGINX_DEFAULT_PROXY_TIMEOUT_MS),
                max_request_body_bytes: body_policy.unwrap_or(Some(NGINX_DEFAULT_BODY_BYTES)),
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
        default_server: OccurrenceId,
    ) -> Result<Vec<Option<HttpHostSelector>>, Vec<LowerIssue>> {
        if server.origin.occurrence == default_server {
            return Ok(vec![None]);
        }
        let mut hosts = Vec::new();
        let mut issues = Vec::new();
        for name in &server.server_names {
            let host = match name.kind {
                ServerNameKind::Exact => utf8(&name.normalized).and_then(canonical_exact_host),
                ServerNameKind::LeadingWildcard => {
                    issues.push(issue(
                        &name.origin,
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "nginx leading wildcard host semantics are not exactly representable",
                    ));
                    None
                }
                ServerNameKind::LeadingWildcardAndExact | ServerNameKind::TrailingWildcard => {
                    issues.push(issue(
                        &name.origin,
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "nginx wildcard does not have canonical one-label semantics",
                    ));
                    None
                }
                ServerNameKind::Regex | ServerNameKind::Variable | ServerNameKind::Invalid => None,
            };
            if let Some(host) = host {
                if !hosts.iter().any(|candidate: &String| candidate == &host) {
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
            Ok(hosts
                .into_iter()
                .map(|value| Some(HttpHostSelector::NormalizedHost { value }))
                .collect())
        } else {
            Err(issues)
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "matcher, action, and provenance must be accepted or rejected as one route"
    )]
    fn lower_location(
        &self,
        http: &EffectiveHttp,
        location: &EffectiveLocation,
        hosts: &[Option<HttpHostSelector>],
        service_name: &str,
        http_index: usize,
        route_ordinal: usize,
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
        let (action, pool, timeout_ms, mut origins) = if let Some(value) = return_policy {
            let action = Self::lower_return(&value, &location.origin, &mut issues);
            (
                action.unwrap_or(HttpRouteAction::FixedResponse {
                    status: 500,
                    body: String::new(),
                    headers: Vec::new(),
                }),
                None,
                None,
                value.origins,
            )
        } else if let Some(proxy) = &location.proxy_pass {
            match self.lower_proxy(
                http,
                location,
                proxy,
                service_name,
                http_index,
                route_ordinal,
                &mut issues,
            ) {
                Some(proxy) => (
                    HttpRouteAction::Proxy {
                        upstream_pool: proxy.pool.pool.name.clone(),
                        policy: proxy.policy,
                    },
                    Some(proxy.pool),
                    Some(proxy.timeout_ms),
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
        } else {
            self.block_static(location, &mut issues);
            let mut static_origins = Vec::new();
            for name in [b"root".as_slice(), b"index".as_slice()] {
                for policy in self.effective_list_policy_chain(location.origin.occurrence, name) {
                    static_origins.extend(policy.origins);
                }
            }
            (
                HttpRouteAction::FixedResponse {
                    status: 500,
                    body: String::new(),
                    headers: Vec::new(),
                },
                None,
                None,
                static_origins,
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
                        action: action.clone(),
                    },
                    pool: (index == 0).then(|| pool.clone()).flatten(),
                    origins: origins.clone(),
                })
                .collect(),
            body_bytes,
            timeout_ms,
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
                "proxy_http_version must explicitly be 1.1",
            ));
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
        for name in [b"auth_basic".as_slice(), b"auth_basic_user_file".as_slice()] {
            if let Some(value) = self.effective_policy(location.origin.occurrence, name) {
                let disabled =
                    name == b"auth_basic" && value.arguments.as_slice() == [b"off".to_vec()];
                if !disabled {
                    issues.push(issue(
                        value.origins.last().unwrap_or(&location.origin),
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "nginx Basic authentication does not have bearer-token-file semantics",
                    ));
                }
            }
        }
        None
    }

    fn lower_return(
        value: &PolicyValue,
        fallback: &DirectiveOrigin,
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
                }
            } else {
                HttpRedirectLocation::Literal {
                    value: payload.into(),
                }
            };
            return Some(HttpRouteAction::Redirect { status, location });
        }
        Some(HttpRouteAction::FixedResponse {
            status,
            body: payload.into(),
            headers: Vec::<HttpLiteralHeader>::new(),
        })
    }

    fn block_static(&self, location: &EffectiveLocation, issues: &mut Vec<LowerIssue>) {
        let Some(root) = self.effective_policy(location.origin.occurrence, b"root") else {
            issues.push(issue(
                &location.origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "nginx location has no proxy, return, or static root action",
            ));
            return;
        };
        let root_origin = root.origins.last().unwrap_or(&location.origin);
        if root
            .arguments
            .first()
            .and_then(|value| canonical_directory(value))
            .is_none()
        {
            issues.push(issue(
                root_origin,
                E_INVALID_VALUE,
                "nginx root is not a canonical absolute directory",
            ));
            return;
        }
        let indexes = self.effective_list_policy_chain(location.origin.occurrence, b"index");
        issues.push(issue(
            indexes
                .last()
                .and_then(|value| value.origins.last())
                .unwrap_or(&location.origin),
            E_SEMANTICS_NOT_REPRESENTABLE,
            "nginx static index handling internally redirects and reruns location selection, but canonical static files open indexes directly",
        ));
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
        issues: &mut Vec<LowerIssue>,
    ) -> Option<LoweredProxy> {
        if proxy.replacement_uri.is_some() {
            issues.push(issue(
                &proxy.origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "proxy_pass URI replacement is not represented by canonical routes",
            ));
        }
        if let Err(origin_issues) = self.validate_proxy_origin(http, proxy) {
            issues.extend(origin_issues);
        }
        let policy_values =
            self.validate_route_policy(location.origin.occurrence, &location.origin);
        if let Err(policy_issues) = policy_values {
            issues.extend(policy_issues);
        }
        let issue_count_before_explicit_proxy_policy = issues.len();
        for name in [
            b"proxy_buffering".as_slice(),
            b"proxy_request_buffering".as_slice(),
        ] {
            let policy = self.effective_policy(location.origin.occurrence, name);
            if policy
                .as_ref()
                .is_none_or(|value| value.arguments.as_slice() != [b"off".to_vec()])
            {
                issues.push(issue(
                    policy
                        .as_ref()
                        .and_then(|value| value.origins.last())
                        .unwrap_or(&proxy.origin),
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    format!(
                        "{} must be explicitly disabled to match streaming canonical proxy semantics",
                        utf8(name).expect("static nginx directive name")
                    ),
                ));
            }
        }
        let pool = Self::proxy_pool(http, proxy, service_name, http_index, route_ordinal, issues)?;
        let upstream_host = self.lower_proxy_headers(location, proxy, issues);
        let request_headers = upstream_host
            .as_ref()
            .map_or_else(Vec::new, |(_, headers)| headers.clone());
        let upstream_host =
            upstream_host.map_or(HttpUpstreamHost::PreserveIncoming, |(host, _)| host);
        self.validate_response_controls(location, proxy, issues);
        let response_headers = self.lower_response_headers(location, issues);
        let response_cookie_path_rewrites = self.lower_cookie_rewrites(location, issues);
        let retry = self.lower_proxy_retry(location, pool.pool.endpoints.len(), issues);
        if issues.len() > issue_count_before_explicit_proxy_policy {
            issues.push(issue(
                &proxy.origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "nginx proxy defaults remain blockers unless Host, buffering, request buffering, and retry behavior are explicit",
            ));
        }
        let timeout_ms =
            self.uniform_proxy_timeout(location.origin.occurrence, &location.origin, issues);
        if !issues.is_empty() {
            return None;
        }
        let mut origins = vec![proxy.origin.clone(), location.origin.clone()];
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
                retry,
                cache: None,
            },
            timeout_ms,
            origins,
        })
    }

    fn proxy_pool(
        http: &EffectiveHttp,
        proxy: &crate::nginx::EffectiveProxyPass,
        service_name: &str,
        http_index: usize,
        route_ordinal: usize,
        issues: &mut Vec<LowerIssue>,
    ) -> Option<PoolCandidate> {
        let (endpoints, origin, endpoint_origins) = match proxy.upstream {
            crate::nginx::UpstreamReference::Direct => (
                proxy
                    .direct_endpoint
                    .as_ref()
                    .map(canonical_endpoint)
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
        Some(PoolCandidate {
            pool: UpstreamPool {
                name,
                endpoints,
                algorithm: oxiroute_config::UpstreamAlgorithm::RoundRobin,
                health_check: None,
                tls: None,
                http_versions: HttpVersionPolicy::default(),
            },
            origin,
            endpoint_origins,
        })
    }

    fn lower_proxy_headers(
        &self,
        location: &EffectiveLocation,
        proxy: &crate::nginx::EffectiveProxyPass,
        issues: &mut Vec<LowerIssue>,
    ) -> Option<(HttpUpstreamHost, Vec<HttpRequestHeaderMutation>)> {
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
                host = Self::proxy_host_policy(value, proxy, origin, issues);
                continue;
            }
            let mutation = if value.is_empty() {
                Some(HttpRequestHeaderMutation::Remove { name: name.into() })
            } else {
                Self::request_header_value(value, proxy).map(|value| {
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
        let Some(host) = host else {
            issues.push(issue(
                &proxy.origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "proxy_set_header Host must be explicit for canonical proxy lowering",
            ));
            return None;
        };
        Some((host, headers))
    }

    fn proxy_host_policy(
        value: &[u8],
        proxy: &crate::nginx::EffectiveProxyPass,
        origin: &DirectiveOrigin,
        issues: &mut Vec<LowerIssue>,
    ) -> Option<HttpUpstreamHost> {
        match value {
            b"$http_host" => Some(HttpUpstreamHost::PreserveIncoming),
            b"$proxy_host" => utf8(&proxy.authority).map(|value| HttpUpstreamHost::Literal {
                value: value.into(),
            }),
            b"$host" => {
                issues.push(issue(
                    origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "nginx $host fallback semantics are not an exact incoming-authority policy",
                ));
                None
            }
            value if !value.contains(&b'$') => utf8(value).map(|value| HttpUpstreamHost::Literal {
                value: value.into(),
            }),
            _ => None,
        }
    }

    fn request_header_value(
        value: &[u8],
        proxy: &crate::nginx::EffectiveProxyPass,
    ) -> Option<HttpRequestHeaderValue> {
        match value {
            b"$http_host" => Some(HttpRequestHeaderValue::IncomingAuthority),
            b"$host" => Some(HttpRequestHeaderValue::NormalizedHost),
            b"$remote_addr" => Some(HttpRequestHeaderValue::ClientIp),
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
        hidden
            .into_iter()
            .map(|name| HttpResponseHeaderMutation::Remove { name })
            .collect()
    }

    fn validate_response_controls(
        &self,
        location: &EffectiveLocation,
        proxy: &crate::nginx::EffectiveProxyPass,
        issues: &mut Vec<LowerIssue>,
    ) {
        let policies =
            self.effective_list_policy_chain(location.origin.occurrence, b"proxy_ignore_headers");
        let ignored = policies
            .iter()
            .flat_map(|policy| &policy.arguments)
            .collect::<Vec<_>>();
        if NGINX_RESPONSE_CONTROL_HEADERS.iter().all(|required| {
            ignored
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(required))
        }) {
            return;
        }
        issues.push(issue(
            policies
                .last()
                .and_then(|policy| policy.origins.last())
                .unwrap_or(&proxy.origin),
            E_SEMANTICS_NOT_REPRESENTABLE,
            "nginx X-Accel response controls must all be disabled with proxy_ignore_headers before canonical proxy lowering",
        ));
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
            issues.push(issue(
                &location.origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "proxy_next_upstream must be explicit for canonical retry lowering",
            ));
            return HttpRetryPolicy::default();
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
                b"error" => triggers.push(HttpRetryTrigger::ConnectFailure),
                b"timeout" => triggers.push(HttpRetryTrigger::ConnectTimeout),
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

    fn uniform_proxy_timeout(
        &self,
        scope: OccurrenceId,
        origin: &DirectiveOrigin,
        issues: &mut Vec<LowerIssue>,
    ) -> u64 {
        let connect = collect_result(
            self.proxy_timeout(scope, b"proxy_connect_timeout", origin),
            issues,
        );
        let read = collect_result(
            self.proxy_timeout(scope, b"proxy_read_timeout", origin),
            issues,
        );
        let send = collect_result(
            self.proxy_timeout(scope, b"proxy_send_timeout", origin),
            issues,
        );
        match (connect, read, send) {
            (Some(connect), Some(read), Some(send)) if connect == read && read == send => connect,
            (Some(_), Some(_), Some(_)) => {
                issues.push(issue(
                    origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "nginx connect, read, and send timeouts are not one uniform I/O timeout",
                ));
                NGINX_DEFAULT_PROXY_TIMEOUT_MS
            }
            _ => NGINX_DEFAULT_PROXY_TIMEOUT_MS,
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
}

#[derive(Clone)]
struct LoweredRoute {
    route: HttpRoute,
    pool: Option<PoolCandidate>,
    origins: Vec<DirectiveOrigin>,
}

struct LoweredLocation {
    routes: Vec<LoweredRoute>,
    body_bytes: Option<u64>,
    timeout_ms: Option<u64>,
}

struct LoweredProxy {
    pool: PoolCandidate,
    policy: HttpProxyPolicy,
    timeout_ms: u64,
    origins: Vec<DirectiveOrigin>,
}

fn canonical_directory(value: &[u8]) -> Option<PathBuf> {
    let value = utf8(value)?;
    let path = Path::new(value);
    (path.is_absolute()
        && !value.contains("//")
        && (value == "/" || !value.ends_with('/'))
        && !value.split('/').any(|part| matches!(part, "." | "..")))
    .then(|| path.to_path_buf())
}
