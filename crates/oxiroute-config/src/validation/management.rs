#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn validate_management(management: Option<&Management>) -> Result<(), ConfigError> {
    let Some(management) = management else {
        return Ok(());
    };

    if !management.bind.ip().is_loopback() {
        return Err(ConfigError::ManagementMustUseLoopback(management.bind));
    }
    if management.bind.port() == 0 {
        return Err(ConfigError::ZeroPort {
            kind: "management listener",
            name: "management".into(),
            field: "bind",
        });
    }

    Ok(())
}

pub(super) fn validate_stats(stats: Option<&Stats>) -> Result<(), ConfigError> {
    let Some(stats) = stats else {
        return Ok(());
    };
    if stats.binds.len() + stats.pages.len() == 0 || stats.binds.len() + stats.pages.len() > 8 {
        return Err(ConfigError::InvalidStatsBinds);
    }
    for bind in &stats.binds {
        if bind.port() == 0 {
            return Err(ConfigError::ZeroPort {
                kind: "statistics listener",
                name: "stats".into(),
                field: "binds",
            });
        }
    }
    if let Some(path) = &stats.admin_token_file {
        validate_file_path("statistics", "stats", "admin_token_file", path)?;
    }
    for (index, page) in stats.pages.iter().enumerate() {
        validate_stats_page(index, page)?;
    }
    Ok(())
}

fn validate_stats_page(index: usize, page: &StatsPage) -> Result<(), ConfigError> {
    let page_name = format!("page-{index}");
    if page.bind.port() == 0 {
        return Err(ConfigError::ZeroPort {
            kind: "statistics page",
            name: page_name.clone(),
            field: "bind",
        });
    }
    if page.uri_prefix.len() > crate::defaults::MAX_STATS_PAGE_URI_BYTES {
        return Err(ConfigError::InvalidStatsPage {
            page: index,
            field: "uri_prefix",
            detail: "URI prefix exceeds 2048 bytes",
        });
    }
    let path_and_query = page.uri_prefix.parse::<PathAndQuery>();
    if !page.uri_prefix.is_ascii()
        || page.uri_prefix.bytes().any(|byte| {
            byte <= b' '
                || byte == 0x7f
                || matches!(
                    byte,
                    b'?' | b'#'
                        | b'<'
                        | b'>'
                        | b'['
                        | b']'
                        | b'\\'
                        | b'^'
                        | b'`'
                        | b'{'
                        | b'|'
                        | b'}'
                )
        })
        || !path_and_query.is_ok_and(|path| {
            path.query().is_none()
                && path.path() == page.uri_prefix
                && is_unambiguous_http_path(path.path())
        })
    {
        return Err(ConfigError::InvalidStatsPage {
            page: index,
            field: "uri_prefix",
            detail: "URI prefix must be an absolute unambiguous HTTP path without a query or fragment",
        });
    }
    if page.refresh_ms == 0 || page.refresh_ms > crate::defaults::MAX_STATS_PAGE_REFRESH_MS {
        return Err(ConfigError::InvalidStatsPage {
            page: index,
            field: "refresh_ms",
            detail: "refresh must be between 1 and 86400000 milliseconds",
        });
    }
    if page.max_connections == Some(0) {
        return Err(ConfigError::ZeroLimit {
            kind: "statistics page",
            name: page_name.clone(),
            field: "max_connections",
        });
    }
    if let Some(max_connections) = page.max_connections {
        validate_safe_integer(
            "statistics page",
            &page_name,
            "max_connections",
            max_connections,
        )?;
    }
    for (field, timeout) in [
        (
            "downstream_timeouts.client_timeout_ms",
            page.downstream_timeouts.client_timeout_ms,
        ),
        (
            "downstream_timeouts.request_timeout_ms",
            page.downstream_timeouts.request_timeout_ms,
        ),
        (
            "downstream_timeouts.keepalive_timeout_ms",
            page.downstream_timeouts.keepalive_timeout_ms,
        ),
    ] {
        if timeout.is_some_and(|value| value == 0 || value > MAX_HTTP_TIMEOUT_MS) {
            return Err(ConfigError::InvalidStatsPage {
                page: index,
                field,
                detail: "downstream timeouts must be between 1 and 86400000 milliseconds",
            });
        }
        if let Some(timeout) = timeout {
            validate_safe_integer("statistics page", &page_name, field, timeout)?;
        }
    }
    Ok(())
}

