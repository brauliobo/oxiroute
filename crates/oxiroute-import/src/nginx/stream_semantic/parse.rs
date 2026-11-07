#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn parse_stream_listen_endpoint(value: &[u8]) -> Option<ListenEndpoint> {
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
        let rest = value.get(closing + 1..)?;
        let port = rest.strip_prefix(b":").and_then(parse_port)?;
        let address = &value[..=closing];
        ip_address(address)?;
        return Some(ListenEndpoint::Socket {
            address: if address == b"[::]" {
                b"*".to_vec()
            } else {
                ascii_lowercase(address)
            },
            port,
        });
    }
    let colon = value.iter().rposition(|byte| *byte == b':')?;
    if value[..colon].contains(&b':') {
        return None;
    }
    let port = parse_port(&value[colon + 1..])?;
    let address = match &value[..colon] {
        b"" | b"*" | b"0.0.0.0" => b"*".to_vec(),
        address => {
            ip_address(address)?;
            ascii_lowercase(address)
        }
    };
    Some(ListenEndpoint::Socket { address, port })
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

pub(super) fn is_direct_endpoint_value(value: &[u8]) -> bool {
    value.starts_with(b"unix:")
        || value.contains(&b':')
        || ip_address(value).is_some()
        || dns_name(value).is_some()
}

pub(super) fn parse_duration_ms(value: &[u8]) -> Option<u64> {
    const MAX_CANONICAL_INTEGER: u64 = 9_007_199_254_740_991;

    if value.iter().all(u8::is_ascii_digit) {
        let amount = std::str::from_utf8(value).ok()?.parse::<u64>().ok()?;
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
        let amount = std::str::from_utf8(&remaining[..digit_count])
            .ok()?
            .parse::<u64>()
            .ok()?;
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

fn parse_port(value: &[u8]) -> Option<u16> {
    let port = std::str::from_utf8(value).ok()?.parse::<u16>().ok()?;
    (port != 0).then_some(port)
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
    if first_port != second_port {
        return false;
    }
    let Some(first_ip) = listen_endpoint_ip(first_address) else {
        return false;
    };
    let Some(second_ip) = listen_endpoint_ip(second_address) else {
        return false;
    };
    first_ip == second_ip || first_ip.is_unspecified() || second_ip.is_unspecified()
}

fn listen_endpoint_ip(address: &[u8]) -> Option<IpAddr> {
    if address == b"*" {
        return Some(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
    }
    let address = address
        .strip_prefix(b"[")
        .and_then(|address| address.strip_suffix(b"]"))
        .unwrap_or(address);
    ip_address(address)
}

pub(super) fn include_failure(edge: &IncludeEdge) -> Option<DiagnosticCode> {
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

pub(super) fn has_variable(value: &[u8]) -> bool {
    value
        .windows(2)
        .any(|window| window[0] == b'$' && window[1] != b'$')
}

pub(super) fn ascii_lowercase(value: &[u8]) -> Vec<u8> {
    value.to_ascii_lowercase()
}
