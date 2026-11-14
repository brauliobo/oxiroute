#[allow(clippy::wildcard_imports)]
use super::*;

pub(in super::super) fn certbot_host_condition(directive: &Directive) -> Option<&[u8]> {
    let [variable, operator, host] = directive.arguments.as_slice() else {
        return None;
    };
    let variable = variable.value.strip_prefix(b"(").unwrap_or(&variable.value);
    let host = host.value.strip_suffix(b")").unwrap_or(&host.value);
    (variable == b"$host" && operator.value == b"=" && !host.is_empty() && !has_variable(host))
        .then_some(host)
}

pub(super) fn parse_listen_endpoint(value: &[u8]) -> Option<ListenEndpoint> {
    if let Some(path) = value.strip_prefix(b"unix:") {
        return unix_socket_path(path).map(|path| ListenEndpoint::Unix { path });
    }
    if value.iter().all(u8::is_ascii_digit) {
        return parse_port(value).map(|port| ListenEndpoint::Socket {
            address: b"*".to_vec(),
            port,
        });
    }
    if value.starts_with(b"[") {
        let closing = value.iter().position(|byte| *byte == b']')?;
        let port = match value.get(closing + 1..) {
            Some([]) => 80,
            Some(rest) if rest.first() == Some(&b':') => parse_port(&rest[1..])?,
            _ => return None,
        };
        return Some(ListenEndpoint::Socket {
            address: ascii_lowercase(&value[..=closing]),
            port,
        });
    }
    let Some(colon) = value.iter().rposition(|byte| *byte == b':') else {
        return Some(ListenEndpoint::Socket {
            address: match value {
                b"*" | b"0.0.0.0" => b"*".to_vec(),
                address if !address.is_empty() => ascii_lowercase(address),
                _ => return None,
            },
            port: 80,
        });
    };
    if value[..colon].contains(&b':') {
        return None;
    }
    let address = match &value[..colon] {
        b"" | b"*" | b"0.0.0.0" => b"*".to_vec(),
        address => ascii_lowercase(address),
    };
    Some(ListenEndpoint::Socket {
        address,
        port: parse_port(&value[colon + 1..])?,
    })
}

/// The IPv4 wildcard endpoint that nginx pairs with an IPv6 `[::]` wildcard on the same port.
pub(super) fn ipv4_wildcard_pair(endpoint: &ListenEndpoint) -> Option<ListenEndpoint> {
    match endpoint {
        ListenEndpoint::Socket { address, port } if address == b"[::]" => {
            Some(ListenEndpoint::Socket {
                address: b"*".to_vec(),
                port: *port,
            })
        }
        _ => None,
    }
}

/// `http2` is deliberately excluded: nginx applies it socket-wide, so mixed values are reconciled by lowering.
pub(super) fn listen_ssl(listen: &EffectiveListen) -> bool {
    listen.options.iter().any(|option| option.value == b"ssl")
}

pub(super) fn listen_endpoints_overlap(first: &ListenEndpoint, second: &ListenEndpoint) -> bool {
    let (
        ListenEndpoint::Socket {
            address: first_address,
            port: first_port,
        },
        ListenEndpoint::Socket {
            address: second_address,
            port: second_port,
        },
    ) = (first, second)
    else {
        return false;
    };
    if first == second || first_port != second_port {
        return false;
    }
    let Some(first_ip) = listen_endpoint_ip(first_address) else {
        return false;
    };
    let Some(second_ip) = listen_endpoint_ip(second_address) else {
        return false;
    };
    let first_ip = canonical_ip(first_ip);
    let second_ip = canonical_ip(second_ip);
    first_ip == second_ip
        || first_ip.is_ipv4() == second_ip.is_ipv4()
            && (first_ip.is_unspecified() || second_ip.is_unspecified())
}