pub(super) fn validate_bind_conflicts(
    management: Option<&Management>,
    stats: Option<&Stats>,
    listeners: &[Listener],
) -> Result<(), ConfigError> {
    let mut binds = Vec::with_capacity(
        listeners.len()
            + usize::from(management.is_some())
            + stats.map_or(0, |stats| stats.binds.len() + stats.pages.len()),
    );
    if let Some(management) = management {
        binds.push((
            "management".to_owned(),
            ListenerBind::Socket {
                address: management.bind,
            },
        ));
    }
    if let Some(stats) = stats {
        for (index, address) in stats.binds.iter().enumerate() {
            let bind = ListenerBind::Socket { address: *address };
            for (first_name, first_bind) in &binds {
                if binds_overlap(first_bind, &bind) {
                    return Err(ConfigError::OverlappingBind {
                        first_name: first_name.clone(),
                        first_bind: Box::new(first_bind.clone()),
                        second_name: format!("stats-{index}"),
                        second_bind: Box::new(bind),
                    });
                }
            }
            binds.push((format!("stats-{index}"), bind));
        }
        for (index, page) in stats.pages.iter().enumerate() {
            let bind = ListenerBind::Socket { address: page.bind };
            for (first_name, first_bind) in &binds {
                if binds_overlap(first_bind, &bind) {
                    return Err(ConfigError::OverlappingBind {
                        first_name: first_name.clone(),
                        first_bind: Box::new(first_bind.clone()),
                        second_name: format!("stats-page-{index}"),
                        second_bind: Box::new(bind),
                    });
                }
            }
            binds.push((format!("stats-page-{index}"), bind));
        }
    }

    for listener in listeners {
        for (first_name, first_bind) in &binds {
            if binds_overlap(first_bind, &listener.bind) {
                return Err(ConfigError::OverlappingBind {
                    first_name: first_name.clone(),
                    first_bind: Box::new(first_bind.clone()),
                    second_name: listener.name.clone(),
                    second_bind: Box::new(listener.bind.clone()),
                });
            }
        }
        binds.push((listener.name.clone(), listener.bind.clone()));
    }

    Ok(())
}

fn binds_overlap(first: &ListenerBind, second: &ListenerBind) -> bool {
    match (first, second) {
        (
            ListenerBind::Socket {
                address: first_address,
            }
            | ListenerBind::Udp {
                address: first_address,
            },
            ListenerBind::Socket {
                address: second_address,
            }
            | ListenerBind::Udp {
                address: second_address,
            },
        ) if matches!(
            (first, second),
            (ListenerBind::Socket { .. }, ListenerBind::Socket { .. })
                | (ListenerBind::Udp { .. }, ListenerBind::Udp { .. })
        ) =>
        {
            let first_ip = canonical_ip(first_address.ip());
            let second_ip = canonical_ip(second_address.ip());
            first_address.port() == second_address.port()
                && (first_ip == second_ip
                    || first_ip.is_unspecified()
                    || second_ip.is_unspecified())
        }
        (ListenerBind::Unix { path: first, .. }, ListenerBind::Unix { path: second, .. }) => {
            first == second
        }
        _ => false,
    }
}

