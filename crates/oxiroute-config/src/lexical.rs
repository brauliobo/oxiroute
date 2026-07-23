use std::{
    borrow::Cow,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
};

use crate::{
    defaults::{
        MAX_FILE_PATH_BYTES, MAX_RECORDING_SUFFIX_TEMPLATE_BYTES, MAX_SERVER_NAME_BYTES,
        MAX_UNIX_SOCKET_PATH_BYTES,
    },
    model::{ConfigError, Listener, ListenerBind, UpstreamEndpoint, UpstreamPool},
};

pub(crate) fn validate_file_path(
    kind: &'static str,
    name: &str,
    field: &'static str,
    path: &Path,
) -> Result<(), ConfigError> {
    validate_path(
        kind,
        name,
        field,
        path,
        "path must identify a file, not end with `/`",
    )
}

pub(crate) fn validate_directory_path(
    kind: &'static str,
    name: &str,
    field: &'static str,
    path: &Path,
) -> Result<(), ConfigError> {
    validate_path(
        kind,
        name,
        field,
        path,
        "directory path must not end with `/`",
    )
}

fn validate_path(
    kind: &'static str,
    name: &str,
    field: &'static str,
    path: &Path,
    trailing_separator_detail: &'static str,
) -> Result<(), ConfigError> {
    let invalid = |detail| ConfigError::InvalidFilePath {
        kind,
        name: name.into(),
        field,
        detail,
    };
    let path = path
        .to_str()
        .ok_or_else(|| invalid("path must be valid UTF-8"))?;
    if path.len() > MAX_FILE_PATH_BYTES {
        return Err(invalid("path exceeds 4096 bytes"));
    }
    if !path.starts_with('/') {
        return Err(invalid("path must be absolute"));
    }
    if path.as_bytes().contains(&0) {
        return Err(invalid("path must not contain NUL"));
    }
    if path.contains("//") {
        return Err(invalid("path must not contain repeated `/` separators"));
    }
    if path.ends_with('/') {
        return Err(invalid(trailing_separator_detail));
    }
    if path
        .split('/')
        .any(|segment| segment == "." || segment == "..")
    {
        return Err(invalid("path must not contain `.` or `..` segments"));
    }

    Ok(())
}

pub(crate) fn normalize_listener_binds(listeners: &mut [Listener]) -> Result<(), ConfigError> {
    for listener in listeners {
        match &mut listener.bind {
            ListenerBind::Socket { address } | ListenerBind::Udp { address } => {
                normalize_socket_address(address);
            }
            ListenerBind::Unix { path } => {
                normalize_unix_path("listener", &listener.name, "bind.path", path)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn normalize_upstream_endpoints(
    upstream_pools: &mut [UpstreamPool],
) -> Result<(), ConfigError> {
    for pool in upstream_pools {
        for endpoint in &mut pool.endpoints {
            normalize_upstream_endpoint(&pool.name, endpoint)?;
        }
    }
    Ok(())
}

pub(crate) fn normalize_upstream_endpoint(
    pool: &str,
    endpoint: &mut UpstreamEndpoint,
) -> Result<(), ConfigError> {
    match endpoint {
        UpstreamEndpoint::Socket { address } => normalize_socket_address(address),
        UpstreamEndpoint::Dns { host, .. } => host.make_ascii_lowercase(),
        UpstreamEndpoint::Unix { path } => {
            normalize_unix_path("upstream pool", pool, "endpoints[].path", path)?;
        }
    }
    Ok(())
}

fn normalize_socket_address(address: &mut SocketAddr) {
    *address = SocketAddr::new(canonical_ip(address.ip()), address.port());
}

fn normalize_unix_path(
    kind: &'static str,
    name: &str,
    field: &'static str,
    path: &mut PathBuf,
) -> Result<(), ConfigError> {
    let invalid = |detail| ConfigError::InvalidUnixPath {
        kind,
        name: name.into(),
        field,
        detail,
    };
    normalize_absolute_path(
        path,
        MAX_UNIX_SOCKET_PATH_BYTES,
        "path must identify a socket, not end with `/`",
        "path must identify a socket",
        "path exceeds 107 bytes",
    )
    .map_err(invalid)
}

pub(crate) fn normalize_recording_root(path: &mut PathBuf) -> Result<(), &'static str> {
    normalize_absolute_directory(path)
}

pub(crate) fn normalize_absolute_directory(path: &mut PathBuf) -> Result<(), &'static str> {
    if path
        .to_str()
        .is_some_and(|value| value.len() > MAX_FILE_PATH_BYTES)
    {
        return Err("path exceeds 4096 bytes");
    }
    normalize_absolute_path(
        path,
        MAX_FILE_PATH_BYTES,
        "directory path must not end with `/`",
        "path must identify a directory",
        "path exceeds 4096 bytes",
    )
}

pub(crate) fn validate_relative_path(path: &Path, max_bytes: usize) -> Result<(), &'static str> {
    let value = path.to_str().ok_or("path must be valid UTF-8")?;
    if value.is_empty() {
        return Err("path must not be empty");
    }
    if value.len() > max_bytes {
        return Err("path exceeds its byte limit");
    }
    if value.starts_with('/') {
        return Err("path must be relative");
    }
    if value
        .bytes()
        .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return Err("path must not contain NUL or control bytes");
    }
    if value.contains('\\') {
        return Err("path must not contain backslashes");
    }
    if value.ends_with('/') || value.contains("//") {
        return Err("path must not contain empty segments");
    }
    if value
        .split('/')
        .any(|segment| segment == "." || segment == "..")
    {
        return Err("path must not contain `.` or `..` segments");
    }
    Ok(())
}

