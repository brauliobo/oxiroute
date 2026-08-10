use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
};

use http::header::{HeaderName, HeaderValue};
use oxiroute_config::{
    AccessLogPolicy, DnsResolutionPolicy, DownstreamTimeoutPolicy, HttpAccessPolicy,
    HttpCookieAttributePolicy, HttpCookiePathRewrite, HttpGzipMinimumVersion, HttpGzipPolicy,
    HttpHostSelector, HttpLiteralHeader, HttpMimeType, HttpPathSelector, HttpProxyPathRewrite,
    HttpProxyPolicy, HttpRedirectLocation, HttpRequestHeaderMutation, HttpRequestHeaderValue,
    HttpResponseHeaderMutation, HttpRetryPolicy, HttpRetryTrigger, HttpRoute, HttpRouteAction,
    HttpRoutePolicy, HttpSameSite, HttpService, HttpStaticErrorResponse, HttpStaticMimePolicy,
    HttpStaticPathMapping, HttpStaticTryFile, HttpUpstreamHost, HttpVersionPolicy, Listener,
    ListenerBind, Protocol, UpstreamAlgorithm, UpstreamConnectionReuse, UpstreamEndpoint,
    UpstreamPool, UpstreamServer, UpstreamTls, canonical_dns_name as canonical_endpoint_dns_name,
    canonicalize_http_path,
};

use crate::canonical::absolute_file_path;
use crate::{E_INVALID_VALUE, E_SEMANTICS_NOT_REPRESENTABLE, E_UNSUPPORTED_FEATURE};

use crate::nginx::{
    DirectiveOrigin, EffectiveBind, EffectiveHttp, EffectiveLocation, EffectiveServer,
    ListenEndpoint, LocationKind, OccurrenceId, ProxyPassScheme, ServerNameKind, StaticEndpoint,
    semantic::certbot_host_condition,
};

use super::{
    BindBlock, BindCandidate, GzipOrigins, LowerIssue, Lowerer, PoolCandidate,
    provenance::{PolicyValue, collect_result, issue, utf8},
    upstream::canonical_endpoint,
};

const NGINX_DEFAULT_BODY_BYTES: u64 = 1024 * 1024;
const NGINX_DEFAULT_PROXY_TIMEOUT_MS: u64 = 60_000;
const NGINX_DEFAULT_GZIP_LEVEL: u8 = 1;
const NGINX_DEFAULT_GZIP_MIN_LENGTH_BYTES: u64 = 20;
const NGINX_MAX_HEADER_NAME_BYTES: usize = 64;
const NGINX_MAX_HEADER_VALUE_BYTES: usize = 8192;
const NGINX_MAX_LITERAL_HEADERS: usize = 32;
const NGINX_MAX_STATIC_ERROR_RESPONSES: usize = 16;
const NGINX_MAX_STATIC_ERROR_STATUSES: usize = 16;
const NGINX_MAX_STATIC_ERROR_TARGET_BYTES: usize = 1024;
const NGINX_IMPLICIT_GZIP_TYPE: &str = "text/html";
const NGINX_GZIP_PROXIED_MODES: [&[u8]; 9] = [
    b"off",
    b"expired",
    b"no-cache",
    b"no-store",
    b"private",
    b"no_last_modified",
    b"no_etag",
    b"auth",
    b"any",
];
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
    canonical_endpoint_dns_name(host).ok()
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

pub(super) fn parse_size(value: &[u8]) -> Option<u64> {
    parse_size_inner(value, false)
}

fn parse_nonnegative_size(value: &[u8]) -> Option<u64> {
    parse_size_inner(value, true)
}

fn parse_size_inner(value: &[u8], allow_zero: bool) -> Option<u64> {
    let (digits, multiplier) = match value.last().copied() {
        Some(b'k' | b'K') => (&value[..value.len() - 1], 1024_u64),
        Some(b'm' | b'M') => (&value[..value.len() - 1], 1024_u64.pow(2)),
        Some(b'g' | b'G') => (&value[..value.len() - 1], 1024_u64.pow(3)),
        Some(_) => (value, 1),
        None => return None,
    };
    let amount = utf8(digits)?.parse::<u64>().ok()?;
    let bytes = amount.checked_mul(multiplier)?;
    ((allow_zero || bytes > 0) && bytes <= 9_007_199_254_740_991).then_some(bytes)
}