pub(super) fn endpoint_exposes_management(endpoint: SocketAddr, management: SocketAddr) -> bool {
    let endpoint_ip = canonical_ip(endpoint.ip());
    endpoint.port() == management.port()
        && (endpoint_ip == canonical_ip(management.ip()) || endpoint_ip.is_unspecified())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn validate_listeners(
    listeners: &[Listener],
    http_service_names: &HashSet<String>,
    http_services: &HashMap<&str, &HttpService>,
    rtmp_service_names: &HashSet<String>,
    l4_service_names: &HashSet<String>,
    tls_profile_names: &HashSet<String>,
    forward_proxy_services: &HashMap<&str, &ForwardProxyService>,
    tls_profiles: &HashMap<&str, &TlsProfile>,
) -> Result<(), ConfigError> {
    for listener in listeners {
        validate_listener_basics(listener, tls_profile_names)?;
        match (listener.protocol, listener.service.as_deref()) {
            (
                Protocol::Http
                | Protocol::Rtmp
                | Protocol::Tcp
                | Protocol::Udp
                | Protocol::ForwardHttp1
                | Protocol::ForwardHttp2
                | Protocol::ForwardHttp3
                | Protocol::Http3,
                None,
            ) => {
                return Err(ConfigError::MissingListenerService {
                    listener: listener.name.clone(),
                    protocol: listener.protocol,
                });
            }
            (Protocol::Http | Protocol::Http3, Some(service))
                if !http_service_names.contains(service) =>
            {
                return Err(ConfigError::UnknownListenerService {
                    listener: listener.name.clone(),
                    protocol: listener.protocol,
                    service: service.into(),
                });
            }
            (Protocol::Http3, Some(service)) => {
                validate_http3_listener(
                    listener,
                    http_services
                        .get(service)
                        .expect("validated HTTP/3 service reference"),
                    tls_profiles,
                )?;
            }
            (Protocol::Tcp | Protocol::Udp, Some(service))
                if !l4_service_names.contains(service) =>
            {
                return Err(ConfigError::UnknownListenerService {
                    listener: listener.name.clone(),
                    protocol: listener.protocol,
                    service: service.into(),
                });
            }
            (Protocol::Rtmp, Some(service)) if !rtmp_service_names.contains(service) => {
                return Err(ConfigError::UnknownListenerService {
                    listener: listener.name.clone(),
                    protocol: listener.protocol,
                    service: service.into(),
                });
            }
            (
                Protocol::ForwardHttp1 | Protocol::ForwardHttp2 | Protocol::ForwardHttp3,
                Some(service),
            ) => {
                let forward_service = forward_proxy_services.get(service).ok_or_else(|| {
                    ConfigError::UnknownListenerService {
                        listener: listener.name.clone(),
                        protocol: listener.protocol,
                        service: service.into(),
                    }
                })?;
                validate_forward_listener(listener, forward_service, tls_profiles)?;
            }
            _ => {}
        }
    }

    Ok(())
}

fn validate_listener_basics(
    listener: &Listener,
    tls_profile_names: &HashSet<String>,
) -> Result<(), ConfigError> {
    if matches!(
        &listener.bind,
        ListenerBind::Socket { address } | ListenerBind::Udp { address }
            if address.port() == 0
    ) {
        return Err(ConfigError::ZeroPort {
            kind: "listener",
            name: listener.name.clone(),
            field: "bind",
        });
    }
    validate_listener_policies(listener)?;
    if let (ListenerBind::Unix { .. }, Some(profile)) =
        (&listener.bind, listener.tls_profile.as_deref())
    {
        return Err(ConfigError::UnsupportedUnixListenerTls {
            listener: listener.name.clone(),
            profile: profile.into(),
        });
    }

    let datagram = matches!(listener.bind, ListenerBind::Udp { .. });
    let datagram_protocol = matches!(
        listener.protocol,
        Protocol::ForwardHttp3 | Protocol::Http3 | Protocol::Udp
    );
    if datagram != datagram_protocol {
        return Err(ConfigError::InvalidListenerTransport {
            listener: listener.name.clone(),
            protocol: listener.protocol,
            detail: if datagram_protocol {
                "this protocol requires a UDP bind"
            } else {
                "UDP binds require http3, forward_http3, or udp"
            },
        });
    }

    match (listener.protocol, listener.tls_profile.as_deref()) {
        (Protocol::Http, Some(profile)) if !tls_profile_names.contains(profile) => {
            Err(ConfigError::UnknownListenerTlsProfile {
                listener: listener.name.clone(),
                profile: profile.into(),
            })
        }
        (
            Protocol::Http
            | Protocol::Http3
            | Protocol::ForwardHttp1
            | Protocol::ForwardHttp2
            | Protocol::ForwardHttp3,
            _,
        )
        | (_, None) => Ok(()),
        (protocol, Some(profile)) => Err(ConfigError::UnexpectedListenerTlsProfile {
            listener: listener.name.clone(),
            protocol,
            profile: profile.into(),
        }),
    }
}

fn validate_listener_policies(listener: &Listener) -> Result<(), ConfigError> {
    if listener.max_connections == Some(0) {
        return Err(ConfigError::ZeroLimit {
            kind: "listener",
            name: listener.name.clone(),
            field: "max_connections",
        });
    }
    if let Some(max_connections) = listener.max_connections {
        validate_safe_integer(
            "listener",
            &listener.name,
            "max_connections",
            max_connections,
        )?;
    }
    if let ListenerBind::Unix {
        mode: Some(mode), ..
    } = listener.bind
        && (mode == 0 || mode > 0o777)
    {
        return Err(ConfigError::InvalidListenerUnixMode {
            listener: listener.name.clone(),
            mode,
        });
    }
    for (field, timeout) in [
        (
            "downstream_timeouts.client_timeout_ms",
            listener.downstream_timeouts.client_timeout_ms,
        ),
        (
            "downstream_timeouts.request_timeout_ms",
            listener.downstream_timeouts.request_timeout_ms,
        ),
        (
            "downstream_timeouts.keepalive_timeout_ms",
            listener.downstream_timeouts.keepalive_timeout_ms,
        ),
    ] {
        if timeout.is_some_and(|value| value == 0 || value > MAX_HTTP_TIMEOUT_MS) {
            return Err(ConfigError::InvalidListenerTransport {
                listener: listener.name.clone(),
                protocol: listener.protocol,
                detail: "downstream timeouts must be between 1 and 86400000 milliseconds",
            });
        }
        if let Some(timeout) = timeout {
            validate_safe_integer("listener", &listener.name, field, timeout)?;
        }
    }
    if let Some(policy) = listener.proxy_protocol {
        validate_proxy_protocol_timeout("listener", &listener.name, policy.timeout_ms)?;
        if !matches!(listener.protocol, Protocol::Tcp | Protocol::Udp) {
            return Err(ConfigError::InvalidProxyProtocolPolicy {
                kind: "listener",
                name: listener.name.clone(),
                field: "proxy_protocol",
                detail: "PROXY protocol is supported only by TCP and UDP listeners",
            });
        }
        if listener.protocol == Protocol::Udp && matches!(policy.version, ProxyProtocolVersion::V1)
        {
            return Err(ConfigError::InvalidProxyProtocolPolicy {
                kind: "listener",
                name: listener.name.clone(),
                field: "proxy_protocol.version",
                detail: "UDP listeners require v2 or auto PROXY protocol",
            });
        }
    }
    if !matches!(
        listener.protocol,
        Protocol::Http
            | Protocol::Http3
            | Protocol::ForwardHttp1
            | Protocol::ForwardHttp2
            | Protocol::ForwardHttp3
    ) && (listener.downstream_timeouts.request_timeout_ms.is_some()
        || listener.downstream_timeouts.keepalive_timeout_ms.is_some())
    {
        return Err(ConfigError::InvalidListenerTransport {
            listener: listener.name.clone(),
            protocol: listener.protocol,
            detail: "request and keepalive timeouts apply only to HTTP listeners",
        });
    }
    Ok(())
}

fn validate_http3_listener(
    listener: &Listener,
    service: &HttpService,
    tls_profiles: &HashMap<&str, &TlsProfile>,
) -> Result<(), ConfigError> {
    let invalid = |detail: &'static str| ConfigError::InvalidListenerTransport {
        listener: listener.name.clone(),
        protocol: Protocol::Http3,
        detail,
    };
    let Some(profile_name) = listener.tls_profile.as_deref() else {
        return Err(invalid(
            "HTTP/3 requires a TLS 1.3 profile advertising only h3",
        ));
    };
    let profile =
        tls_profiles
            .get(profile_name)
            .ok_or_else(|| ConfigError::UnknownListenerTlsProfile {
                listener: listener.name.clone(),
                profile: profile_name.into(),
            })?;
    if profile.min_version != TlsVersion::Tls13 || profile.alpn.as_slice() != [AlpnProtocol::H3] {
        return Err(invalid(
            "HTTP/3 requires a TLS 1.3 profile advertising only h3",
        ));
    }
    if service.max_request_body_bytes.is_none() {
        return Err(invalid("HTTP/3 requires a bounded service request body"));
    }
    if service
        .max_request_body_bytes
        .is_some_and(|value| value > MAX_HTTP3_REQUEST_BODY_BYTES)
    {
        return Err(invalid(
            "HTTP/3 service request body exceeds the 64 MiB limit",
        ));
    }
    if service.gzip.is_some() {
        return Err(invalid(
            "HTTP/3 reverse response compression is not supported",
        ));
    }
    for route in &service.routes {
        if route.policy.max_request_body_bytes.is_none() {
            return Err(invalid("HTTP/3 requires a bounded route request body"));
        }
        if route
            .policy
            .max_request_body_bytes
            .is_some_and(|value| value > MAX_HTTP3_REQUEST_BODY_BYTES)
        {
            return Err(invalid(
                "HTTP/3 route request body exceeds the 64 MiB limit",
            ));
        }
        if !route.policy.request_buffering {
            return Err(invalid("HTTP/3 requires request buffering"));
        }
        if route.policy.response_buffering {
            return Err(invalid("HTTP/3 response buffering is not supported"));
        }
        match &route.action {
            HttpRouteAction::Proxy { policy, .. } => {
                if policy.cache.is_some() {
                    return Err(invalid("HTTP/3 reverse cache policy is not supported"));
                }
                if policy.request_headers.iter().any(|mutation| {
                    matches!(
                        mutation,
                        HttpRequestHeaderMutation::Set { name, .. }
                            if is_http3_hop_by_hop_header(name)
                    )
                }) {
                    return Err(invalid(
                        "HTTP/3 reverse hop-by-hop headers are not supported",
                    ));
                }
                if policy.response_headers.iter().any(|mutation| {
                    let name = match mutation {
                        HttpResponseHeaderMutation::Set { name, .. }
                        | HttpResponseHeaderMutation::Add { name, .. }
                        | HttpResponseHeaderMutation::Remove { name } => name,
                    };
                    is_http3_hop_by_hop_header(name)
                }) {
                    return Err(invalid(
                        "HTTP/3 reverse hop-by-hop headers are not supported",
                    ));
                }
            }
            HttpRouteAction::FixedResponse { .. }
            | HttpRouteAction::Redirect { .. }
            | HttpRouteAction::StaticFiles { .. } => {}
        }
    }
    Ok(())
}