fn normalize_absolute_path(
    path: &mut PathBuf,
    max_bytes: usize,
    trailing_separator_detail: &'static str,
    empty_detail: &'static str,
    too_long_detail: &'static str,
) -> Result<(), &'static str> {
    let value = path.to_str().ok_or("path must be valid UTF-8")?;
    if !value.starts_with('/') {
        return Err("path must be absolute");
    }
    if value
        .bytes()
        .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return Err("path must not contain NUL or control bytes");
    }
    if value.ends_with('/') {
        return Err(trailing_separator_detail);
    }

    let mut normalized = String::with_capacity(value.len());
    for segment in value.split('/').filter(|segment| !segment.is_empty()) {
        if segment == "." || segment == ".." {
            return Err("path must not contain `.` or `..` segments");
        }
        normalized.push('/');
        normalized.push_str(segment);
    }
    if normalized.is_empty() {
        return Err(empty_detail);
    }
    if normalized.len() > max_bytes {
        return Err(too_long_detail);
    }

    *path = normalized.into();
    Ok(())
}

pub(crate) fn validate_recording_suffix_template(
    suffix_template: &str,
) -> Result<(), &'static str> {
    let bytes = suffix_template.as_bytes();
    if bytes.len() > MAX_RECORDING_SUFFIX_TEMPLATE_BYTES {
        return Err("template exceeds 128 bytes");
    }
    if bytes.contains(&0) {
        return Err("template must not contain NUL");
    }
    if bytes.iter().any(|byte| matches!(*byte, b'/' | b'\\')) {
        return Err("template must not contain path separators");
    }

    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        if !matches!(
            bytes.get(index + 1),
            Some(b'Y' | b'm' | b'd' | b'H' | b'M' | b'S' | b'%')
        ) {
            return Err("template contains an unsupported percent format item");
        }
        index += 2;
    }
    Ok(())
}

pub(crate) fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ipv6) => ipv6.to_ipv4_mapped().map_or(IpAddr::V6(ipv6), IpAddr::V4),
        IpAddr::V4(_) => ip,
    }
}

pub(crate) fn normalize_upstream_server_names(upstream_pools: &mut [UpstreamPool]) {
    for tls in upstream_pools
        .iter_mut()
        .filter_map(|pool| pool.tls.as_mut())
    {
        tls.server_name.make_ascii_lowercase();
    }
}