pub(super) fn parse_duration_ms(value: &[u8]) -> Option<u64> {
    const MAX_CANONICAL_INTEGER: u64 = 9_007_199_254_740_991;

    if value.iter().all(u8::is_ascii_digit) {
        let amount = utf8(value)?.parse::<u64>().ok()?;
        let milliseconds = amount.checked_mul(1_000)?;
        return (milliseconds > 0 && milliseconds <= MAX_CANONICAL_INTEGER).then_some(milliseconds);
    }

    let mut remaining = value;
    let mut previous_rank = None;
    let mut only_bare_value_remains = false;
    let mut total = 0_u64;
    while !remaining.is_empty() {
        let digit_count = remaining
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        if digit_count == 0 {
            return None;
        }
        let amount = utf8(&remaining[..digit_count])?.parse::<u64>().ok()?;
        remaining = &remaining[digit_count..];
        if remaining.is_empty() {
            total = total.checked_add(amount.checked_mul(1_000)?)?;
            break;
        }
        if only_bare_value_remains {
            return None;
        }
        let (rank, multiplier, suffix_bytes) = if remaining.starts_with(b"ms") {
            (7_u8, 1_u64, 2_usize)
        } else {
            match remaining[0] {
                b'w' => (2, 7 * 86_400_000, 1),
                b'd' => (3, 86_400_000, 1),
                b'h' => (4, 3_600_000, 1),
                b'm' => (5, 60_000, 1),
                b's' => (6, 1_000, 1),
                _ => return None,
            }
        };
        if previous_rank.is_some_and(|previous| rank <= previous) {
            return None;
        }
        previous_rank = Some(rank);
        remaining = &remaining[suffix_bytes..];
        total = total.checked_add(amount.checked_mul(multiplier)?)?;
        if remaining.first() == Some(&b' ') {
            if rank >= 6 {
                return None;
            }
            remaining = remaining.trim_ascii_start();
            only_bare_value_remains = true;
        }
    }
    (total > 0 && total <= MAX_CANONICAL_INTEGER).then_some(total)
}