pub(super) fn validate_h3_upstream_usage(
    listeners: &[Listener],
    services: &[HttpService],
    upstream_pools: &[UpstreamPool],
) -> Result<(), ConfigError> {
    let pools = upstream_pools
        .iter()
        .map(|pool| (pool.name.as_str(), pool))
        .collect::<HashMap<_, _>>();
    for service in services {
        let service_listeners = listeners
            .iter()
            .filter(|listener| listener.service.as_deref() == Some(service.name.as_str()))
            .collect::<Vec<_>>();
        let uses_h3_pool = service.routes.iter().any(|route| {
            let HttpRouteAction::Proxy { upstream_pool, .. } = &route.action else {
                return false;
            };
            pools
                .get(upstream_pool.as_str())
                .is_some_and(|pool| pool.http_versions.min == HttpVersion::Http3)
        });
        let Some(http3_listener) = listeners.iter().find(|listener| {
            listener.service.as_deref() == Some(service.name.as_str())
                && listener.protocol == Protocol::Http3
        }) else {
            if uses_h3_pool && let Some(listener) = service_listeners.first() {
                return Err(ConfigError::InvalidListenerTransport {
                    listener: listener.name.clone(),
                    protocol: listener.protocol,
                    detail: "an HTTP/3 upstream pool requires an http3 listener",
                });
            }
            continue;
        };
        let mut uses_h3_pool = false;
        for route in &service.routes {
            let HttpRouteAction::Proxy { upstream_pool, .. } = &route.action else {
                continue;
            };
            let Some(pool) = pools.get(upstream_pool.as_str()) else {
                return Err(ConfigError::InvalidListenerTransport {
                    listener: http3_listener.name.clone(),
                    protocol: Protocol::Http3,
                    detail: "HTTP/3 reverse routes require a resolvable exact HTTP/3 upstream pool",
                });
            };
            if pool.http_versions.min != HttpVersion::Http3
                || pool.http_versions.max != HttpVersion::Http3
            {
                return Err(ConfigError::InvalidListenerTransport {
                    listener: http3_listener.name.clone(),
                    protocol: Protocol::Http3,
                    detail: "HTTP/3 reverse routes require an exact HTTP/3 upstream pool",
                });
            }
            uses_h3_pool = true;
        }
        if uses_h3_pool
            && service_listeners
                .iter()
                .any(|listener| listener.protocol != Protocol::Http3)
        {
            return Err(ConfigError::InvalidListenerTransport {
                listener: http3_listener.name.clone(),
                protocol: Protocol::Http3,
                detail: "a service using an HTTP/3 upstream pool cannot be shared with a non-HTTP/3 listener",
            });
        }
    }
    Ok(())
}

