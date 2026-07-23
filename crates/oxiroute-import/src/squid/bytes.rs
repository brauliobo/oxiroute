use std::{net::IpAddr, time::Duration};

use crate::{ByteRange, Span};

use super::{PortEndpoint, Word};

pub(super) fn unsigned<T: std::str::FromStr>(value: &[u8]) -> Option<T> {
    std::str::from_utf8(value).ok()?.parse().ok()
}

pub(super) fn boolean(value: &[u8]) -> Option<bool> {
    match value {
        b"on" => Some(true),
        b"off" => Some(false),
        _ => None,
    }
}

pub(super) fn percent(value: &[u8]) -> Option<u8> {
    unsigned(value.strip_suffix(b"%")?)
}

pub(super) fn duration(value: &[u8], unit: &[u8]) -> Option<Duration> {
    let value = unsigned::<u64>(value)?;
    let seconds = match unit {
        b"second" | b"seconds" => value,
        b"minute" | b"minutes" => value.checked_mul(60)?,
        b"hour" | b"hours" => value.checked_mul(60 * 60)?,
        b"day" | b"days" => value.checked_mul(24 * 60 * 60)?,
        b"week" | b"weeks" => value.checked_mul(7 * 24 * 60 * 60)?,
        _ => return None,
    };
    Some(Duration::from_secs(seconds))
}

pub(super) fn minutes(value: &[u8]) -> Option<Duration> {
    Some(Duration::from_secs(
        unsigned::<u64>(value)?.checked_mul(60)?,
    ))
}

pub(super) fn byte_size(value: &[u8], unit: &[u8]) -> Option<u64> {
    let value = unsigned::<u64>(value)?;
    let multiplier = match unit {
        b"bytes" | b"B" => 1,
        b"KB" | b"kB" => 1024,
        b"MB" => 1024 * 1024,
        b"GB" => 1024 * 1024 * 1024,
        _ => return None,
    };
    value.checked_mul(multiplier)
}

pub(super) fn ip_network(value: &[u8]) -> Option<super::IpNetwork> {
    let (address, prefix) = value
        .iter()
        .position(|byte| *byte == b'/')
        .map(|index| (&value[..index], &value[index + 1..]))
        .map_or((value, None), |(address, prefix)| (address, Some(prefix)));
    let address = std::str::from_utf8(address).ok()?.parse::<IpAddr>().ok()?;
    let maximum = if address.is_ipv4() { 32 } else { 128 };
    let prefix_length = prefix.map_or(Some(maximum), unsigned::<u8>)?;
    (prefix_length <= maximum).then_some(super::IpNetwork {
        address,
        prefix_length,
    })
}

pub(super) fn port_range(value: &[u8]) -> Option<super::PortRange> {
    let (start, end) = value
        .iter()
        .position(|byte| *byte == b'-')
        .map(|index| (&value[..index], &value[index + 1..]))
        .map_or((value, value), |(start, end)| (start, end));
    let start = unsigned(start)?;
    let end = unsigned(end)?;
    (start <= end).then_some(super::PortRange { start, end })
}

pub(super) fn port_endpoint(value: &[u8]) -> Option<PortEndpoint> {
    if let Some(port) = unsigned(value) {
        return Some(PortEndpoint::Wildcard { port });
    }
    if value.starts_with(b"[") {
        let close = value.iter().position(|byte| *byte == b']')?;
        let address = std::str::from_utf8(&value[1..close]).ok()?.parse().ok()?;
        let port = unsigned(value.get(close + 2..)?)?;
        return (value.get(close + 1) == Some(&b':')).then_some(PortEndpoint::Ip { address, port });
    }
    let colon = value.iter().rposition(|byte| *byte == b':')?;
    let host = &value[..colon];
    let port = unsigned(&value[colon + 1..])?;
    if let Ok(address) = std::str::from_utf8(host).ok()?.parse() {
        Some(PortEndpoint::Ip { address, port })
    } else if !host.is_empty() {
        Some(PortEndpoint::Host {
            host: host.to_ascii_lowercase(),
            port,
        })
    } else {
        Some(PortEndpoint::Wildcard { port })
    }
}

pub(super) fn assignment<'a>(value: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    let index = value.iter().position(|byte| *byte == b'=')?;
    let (candidate, value) = (&value[..index], &value[index + 1..]);
    (candidate == key).then_some(value)
}

pub(super) fn words_span(words: &[Word]) -> Option<Span> {
    let first = words.first()?;
    let last = words.last()?;
    Some(Span::new(
        first.span.source(),
        ByteRange::new(first.span.range().start(), last.span.range().end()),
    ))
}