pub(crate) fn is_valid_certificate_dns_name(dns_name: &str) -> bool {
    if !dns_name.is_ascii()
        || dns_name.is_empty()
        || dns_name.len() > MAX_SERVER_NAME_BYTES
        || dns_name.ends_with('.')
    {
        return false;
    }

    let exact_name = if let Some(exact_name) = dns_name.strip_prefix("*.") {
        if exact_name.parse::<IpAddr>().is_ok() {
            return false;
        }
        exact_name
    } else {
        if dns_name.contains('*') || dns_name.parse::<IpAddr>().is_ok() {
            return false;
        }
        dns_name
    };

    !exact_name.is_empty() && exact_name.split('.').all(is_valid_dns_label)
}

pub(crate) fn is_valid_dns_name(name: &str) -> bool {
    name.is_ascii()
        && !name.is_empty()
        && name.len() <= MAX_SERVER_NAME_BYTES
        && !name.ends_with('.')
        && !name.contains('*')
        && name.parse::<IpAddr>().is_err()
        && name.split('.').all(is_valid_dns_label)
}

fn is_valid_dns_label(label: &str) -> bool {
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
}

pub(crate) fn authority_has_invalid_port(authority: &http::uri::Authority) -> bool {
    let value = authority.as_str();
    if let Some(remainder) = value
        .strip_prefix('[')
        .and_then(|value| value.split_once(']').map(|(_, remainder)| remainder))
    {
        !remainder.is_empty() && (!remainder.starts_with(':') || authority.port().is_none())
    } else {
        value.contains(':') && authority.port().is_none()
    }
}

pub(crate) fn normalize_host(host: &mut String) -> bool {
    host.make_ascii_lowercase();
    if let Ok(ip) = host.parse::<IpAddr>() {
        *host = ip.to_string();
        return true;
    }

    if host.len() > MAX_SERVER_NAME_BYTES {
        return false;
    }

    let dns_name = if let Some(dns_name) = host.strip_prefix("*.") {
        if dns_name.parse::<IpAddr>().is_ok() {
            return false;
        }
        dns_name
    } else {
        if host.starts_with('*') {
            return false;
        }
        host.as_str()
    };

    !dns_name.is_empty() && dns_name.split('.').all(is_valid_dns_label)
}

/// Returns whether an HTTP path has one stable interpretation for routing and forwarding.
///
/// Dot segments, backslashes, repeated separators, malformed escapes, percent-encoded
/// unreserved characters, and encoded path separators are rejected.
#[must_use]
pub fn is_unambiguous_http_path(path: &str) -> bool {
    canonicalize_http_path(path).is_some()
}

/// Validates an HTTP path and uppercases accepted percent-triplet hex digits.
#[must_use]
pub fn canonicalize_http_path(path: &str) -> Option<Cow<'_, str>> {
    let bytes = path.as_bytes();
    if bytes.contains(&b'\\')
        || bytes.windows(2).any(|window| window == b"//")
        || path
            .split('/')
            .any(|segment| segment == "." || segment == "..")
    {
        return None;
    }

    let mut canonical = None;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        let encoded = bytes
            .get(index + 1..index + 3)
            .and_then(|digits| decode_hex_byte(digits[0], digits[1]))?;
        if encoded.is_ascii_alphanumeric()
            || matches!(encoded, b'-' | b'.' | b'_' | b'~' | b'/' | b'\\')
        {
            return None;
        }
        if bytes[index + 1] != bytes[index + 1].to_ascii_uppercase()
            || bytes[index + 2] != bytes[index + 2].to_ascii_uppercase()
        {
            let canonical = canonical.get_or_insert_with(|| bytes.to_vec());
            canonical[index + 1] = canonical[index + 1].to_ascii_uppercase();
            canonical[index + 2] = canonical[index + 2].to_ascii_uppercase();
        }
        index += 3;
    }
    canonical.map_or_else(
        || Some(Cow::Borrowed(path)),
        |canonical| String::from_utf8(canonical).ok().map(Cow::Owned),
    )
}

fn decode_hex_byte(high: u8, low: u8) -> Option<u8> {
    Some(hex_nibble(high)? << 4 | hex_nibble(low)?)
}

const fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

pub(crate) fn is_uppercase_http_token(method: &str) -> bool {
    !method.is_empty()
        && method.bytes().all(|byte| {
            byte.is_ascii_uppercase() || byte.is_ascii_digit() || b"!#$%&'*+-.^_`|~".contains(&byte)
        })
}