fn is_http3_hop_by_hop_header(name: &str) -> bool {
    [
        "connection",
        "keep-alive",
        "proxy-connection",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ]
    .iter()
    .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

fn validate_forward_listener(
    listener: &Listener,
    service: &ForwardProxyService,
    tls_profiles: &HashMap<&str, &TlsProfile>,
) -> Result<(), ConfigError> {
    let version = match listener.protocol {
        Protocol::ForwardHttp1 => ForwardHttpVersion::H1,
        Protocol::ForwardHttp2 => ForwardHttpVersion::H2,
        Protocol::ForwardHttp3 => ForwardHttpVersion::H3,
        Protocol::Http | Protocol::Http3 | Protocol::Rtmp | Protocol::Tcp | Protocol::Udp => {
            return Ok(());
        }
    };
    let invalid = |detail: String| ConfigError::InvalidForwardProxyListener {
        listener: listener.name.clone(),
        detail,
    };

    if listener.downstream_timeouts.client_timeout_ms.is_some()
        || listener.downstream_timeouts.keepalive_timeout_ms.is_some()
    {
        return Err(invalid(
            "forward HTTP/1 currently accepts only the request-header downstream timeout".into(),
        ));
    }

    if !service.enabled_versions.contains(&version) {
        return Err(invalid(format!(
            "referenced service `{}` does not enable {version:?}",
            service.name
        )));
    }
    if version == ForwardHttpVersion::H3 && service.header_policy.cache.is_some() {
        return Err(ConfigError::InvalidForwardProxyService {
            service: service.name.clone(),
            field: "cache",
            detail: format!(
                "is not supported by forward HTTP/3 listener `{}`",
                listener.name
            ),
        });
    }
    if version == ForwardHttpVersion::H3
        && service
            .max_request_body_bytes
            .is_some_and(|value| value > MAX_HTTP3_REQUEST_BODY_BYTES)
    {
        return Err(invalid(
            "forward HTTP/3 request body exceeds the 64 MiB limit".into(),
        ));
    }
    if matches!(listener.bind, ListenerBind::Unix { .. }) && service.tls_required {
        return Err(invalid(
            "a Unix listener requires its service to set tls_required = false".into(),
        ));
    }
    let tls_required = version == ForwardHttpVersion::H3
        || (service.tls_required && !matches!(listener.bind, ListenerBind::Unix { .. }));
    if tls_required && listener.tls_profile.is_none() {
        return Err(invalid("this listener requires a TLS profile".into()));
    }
    let Some(profile_name) = listener.tls_profile.as_deref() else {
        return Ok(());
    };
    let profile =
        tls_profiles
            .get(profile_name)
            .ok_or_else(|| ConfigError::UnknownListenerTlsProfile {
                listener: listener.name.clone(),
                profile: profile_name.into(),
            })?;
    let required_alpn = match version {
        ForwardHttpVersion::H1 => AlpnProtocol::Http11,
        ForwardHttpVersion::H2 => AlpnProtocol::H2,
        ForwardHttpVersion::H3 => AlpnProtocol::H3,
    };
    if !profile.alpn.contains(&required_alpn) {
        return Err(invalid(format!(
            "TLS profile `{profile_name}` does not advertise the required ALPN"
        )));
    }
    if version == ForwardHttpVersion::H3
        && (profile.min_version != TlsVersion::Tls13
            || profile.alpn.as_slice() != [AlpnProtocol::H3])
    {
        return Err(invalid(
            "forward_http3 requires a TLS 1.3 profile advertising only h3".into(),
        ));
    }
    Ok(())
}