fn listen_endpoint_ip(address: &[u8]) -> Option<IpAddr> {
    if address == b"*" {
        return Some(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
    }
    let address = address
        .strip_prefix(b"[")
        .and_then(|address| address.strip_suffix(b"]"))
        .unwrap_or(address);
    std::str::from_utf8(address).ok()?.parse().ok()
}

pub(super) fn include_failure(edge: &super::super::IncludeEdge) -> Option<DiagnosticCode> {
    edge.failure.or_else(|| {
        edge.candidates
            .iter()
            .find_map(|candidate| match candidate.status {
                IncludeCandidateStatus::Expanded(_) => None,
                IncludeCandidateStatus::Cycle(_) => Some(E_INCLUDE_CYCLE),
                IncludeCandidateStatus::ExpansionLimit(_)
                | IncludeCandidateStatus::SourceSizeLimit
                | IncludeCandidateStatus::SourceFileLimit
                | IncludeCandidateStatus::AggregateSourceLimit => Some(E_SOURCE_LIMIT),
                IncludeCandidateStatus::CanonicalizeFailed
                | IncludeCandidateStatus::SourceChanged => Some(E_SOURCE_CHANGED),
                IncludeCandidateStatus::SourceIo => Some(E_SOURCE_IO),
            })
    })
}

pub(super) fn parse_static_endpoint(value: &[u8], default_port: u16) -> Option<StaticEndpoint> {
    if value.is_empty() {
        return None;
    }
    if let Some(path) = value.strip_prefix(b"unix:") {
        return unix_socket_path(path).map(|path| StaticEndpoint::Unix { path });
    }
    let (host, port) = if value.starts_with(b"[") {
        let closing = value.iter().position(|byte| *byte == b']')?;
        let port = match value.get(closing + 1..) {
            Some([]) => default_port,
            Some(rest) if rest.first() == Some(&b':') => parse_port(&rest[1..])?,
            _ => return None,
        };
        (&value[..=closing], port)
    } else if let Some(colon) = value.iter().rposition(|byte| *byte == b':') {
        if value[..colon].contains(&b':') || colon == 0 {
            return None;
        }
        (&value[..colon], parse_port(&value[colon + 1..])?)
    } else {
        (value, default_port)
    };
    if let Some(ip) = ip_address(host) {
        return Some(StaticEndpoint::Socket {
            address: SocketAddr::new(ip, port),
        });
    }
    dns_name(host).map(|host| StaticEndpoint::Dns { host, port })
}

fn parse_port(value: &[u8]) -> Option<u16> {
    let port = std::str::from_utf8(value).ok()?.parse::<u16>().ok()?;
    (port != 0).then_some(port)
}

pub(super) fn server_name_kind(value: &[u8]) -> ServerNameKind {
    if has_variable(value) {
        return ServerNameKind::Variable;
    }
    if value.starts_with(b"~") {
        return ServerNameKind::Regex;
    }
    if value.starts_with(b"*.") && value.len() > 2 && !value[2..].contains(&b'*') {
        ServerNameKind::LeadingWildcard
    } else if value.starts_with(b".") && value.len() > 1 && !value.contains(&b'*') {
        ServerNameKind::LeadingWildcardAndExact
    } else if value.ends_with(b".*") && value.len() > 2 && !value[..value.len() - 2].contains(&b'*')
    {
        ServerNameKind::TrailingWildcard
    } else if !value.contains(&b'*') {
        ServerNameKind::Exact
    } else {
        ServerNameKind::Invalid
    }
}

pub(super) fn server_name_claims(name: &EffectiveServerName) -> Vec<EffectiveServerName> {
    if name.kind != ServerNameKind::LeadingWildcardAndExact {
        return vec![name.clone()];
    }
    let suffix = name
        .normalized
        .strip_prefix(b".")
        .expect("leading-dot name has a dot prefix");
    let mut exact = name.clone();
    exact.kind = ServerNameKind::Exact;
    exact.normalized = suffix.to_vec();
    let mut wildcard = name.clone();
    wildcard.kind = ServerNameKind::LeadingWildcard;
    wildcard.normalized = [b"*.".as_slice(), suffix].concat();
    vec![exact, wildcard]
}

pub(super) const fn is_supported_server_name(kind: ServerNameKind) -> bool {
    matches!(
        kind,
        ServerNameKind::Exact
            | ServerNameKind::LeadingWildcard
            | ServerNameKind::LeadingWildcardAndExact
            | ServerNameKind::TrailingWildcard
    )
}

pub(super) const fn is_supported_location(kind: LocationKind) -> bool {
    matches!(kind, LocationKind::Exact | LocationKind::Prefix)
}

pub(super) fn is_http_policy(name: &[u8]) -> bool {
    is_location_policy(name)
        || matches!(
            name,
            b"ssl_certificate"
                | b"ssl_certificate_key"
                | b"ssl_protocols"
                | b"ssl_ciphers"
                | b"ssl_dhparam"
                | b"ssl_prefer_server_ciphers"
                | b"ssl_session_cache"
                | b"ssl_session_tickets"
                | b"ssl_session_timeout"
                | b"http2"
                | b"access_log"
                | b"error_log"
                | b"log_format"
                | b"gzip"
                | b"gzip_comp_level"
                | b"gzip_http_version"
                | b"gzip_min_length"
                | b"gzip_proxied"
                | b"gzip_types"
                | b"gzip_vary"
                | b"keepalive_timeout"
                | b"large_client_header_buffers"
                | b"limit_conn_zone"
                | b"limit_req_zone"
                | b"send_timeout"
                | b"sendfile"
                | b"types_hash_max_size"
        )
}

pub(super) fn is_server_policy(name: &[u8]) -> bool {
    is_http_policy(name)
}

pub(super) fn is_location_policy(name: &[u8]) -> bool {
    matches!(
        name,
        b"client_max_body_size"
            | b"proxy_connect_timeout"
            | b"proxy_read_timeout"
            | b"proxy_send_timeout"
            | b"proxy_http_version"
            | b"proxy_buffering"
            | b"proxy_request_buffering"
            | b"proxy_next_upstream"
            | b"proxy_next_upstream_tries"
            | b"proxy_set_header"
            | b"proxy_hide_header"
            | b"proxy_pass_header"
            | b"proxy_ignore_headers"
            | b"proxy_cookie_path"
            | b"proxy_cookie_flags"
            | b"root"
            | b"index"
            | b"return"
            | b"auth_basic"
            | b"auth_basic_user_file"
            | b"add_header"
            | b"alias"
            | b"autoindex"
            | b"autoindex_exact_size"
            | b"autoindex_localtime"
            | b"default_type"
            | b"error_page"
            | b"etag"
            | b"expires"
            | b"proxy_cache"
            | b"try_files"
    )
}

pub(super) fn is_scalar_policy(name: &[u8]) -> bool {
    matches!(
        name,
        b"client_max_body_size"
            | b"proxy_connect_timeout"
            | b"proxy_read_timeout"
            | b"proxy_send_timeout"
            | b"proxy_http_version"
            | b"proxy_buffering"
            | b"proxy_request_buffering"
            | b"proxy_next_upstream"
            | b"proxy_next_upstream_tries"
            | b"root"
            | b"return"
            | b"auth_basic"
            | b"auth_basic_user_file"
            | b"etag"
            | b"expires"
            | b"keepalive_timeout"
            | b"ssl_protocols"
            | b"ssl_ciphers"
            | b"ssl_dhparam"
            | b"ssl_prefer_server_ciphers"
            | b"ssl_session_cache"
            | b"ssl_session_tickets"
            | b"ssl_session_timeout"
            | b"http2"
            | b"gzip"
            | b"gzip_comp_level"
            | b"gzip_http_version"
            | b"gzip_min_length"
            | b"gzip_proxied"
            | b"gzip_types"
            | b"gzip_vary"
    )
}

pub(super) fn policy_argument_count_valid(name: &[u8], count: usize) -> bool {
    match name {
        b"ssl_protocols"
        | b"index"
        | b"proxy_next_upstream"
        | b"proxy_ignore_headers"
        | b"proxy_cookie_flags"
        | b"gzip_proxied"
        | b"gzip_types"
        | b"log_format"
        | b"try_files"
        | b"error_page" => count > 0,
        b"proxy_set_header"
        | b"proxy_cookie_path"
        | b"large_client_header_buffers"
        | b"limit_conn_zone"
        | b"limit_req_zone" => count >= 2,
        b"add_header" => matches!(count, 2 | 3),
        b"access_log" | b"error_log" | b"keepalive_timeout" => matches!(count, 1 | 2),
        b"return" => matches!(count, 1 | 2),
        _ => count == 1,
    }
}

pub(super) fn policy_allows_variables(name: &[u8]) -> bool {
    matches!(
        name,
        b"proxy_set_header"
            | b"proxy_cookie_path"
            | b"return"
            | b"limit_conn_zone"
            | b"limit_req_zone"
            | b"log_format"
            | b"try_files"
    )
}

pub(super) fn parse_proxy_pass(
    value: &[u8],
) -> Option<(ProxyPassScheme, Vec<u8>, Option<Vec<u8>>)> {
    let (scheme, rest) = if let Some(rest) = value.strip_prefix(b"http://") {
        (ProxyPassScheme::Http, rest)
    } else if let Some(rest) = value.strip_prefix(b"https://") {
        (ProxyPassScheme::Https, rest)
    } else if let Some(rest) = value.strip_prefix(b"$scheme://") {
        (ProxyPassScheme::Downstream, rest)
    } else {
        let separator = value.windows(3).position(|window| window == b"://")?;
        (
            ProxyPassScheme::Unsupported(value[..separator].to_vec()),
            &value[separator + 3..],
        )
    };
    let uri_start = rest
        .iter()
        .position(|byte| matches!(*byte, b'/' | b'?' | b'#'));
    let (authority, replacement_uri) = uri_start.map_or_else(
        || (rest.to_vec(), None),
        |index| (rest[..index].to_vec(), Some(rest[index..].to_vec())),
    );
    if authority.is_empty() {
        return None;
    }
    Some((scheme, authority, replacement_uri))
}

pub(super) fn has_variable(value: &[u8]) -> bool {
    value.iter().enumerate().any(|(index, byte)| {
        *byte == b'$'
            && value[..index]
                .iter()
                .rev()
                .take_while(|candidate| **candidate == b'\\')
                .count()
                % 2
                == 0
    })
}

pub(super) fn ascii_lowercase(value: &[u8]) -> Vec<u8> {
    value.iter().map(u8::to_ascii_lowercase).collect()
}