fn valid_gzip_type(value: &str) -> bool {
    let Some((kind, subtype)) = value.split_once('/') else {
        return false;
    };
    !kind.is_empty()
        && !subtype.is_empty()
        && !subtype.contains('/')
        && value.len() <= 128
        && kind.bytes().chain(subtype.bytes()).all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

enum NginxProxyValue {
    IncomingAuthority,
    NginxHost(String),
    ClientIp,
    AppendedXForwardedFor,
    DownstreamScheme,
    IncomingUpgrade,
    Literal(String),
}

impl NginxProxyValue {
    fn into_upstream_host(self) -> Option<HttpUpstreamHost> {
        match self {
            Self::IncomingAuthority => Some(HttpUpstreamHost::PreserveIncoming),
            Self::NginxHost(fallback) => Some(HttpUpstreamHost::NginxHost { fallback }),
            Self::Literal(value) => Some(HttpUpstreamHost::Literal { value }),
            Self::ClientIp
            | Self::AppendedXForwardedFor
            | Self::DownstreamScheme
            | Self::IncomingUpgrade => None,
        }
    }

    fn into_request_header_value(self) -> HttpRequestHeaderValue {
        match self {
            Self::IncomingAuthority => HttpRequestHeaderValue::IncomingAuthority,
            Self::NginxHost(fallback) => HttpRequestHeaderValue::NginxHost { fallback },
            Self::ClientIp => HttpRequestHeaderValue::ClientIp,
            Self::AppendedXForwardedFor => HttpRequestHeaderValue::AppendedXForwardedFor {
                max_bytes: 8_192,
                except_source_cidrs: Vec::new(),
            },
            Self::DownstreamScheme => HttpRequestHeaderValue::DownstreamScheme,
            Self::IncomingUpgrade => HttpRequestHeaderValue::IncomingHeader {
                name: "upgrade".into(),
                max_bytes: 8_192,
            },
            Self::Literal(value) => HttpRequestHeaderValue::Literal { value },
        }
    }
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
        let (gzip, gzip_origins) = match self.lower_gzip(bind, &servers) {
            Ok(gzip) => gzip,
            Err(gzip_issues) => {
                issues.extend(gzip_issues);
                (None, GzipOrigins::default())
            }
        };
        let mut uses_default_access_log = false;
        let mut disables_access_log = false;
        for server in &servers {
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
            gzip,
            gzip_origins,
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
        gzip: Option<HttpGzipPolicy>,
        gzip_origins: GzipOrigins,
        issues: &mut Vec<LowerIssue>,
    ) -> Option<BindCandidate> {
        let mut routes = Vec::new();
        let mut pools = Vec::new();
        let mut pool_names = HashSet::new();
        let mut route_origins = Vec::new();
        let mut all_origins = Vec::new();
        let (downstream_timeouts, downstream_timeout_origins) =
            self.lower_downstream_timeouts(bind, servers, issues);
        for server in servers {
            if !Self::server_participates(bind, server) {
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
                    .map(|host| match host.parse::<IpAddr>() {
                        Ok(IpAddr::V6(ip)) => format!("[{ip}]"),
                        _ => host,
                    })
            });
            all_origins.push(server.origin.clone());
            let mut has_local_catch_all = false;
            for location in &server.locations {
                let synthetic_name = self
                    .occurrence(location.origin.occurrence)
                    .map(|occurrence| occurrence.directive.name.value.as_slice());
                let location_hosts = if synthetic_name == Some(b"if") {
                    let host = self
                        .occurrence(location.origin.occurrence)
                        .and_then(|occurrence| certbot_host_condition(&occurrence.directive))
                        .and_then(|host| utf8(host).and_then(canonical_exact_host));
                    let Some(host) = host else {
                        issues.push(issue(
                            &location.origin,
                            E_INVALID_VALUE,
                            "nginx host redirect condition is not a canonical exact host",
                        ));
                        continue;
                    };
                    vec![Some(HttpHostSelector::NormalizedHost { value: host })]
                } else if synthetic_name == Some(b"return") && hosts.contains(&None) {
                    vec![None]
                } else {
                    hosts.clone()
                };
                let top_level_catch_all = location.kind == LocationKind::Prefix
                    && location
                        .path
                        .as_ref()
                        .is_some_and(|path| path.value == b"/");
                has_local_catch_all |= top_level_catch_all;
                match self.lower_location(
                    http,
                    location,
                    &location_hosts,
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
                            if let Some(pool) = route.pool
                                && pool_names.insert(pool.pool.name.clone())
                            {
                                pools.push(pool);
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
        all_origins.extend(gzip_origins.all());
        all_origins.extend(downstream_timeout_origins);
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
        let upstream_io_timeout_ms = routes
            .iter()
            .filter_map(|route| {
                matches!(&route.action, HttpRouteAction::Proxy { .. }).then_some(
                    route
                        .policy
                        .connect_timeout_ms
                        .max(route.policy.read_timeout_ms)
                        .max(route.policy.write_timeout_ms),
                )
            })
            .max()
            .unwrap_or(NGINX_DEFAULT_PROXY_TIMEOUT_MS);
        Some(BindCandidate {
            listener: Listener {
                name: format!("nginx-http-listener-{http_index}-{bind_index}"),
                bind: listener_bind,
                protocol: Protocol::Http,
                service: Some(service_name.clone()),
                tls_profile: tls_profile.as_ref().map(|profile| profile.name.clone()),
                proxy_protocol: None,
                max_connections: None,
                downstream_timeouts,
            },
            service: HttpService {
                name: service_name,
                routes,
                automatic_response_headers: true,
                upstream_io_timeout_ms,
                max_request_body_bytes: Some(NGINX_DEFAULT_BODY_BYTES),
                gzip,
                access_log,
            },
            pools,
            certificates,
            tls_profile,
            origins: all_origins,
            gzip_origins,
            route_origins,
        })
    }

    fn lower_gzip(
        &self,
        bind: &EffectiveBind,
        servers: &[&EffectiveServer],
    ) -> Result<(Option<HttpGzipPolicy>, GzipOrigins), Vec<LowerIssue>> {
        let mut effective: Option<Option<HttpGzipPolicy>> = None;
        let mut origins = GzipOrigins::default();
        let mut issues = Vec::new();
        for server in servers
            .iter()
            .copied()
            .filter(|server| Self::server_participates(bind, server))
        {
            if let Some((policy, policy_origins)) = self.effective_gzip(server, &mut issues) {
                if effective
                    .as_ref()
                    .is_some_and(|existing| existing.as_ref() != policy.as_ref())
                {
                    let mismatch_origins = policy_origins.all();
                    issues.push(issue(
                        mismatch_origins.last().unwrap_or(&server.origin),
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "virtual servers on one nginx bind have different effective gzip policies",
                    ));
                } else if effective.is_none() {
                    effective = Some(policy.clone());
                }
                origins.extend(policy_origins);
            }
        }
        if issues.is_empty() {
            Ok((effective.unwrap_or(None), origins))
        } else {
            Err(issues)
        }
    }

    fn lower_downstream_timeouts(
        &self,
        bind: &EffectiveBind,
        servers: &[&EffectiveServer],
        issues: &mut Vec<LowerIssue>,
    ) -> (DownstreamTimeoutPolicy, Vec<DirectiveOrigin>) {
        let mut values = Vec::new();
        let mut origins = Vec::new();
        let mut invalid = false;
        for server in servers
            .iter()
            .copied()
            .filter(|server| Self::server_participates(bind, server))
        {
            let Some(policy) =
                self.effective_policy(server.origin.occurrence, b"keepalive_timeout")
            else {
                values.push(None);
                continue;
            };
            origins.extend(policy.origins.clone());
            let origin = policy.origins.last().unwrap_or(&server.origin);
            let value = match policy.arguments.as_slice() {
                [timeout] => {
                    if let Some(milliseconds) = parse_duration_ms(timeout) {
                        Some(milliseconds)
                    } else {
                        invalid = true;
                        issues.push(issue(
                            origin,
                            E_INVALID_VALUE,
                            "keepalive_timeout must be a positive finite nginx duration",
                        ));
                        None
                    }
                }
                [_, _] => {
                    invalid = true;
                    issues.push(issue(
                        origin,
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "keepalive_timeout header timeout is not represented by the canonical listener policy",
                    ));
                    None
                }
                _ => {
                    invalid = true;
                    issues.push(issue(
                        origin,
                        E_INVALID_VALUE,
                        "keepalive_timeout requires one positive finite timeout",
                    ));
                    None
                }
            };
            values.push(value);
        }

        let distinct = values.iter().copied().collect::<HashSet<_>>();
        if !invalid && distinct.len() > 1 {
            let origin = origins.first().or_else(|| {
                servers
                    .iter()
                    .find(|server| Self::server_participates(bind, server))
                    .map(|server| &server.origin)
            });
            if let Some(origin) = origin {
                issues.push(issue(
                    origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "virtual servers on one nginx bind have different effective keepalive_timeout policies",
                ));
            }
            invalid = true;
        }

        (
            DownstreamTimeoutPolicy {
                client_timeout_ms: None,
                request_timeout_ms: None,
                keepalive_timeout_ms: (!invalid)
                    .then(|| distinct.into_iter().next().flatten())
                    .flatten(),
            },
            origins,
        )
    }

    #[expect(
        clippy::too_many_lines,
        reason = "effective nginx gzip inheritance, validation, and provenance are one policy"
    )]
    fn effective_gzip(
        &self,
        server: &EffectiveServer,
        issues: &mut Vec<LowerIssue>,
    ) -> Option<(Option<HttpGzipPolicy>, GzipOrigins)> {
        let gzip = self.effective_policy(server.origin.occurrence, b"gzip");
        let level = self.effective_policy(server.origin.occurrence, b"gzip_comp_level");
        let types = self.effective_policy(server.origin.occurrence, b"gzip_types");
        let min_length = self.effective_policy(server.origin.occurrence, b"gzip_min_length");
        let min_http_version =
            self.effective_policy(server.origin.occurrence, b"gzip_http_version");
        let proxied = self.effective_policy(server.origin.occurrence, b"gzip_proxied");
        let vary = self.effective_policy(server.origin.occurrence, b"gzip_vary");

        let compression_level = level
            .as_ref()
            .map_or(Some(NGINX_DEFAULT_GZIP_LEVEL), |policy| {
                let parsed = policy
                    .arguments
                    .first()
                    .and_then(|value| utf8(value))
                    .and_then(|value| value.parse::<u8>().ok())
                    .filter(|value| (1..=9).contains(value));
                if parsed.is_none() {
                    issues.push(issue(
                        policy.origins.last().unwrap_or(&server.origin),
                        E_INVALID_VALUE,
                        "gzip_comp_level must be an integer between 1 and 9",
                    ));
                }
                parsed
            });

        let mut content_types = vec![NGINX_IMPLICIT_GZIP_TYPE.to_owned()];
        if let Some(policy) = &types {
            for value in &policy.arguments {
                let Some(value) = utf8(value) else {
                    issues.push(issue(
                        policy.origins.last().unwrap_or(&server.origin),
                        E_INVALID_VALUE,
                        "gzip_types values must be UTF-8 MIME types",
                    ));
                    continue;
                };
                if value == "*" {
                    issues.push(issue(
                        policy.origins.last().unwrap_or(&server.origin),
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "gzip_types wildcard compression is not representable canonically",
                    ));
                    continue;
                }
                if !valid_gzip_type(value) {
                    issues.push(issue(
                        policy.origins.last().unwrap_or(&server.origin),
                        E_INVALID_VALUE,
                        "gzip_types values must be concrete MIME types",
                    ));
                    continue;
                }
                let value = value.to_ascii_lowercase();
                if !content_types.contains(&value) {
                    content_types.push(value);
                }
            }
        }
        if content_types.len() > 64 {
            issues.push(issue(
                types
                    .as_ref()
                    .and_then(|policy| policy.origins.last())
                    .unwrap_or(&server.origin),
                E_SEMANTICS_NOT_REPRESENTABLE,
                "effective gzip_types exceeds the canonical limit of 64 MIME types",
            ));
        }

        let minimum_length_bytes =
            min_length
                .as_ref()
                .map_or(Some(NGINX_DEFAULT_GZIP_MIN_LENGTH_BYTES), |policy| {
                    let parsed = policy
                        .arguments
                        .first()
                        .and_then(|value| parse_nonnegative_size(value));
                    if parsed.is_none() {
                        issues.push(issue(
                            policy.origins.last().unwrap_or(&server.origin),
                            E_INVALID_VALUE,
                            "gzip_min_length must be a nonnegative nginx size",
                        ));
                    }
                    parsed
                });
        let minimum_http_version =
            min_http_version
                .as_ref()
                .map_or(Some(HttpGzipMinimumVersion::Http11), |policy| match policy
                    .arguments
                    .as_slice()
                {
                    [value] if value == b"1.0" => Some(HttpGzipMinimumVersion::Http10),
                    [value] if value == b"1.1" => Some(HttpGzipMinimumVersion::Http11),
                    _ => {
                        issues.push(issue(
                            policy.origins.last().unwrap_or(&server.origin),
                            E_INVALID_VALUE,
                            "gzip_http_version must be `1.0` or `1.1`",
                        ));
                        None
                    }
                });
        let disable_on_via = proxied.as_ref().map_or(Some(true), |policy| {
            if policy.arguments.as_slice() == [b"off".to_vec()] {
                return Some(true);
            }
            let valid = policy
                .arguments
                .iter()
                .all(|argument| NGINX_GZIP_PROXIED_MODES.contains(&argument.as_slice()));
            issues.push(issue(
                policy.origins.last().unwrap_or(&server.origin),
                if valid {
                    E_SEMANTICS_NOT_REPRESENTABLE
                } else {
                    E_INVALID_VALUE
                },
                if valid {
                    "gzip_proxied modes other than `off` are not representable canonically"
                } else {
                    "gzip_proxied contains an invalid mode"
                },
            ));
            None
        });
        let vary_enabled =
            vary.as_ref()
                .map_or(Some(false), |policy| match policy.arguments.as_slice() {
                    [value] if value == b"off" => Some(false),
                    [value] if value == b"on" => Some(true),
                    _ => {
                        issues.push(issue(
                            policy.origins.last().unwrap_or(&server.origin),
                            E_INVALID_VALUE,
                            "gzip_vary must be `on` or `off`",
                        ));
                        None
                    }
                });

        let enabled = match gzip.as_ref().map(|policy| policy.arguments.as_slice()) {
            None => false,
            Some([value]) if value == b"off" => false,
            Some([value]) if value == b"on" => true,
            Some(_) => {
                let policy = gzip.as_ref().expect("invalid explicit gzip policy");
                issues.push(issue(
                    policy.origins.last().unwrap_or(&server.origin),
                    E_INVALID_VALUE,
                    "gzip must be `on` or `off`",
                ));
                return None;
            }
        };
        let gzip_origins = gzip
            .as_ref()
            .map_or_else(Vec::new, |policy| policy.origins.clone());
        if !enabled {
            return Some((
                None,
                GzipOrigins {
                    gzip: gzip_origins,
                    ..GzipOrigins::default()
                },
            ));
        }
        let (
            Some(level_value),
            Some(min_length_value),
            Some(min_version_value),
            Some(disable_value),
            Some(vary_value),
        ) = (
            compression_level,
            minimum_length_bytes,
            minimum_http_version,
            disable_on_via,
            vary_enabled,
        )
        else {
            return None;
        };
        let field_origins = |policy: Option<&PolicyValue>| {
            policy.map_or_else(|| gzip_origins.clone(), |policy| policy.origins.clone())
        };
        let level_origins = field_origins(level.as_ref());
        let content_type_origins = field_origins(types.as_ref());
        let min_length_origins = field_origins(min_length.as_ref());
        let min_version_origins = field_origins(min_http_version.as_ref());
        let proxied_origins = field_origins(proxied.as_ref());
        let vary_origins = field_origins(vary.as_ref());
        Some((
            Some(HttpGzipPolicy {
                level: level_value,
                content_types,
                min_length_bytes: min_length_value,
                min_http_version: min_version_value,
                disable_on_via: disable_value,
                vary: vary_value,
            }),
            GzipOrigins {
                gzip: gzip_origins,
                level: level_origins,
                content_types: content_type_origins,
                min_length_bytes: min_length_origins,
                min_http_version: min_version_origins,
                disable_on_via: proxied_origins,
                vary: vary_origins,
            },
        ))
    }

    fn server_participates(bind: &EffectiveBind, server: &EffectiveServer) -> bool {
        server.origin.occurrence == bind.default_server
            || bind
                .names
                .iter()
                .any(|name| name.server == server.origin.occurrence)
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
                if !hosts.contains(&Some(host.clone())) {
                    hosts.push(Some(host));
                }
            } else if issues.is_empty() {
                issues.push(issue(
                    &name.origin,
                    E_INVALID_VALUE,
                    "server_name is not a canonical exact or one-label wildcard host",
                ));
            }
        }
        if server.origin.occurrence == bind.default_server {
            hosts.push(None);
        } else if hosts.is_empty() && issues.is_empty() {
            issues.push(issue(
                &server.origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "non-default nginx server has no canonical host identity",
            ));
        }
        if issues.is_empty() {
            Ok(hosts)
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
            let action = self.lower_return(
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
                            request_buffering: self
                                .effective_policy(
                                    location.origin.occurrence,
                                    b"proxy_request_buffering",
                                )
                                .is_none_or(|value| {
                                    value.arguments.as_slice() != [b"off".to_vec()]
                                }),
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
            .is_some_and(|value| value.arguments.as_slice() != [b"1.1".to_vec()])
        {
            let origin = version
                .as_ref()
                .and_then(|value| value.origins.last())
                .unwrap_or(fallback_origin);
            issues.push(issue(
                origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "proxy_http_version must be 1.1 for the canonical upstream HTTP policy",
            ));
        }
        let response_buffering = self.effective_policy(scope, b"proxy_buffering");
        if response_buffering
            .as_ref()
            .is_none_or(|value| value.arguments.as_slice() != [b"off".to_vec()])
        {
            issues.push(issue(
                response_buffering
                    .as_ref()
                    .and_then(|value| value.origins.last())
                    .unwrap_or(fallback_origin),
                E_SEMANTICS_NOT_REPRESENTABLE,
                "proxy_buffering must explicitly be off to match unbuffered runtime semantics",
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
            .and_then(|value| absolute_file_path(value))
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
        &self,
        value: &PolicyValue,
        fallback: &DirectiveOrigin,
        nginx_host_fallback: Option<&str>,
        mut headers: Vec<HttpLiteralHeader>,
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
        if (matches!(status, 301 | 302 | 307 | 308) || (status == 404 && payload.is_empty()))
            && let Some(server) = self.default_error_server.as_deref()
        {
            headers.extend(nginx_default_headers(server));
            self.used_default_error_overlay.set(true);
        }
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
            body: if status == 404 && payload.is_empty() {
                self.default_error_server
                    .as_deref()
                    .map_or_else(String::new, |server| {
                        nginx_error_body(404, "Not Found", server)
                    })
            } else {
                payload.into()
            },
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
        if let Some(server) = self.default_error_server.clone()
            && !self.error_page_matches_status(location.origin.occurrence, 404)
        {
            error_responses.push(nginx_default_404(&server));
            self.used_default_error_overlay.set(true);
        }
        let autoindex = self.policy_enabled(location.origin.occurrence, b"autoindex", false);
        let autoindex_exact_size =
            self.policy_enabled(location.origin.occurrence, b"autoindex_exact_size", true);
        let autoindex_local_time =
            self.policy_enabled(location.origin.occurrence, b"autoindex_localtime", false);
        let etag = match self.effective_policy(location.origin.occurrence, b"etag") {
            None => true,
            Some(policy) => {
                origins.extend(policy.origins.clone());
                match policy.arguments.as_slice() {
                    [value] if value == b"on" => true,
                    [value] if value == b"off" => false,
                    _ => {
                        issues.push(issue(
                            policy.origins.last().unwrap_or(&location.origin),
                            E_INVALID_VALUE,
                            "etag must be `on` or `off`",
                        ));
                        true
                    }
                }
            }
        };

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
                etag,
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
        let upstream_path_rewrite = if let Some(replacement) = proxy.replacement_uri.as_deref() {
            let Some(from) = location.path.as_ref().and_then(|path| utf8(&path.value)) else {
                issues.push(issue(
                    &proxy.origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "proxy_pass URI replacement requires a canonical location path",
                ));
                return None;
            };
            let Some(to) = utf8(replacement) else {
                issues.push(issue(
                    &proxy.origin,
                    E_INVALID_VALUE,
                    "proxy_pass URI replacement is not UTF-8",
                ));
                return None;
            };
            let valid = to.starts_with('/')
                && to
                    .parse::<http::uri::PathAndQuery>()
                    .is_ok_and(|parsed| parsed.query().is_none() && parsed.path() == to);
            let Some(to) = valid
                .then(|| canonicalize_http_path(to))
                .flatten()
                .map(std::borrow::Cow::into_owned)
            else {
                issues.push(issue(
                    &proxy.origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "proxy_pass URI replacement must be a canonical absolute path",
                ));
                return None;
            };
            Some(HttpProxyPathRewrite {
                from: from.into(),
                to,
            })
        } else {
            None
        };
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
        let mut response_headers =
            self.lower_response_headers(location, &mut response_header_origins, issues);
        if let Some(server) = self.default_error_server.as_deref() {
            response_headers.extend([
                HttpResponseHeaderMutation::Set {
                    name: "server".into(),
                    value: server.into(),
                    always: true,
                },
                HttpResponseHeaderMutation::Set {
                    name: "content-type".into(),
                    value: "text/html".into(),
                    always: true,
                },
            ]);
            self.used_default_error_overlay.set(true);
        }
        let response_cookie_path_rewrites = self.lower_cookie_rewrites(location, issues);
        let response_cookie_attributes = self.lower_cookie_attributes(location, issues);
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
            b"proxy_cookie_flags".as_slice(),
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
                upstream_path_rewrite,
                request_headers,
                response_headers,
                response_cookie_path_rewrites,
                response_cookie_attributes,
                retry,
                cache: None,
            },
            timeouts,
            origins,
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "endpoint, weighted policy, pool construction, and provenance stay atomic"
    )]
    fn proxy_pool(
        http: &EffectiveHttp,
        proxy: &crate::nginx::EffectiveProxyPass,
        service_name: &str,
        http_index: usize,
        route_ordinal: usize,
        upstream_tls: Option<UpstreamTls>,
        issues: &mut Vec<LowerIssue>,
    ) -> Option<PoolCandidate> {
        let (endpoints, origin, endpoint_origins, weight_origins, algorithm) =
            match proxy.upstream {
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
                    None,
                    UpstreamAlgorithm::RoundRobin,
                ),
                crate::nginx::UpstreamReference::Resolved(occurrence) => {
                    let upstream = http
                        .upstreams
                        .iter()
                        .find(|upstream| upstream.origin.occurrence == occurrence)
                        .expect("resolved upstream occurrence is retained");
                    let has_weights = upstream
                        .servers
                        .iter()
                        .any(|server| server.weight.is_some());
                    let mut endpoints = Vec::new();
                    let mut endpoint_origins = Vec::new();
                    let mut weight_origins = has_weights.then(Vec::new);
                    for server in &upstream.servers {
                        let Some(endpoint) = server.endpoint.as_ref() else {
                            continue;
                        };
                        endpoints.push(canonical_endpoint(endpoint));
                        endpoint_origins.push(server.origin.clone());
                        if let Some(origins) = &mut weight_origins {
                            origins.push(server.weight.as_ref().map_or_else(
                                || server.origin.clone(),
                                |weight| weight.origin.clone(),
                            ));
                        }
                    }
                    (
                        endpoints,
                        upstream.origin.clone(),
                        endpoint_origins,
                        weight_origins,
                        if has_weights {
                            UpstreamAlgorithm::WeightedRoundRobin {
                                weights: upstream
                                    .servers
                                    .iter()
                                    .filter_map(|server| {
                                        server.endpoint.as_ref().map(|_| {
                                            server.weight.as_ref().map_or(1, |weight| weight.value)
                                        })
                                    })
                                    .collect(),
                            }
                        } else {
                            UpstreamAlgorithm::RoundRobin
                        },
                    )
                }
                crate::nginx::UpstreamReference::Unresolved
                | crate::nginx::UpstreamReference::Variable => (
                    Vec::new(),
                    proxy.origin.clone(),
                    Vec::new(),
                    None,
                    UpstreamAlgorithm::RoundRobin,
                ),
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
                algorithm,
                health_check: None,
                passive_health: None,
                tls: upstream_tls,
                http_versions: HttpVersionPolicy::default(),
                queue_timeout_ms: None,
                connect_timeout_ms: None,
                server_timeout_ms: None,
                connection_reuse: UpstreamConnectionReuse::Never,
            },
            origin,
            endpoint_origins,
            weight_origins,
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
                host = Self::proxy_value(value, proxy, nginx_host_fallback)
                    .and_then(NginxProxyValue::into_upstream_host);
                if host.is_none() {
                    issues.push(issue(
                        origin,
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "proxy_set_header Host value has no canonical upstream Host policy",
                    ));
                }
                continue;
            }
            let mutation = if value.is_empty() {
                Some(HttpRequestHeaderMutation::Remove { name: name.into() })
            } else {
                Self::proxy_value(value, proxy, nginx_host_fallback).map(|value| {
                    HttpRequestHeaderMutation::Set {
                        name: name.into(),
                        value: value.into_request_header_value(),
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
        let default_host = Self::proxy_value(b"$proxy_host", proxy, nginx_host_fallback)
            .and_then(NginxProxyValue::into_upstream_host)
            .expect("validated proxy authority is a canonical Host value");
        (host.unwrap_or(default_host), headers)
    }

    fn proxy_value(
        value: &[u8],
        proxy: &crate::nginx::EffectiveProxyPass,
        nginx_host_fallback: Option<&str>,
    ) -> Option<NginxProxyValue> {
        match value {
            b"$http_host" => Some(NginxProxyValue::IncomingAuthority),
            b"$host" => nginx_host_fallback
                .map(str::to_owned)
                .map(NginxProxyValue::NginxHost),
            b"$remote_addr" => Some(NginxProxyValue::ClientIp),
            b"$proxy_add_x_forwarded_for" => Some(NginxProxyValue::AppendedXForwardedFor),
            b"$scheme" => Some(NginxProxyValue::DownstreamScheme),
            b"$http_upgrade" => Some(NginxProxyValue::IncomingUpgrade),
            b"$proxy_host" => utf8(&proxy.authority)
                .map(str::to_owned)
                .map(NginxProxyValue::Literal),
            value if !value.contains(&b'$') => {
                utf8(value).map(str::to_owned).map(NginxProxyValue::Literal)
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
        if self.default_error_server.is_some() {
            hidden.retain(|name| !name.eq_ignore_ascii_case("server"));
        }
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
            if self.x_accel_controls_absent {
                return;
            }
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

    fn lower_cookie_attributes(
        &self,
        location: &EffectiveLocation,
        issues: &mut Vec<LowerIssue>,
    ) -> Vec<HttpCookieAttributePolicy> {
        let mut attributes = Vec::new();
        let mut names = HashSet::new();
        for policy in
            self.effective_list_policy_chain(location.origin.occurrence, b"proxy_cookie_flags")
        {
            let origin = policy.origins.last().unwrap_or(&location.origin);
            let Some((name, flags)) = policy.arguments.split_first() else {
                continue;
            };
            let Some(name) = utf8(name) else {
                issues.push(issue(
                    origin,
                    E_INVALID_VALUE,
                    "proxy_cookie_flags cookie name is not UTF-8",
                ));
                continue;
            };
            if name.is_empty() || name.starts_with('~') || name.contains('$') {
                issues.push(issue(
                    origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "proxy_cookie_flags requires an exact static cookie name",
                ));
                continue;
            }

            let mut secure = None;
            let mut http_only = None;
            let mut same_site = None;
            let mut valid = true;
            for flag in flags {
                match flag.as_slice() {
                    b"secure" if secure.is_none() => secure = Some(true),
                    b"nosecure" if secure.is_none() => secure = Some(false),
                    b"httponly" if http_only.is_none() => http_only = Some(true),
                    b"nohttponly" if http_only.is_none() => http_only = Some(false),
                    b"samesite=strict" if same_site.is_none() => {
                        same_site = Some(HttpSameSite::Strict);
                    }
                    b"samesite=lax" if same_site.is_none() => {
                        same_site = Some(HttpSameSite::Lax);
                    }
                    b"samesite=none" if same_site.is_none() => {
                        same_site = Some(HttpSameSite::None);
                    }
                    b"secure" | b"nosecure" | b"httponly" | b"nohttponly" | b"samesite=strict"
                    | b"samesite=lax" | b"samesite=none" => {
                        valid = false;
                        issues.push(issue(
                            origin,
                            E_SEMANTICS_NOT_REPRESENTABLE,
                            "proxy_cookie_flags repeats or conflicts on one cookie attribute",
                        ));
                    }
                    _ => {
                        valid = false;
                        issues.push(issue(
                            origin,
                            E_SEMANTICS_NOT_REPRESENTABLE,
                            "proxy_cookie_flags flag is outside the canonical secure, HttpOnly, and SameSite subset",
                        ));
                    }
                }
            }
            if flags.is_empty() {
                valid = false;
                issues.push(issue(
                    origin,
                    E_INVALID_VALUE,
                    "proxy_cookie_flags requires at least one cookie attribute flag",
                ));
            }
            if !valid {
                continue;
            }
            if !names.insert(name.to_owned()) {
                issues.push(issue(
                    origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "proxy_cookie_flags defines more than one policy for a cookie name",
                ));
                continue;
            }
            attributes.push(HttpCookieAttributePolicy {
                name: name.to_owned(),
                secure,
                http_only,
                same_site,
            });
        }
        attributes
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
            HttpRetryTrigger::EmptyResponse => 3,
            HttpRetryTrigger::ResponseTimeout => 4,
            HttpRetryTrigger::JunkResponse => 5,
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
            if let Err(message) = validate_literal_response_header(name, value) {
                issues.push(issue(
                    policy.origins.last().unwrap_or(&location.origin),
                    E_INVALID_VALUE,
                    message,
                ));
                continue;
            }
            if headers.len() >= NGINX_MAX_LITERAL_HEADERS {
                issues.push(issue(
                    policy.origins.last().unwrap_or(&location.origin),
                    E_INVALID_VALUE,
                    "nginx add_header declarations exceed the canonical response-header bound",
                ));
                continue;
            }
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
        let mut seen_statuses = HashSet::new();
        for policy in self.effective_list_policy_chain(location.origin.occurrence, b"error_page") {
            origins.extend(policy.origins.clone());
            let origin = policy.origins.last().unwrap_or(&location.origin);
            let Some(file) = policy.arguments.last().and_then(|value| utf8(value)) else {
                issues.push(issue(
                    origin,
                    E_INVALID_VALUE,
                    "nginx error_page target must be UTF-8",
                ));
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
            let statuses_valid = statuses.as_ref().is_some_and(|statuses| {
                let mut local = HashSet::new();
                !statuses.is_empty()
                    && statuses.len() <= NGINX_MAX_STATIC_ERROR_STATUSES
                    && statuses.iter().all(|status| local.insert(*status))
            });
            let canonical_file = canonicalize_http_path(file)
                .filter(|canonical| canonical.as_ref() == file)
                .filter(|_| file.starts_with('/'))
                .filter(|_| file.len() <= NGINX_MAX_STATIC_ERROR_TARGET_BYTES);
            let (Some(statuses), Some(canonical_file)) = (statuses, canonical_file) else {
                issues.push(issue(
                    origin,
                    E_INVALID_VALUE,
                    "nginx error_page requires a bounded 400..=599 status list and an absolute canonical URI target",
                ));
                continue;
            };
            if !statuses_valid || statuses.iter().any(|status| seen_statuses.contains(status)) {
                issues.push(issue(
                    origin,
                    E_INVALID_VALUE,
                    "nginx error_page statuses must be unique bounded 400..=599 values",
                ));
                continue;
            }
            if responses.len() >= NGINX_MAX_STATIC_ERROR_RESPONSES {
                issues.push(issue(
                    origin,
                    E_INVALID_VALUE,
                    "nginx error_page declarations exceed the canonical response bound",
                ));
                continue;
            }
            seen_statuses.extend(statuses.iter().copied());
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

fn validate_literal_response_header(name: &str, value: &str) -> Result<(), &'static str> {
    if name.is_empty() || name.len() > NGINX_MAX_HEADER_NAME_BYTES {
        return Err("nginx response header name must be 1..=64 bytes");
    }
    HeaderName::from_bytes(name.as_bytes()).map_err(|_| "nginx response header name is invalid")?;
    if matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "content-length"
            | "host"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "proxy-connection"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    ) {
        return Err("nginx response header is hop-by-hop, framing, or request-managed");
    }
    if value.len() > NGINX_MAX_HEADER_VALUE_BYTES {
        return Err("nginx response header value exceeds 8192 bytes");
    }
    HeaderValue::from_str(value)
        .map_err(|_| "nginx response header value contains invalid bytes")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_nginx_time_uses_seconds() {
        assert_eq!(parse_duration_ms(b"600"), Some(600_000));
        assert_eq!(parse_duration_ms(b"600ms"), Some(600));
        assert_eq!(parse_duration_ms(b"10m"), Some(600_000));
        assert_eq!(parse_duration_ms(b"1h30m"), Some(5_400_000));
        assert_eq!(parse_duration_ms(b"1m500ms"), Some(60_500));
        assert_eq!(parse_duration_ms(b"1h30"), Some(3_630_000));
        assert_eq!(parse_duration_ms(b"1h 30"), Some(3_630_000));
        assert_eq!(parse_duration_ms(b"1y"), None);
        assert_eq!(parse_duration_ms(b"1M"), None);
        assert_eq!(parse_duration_ms(b"1m1h"), None);
    }
}

fn nginx_default_404(server: &str) -> HttpStaticErrorResponse {
    HttpStaticErrorResponse {
        statuses: vec![404],
        file: None,
        body: Some(nginx_error_body(404, "Not Found", server)),
        headers: nginx_default_headers(server),
        internal_redirect: None,
    }
}

fn nginx_error_body(status: u16, reason: &str, server: &str) -> String {
    format!(
        "<html>\r\n<head><title>{status} {reason}</title></head>\r\n<body>\r\n<center><h1>{status} {reason}</h1></center>\r\n<hr><center>{server}</center>\r\n</body>\r\n</html>\r\n"
    )
}

fn nginx_default_headers(server: &str) -> Vec<HttpLiteralHeader> {
    vec![
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
    ]
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
