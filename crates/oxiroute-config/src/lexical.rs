use std::{
    borrow::Cow,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
};

use crate::{
    defaults::{
        MAX_FILE_PATH_BYTES, MAX_HTTP_METHOD_BYTES, MAX_RECORDING_SUFFIX_TEMPLATE_BYTES,
        MAX_SERVER_NAME_BYTES, MAX_UNIX_SOCKET_PATH_BYTES,
    },
    model::{
        ConfigError, DnsResolutionPolicy, Listener, ListenerBind, UpstreamEndpoint, UpstreamPool,
        UpstreamServer,
    },
};

/// A categorical failure from product-neutral lexical validation.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LexicalError {
    #[error("value is not valid UTF-8")]
    InvalidUtf8,
    #[error("value is empty")]
    Empty,
    #[error("value exceeds its byte limit")]
    TooLong,
    #[error("path is not absolute")]
    RelativePath,
    #[error("value contains NUL")]
    Nul,
    #[error("path contains repeated separators")]
    RepeatedSeparator,
    #[error("path has a trailing separator")]
    TrailingSeparator,
    #[error("path contains a dot segment")]
    DotSegment,
    #[error("DNS name is not ASCII")]
    NonAsciiDnsName,
    #[error("DNS name contains a wildcard")]
    Wildcard,
    #[error("DNS name is an IP address")]
    IpAddress,
    #[error("DNS name contains an invalid label")]
    InvalidDnsLabel,
    #[error("value is not an HTTP token")]
    InvalidHttpToken,
}

/// Returns the canonical identity for an IP address.
#[must_use]
pub fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ipv6) => ipv6.to_ipv4_mapped().map_or(IpAddr::V6(ipv6), IpAddr::V4),
        IpAddr::V4(_) => ip,
    }
}

/// Validates and lowercases a DNS endpoint name.
///
/// # Errors
///
/// Returns the corresponding [`LexicalError`] when the name is not a canonical non-IP DNS name.
pub fn canonical_dns_name(name: &str) -> Result<String, LexicalError> {
    validate_dns_name(name, false)?;
    if name.parse::<IpAddr>().is_ok() {
        return Err(LexicalError::IpAddress);
    }
    Ok(name.to_ascii_lowercase())
}

/// Validates and lowercases a DNS certificate identity, including a leading `*.` wildcard.
///
/// # Errors
///
/// Returns the corresponding [`LexicalError`] when the identity is not a canonical DNS name.
pub fn canonical_certificate_dns_name(name: &str) -> Result<String, LexicalError> {
    validate_dns_name(name, true)?;
    let exact_name = name.strip_prefix("*.").unwrap_or(name);
    if exact_name.parse::<IpAddr>().is_ok() {
        return Err(LexicalError::IpAddress);
    }
    Ok(name.to_ascii_lowercase())
}

fn validate_dns_name(name: &str, certificate_wildcard: bool) -> Result<(), LexicalError> {
    if !name.is_ascii() {
        return Err(LexicalError::NonAsciiDnsName);
    }
    if name.is_empty() {
        return Err(LexicalError::Empty);
    }
    if name.len() > MAX_SERVER_NAME_BYTES {
        return Err(LexicalError::TooLong);
    }
    if name.ends_with('.') {
        return Err(LexicalError::InvalidDnsLabel);
    }
    let exact_name = if certificate_wildcard {
        name.strip_prefix("*.").unwrap_or(name)
    } else {
        name
    };
    if exact_name.contains('*') {
        return Err(LexicalError::Wildcard);
    }
    if exact_name.is_empty() || !exact_name.split('.').all(is_valid_dns_label) {
        return Err(LexicalError::InvalidDnsLabel);
    }
    Ok(())
}

/// Validates an absolute file path without normalizing authored separators.
///
/// # Errors
///
/// Returns the corresponding [`LexicalError`] when the path violates the canonical file grammar.
pub fn validate_absolute_file_path(path: &Path) -> Result<(), LexicalError> {
    let path = path.to_str().ok_or(LexicalError::InvalidUtf8)?;
    if path.len() > MAX_FILE_PATH_BYTES {
        return Err(LexicalError::TooLong);
    }
    if !path.starts_with('/') {
        return Err(LexicalError::RelativePath);
    }
    if path.as_bytes().contains(&0) {
        return Err(LexicalError::Nul);
    }
    if path.contains("//") {
        return Err(LexicalError::RepeatedSeparator);
    }
    if path.ends_with('/') {
        return Err(LexicalError::TrailingSeparator);
    }
    if path
        .split('/')
        .any(|segment| segment == "." || segment == "..")
    {
        return Err(LexicalError::DotSegment);
    }
    Ok(())
}

/// Normalizes an absolute Unix socket path and enforces its normalized 107-byte bound.
///
/// # Errors
///
/// Returns the corresponding [`LexicalError`] when the path cannot be normalized safely.
pub fn normalize_unix_socket_path(path: &mut PathBuf) -> Result<(), LexicalError> {
    let value = path.to_str().ok_or(LexicalError::InvalidUtf8)?;
    if !value.starts_with('/') {
        return Err(LexicalError::RelativePath);
    }
    if value.as_bytes().contains(&0) {
        return Err(LexicalError::Nul);
    }
    if value.ends_with('/') {
        return Err(LexicalError::TrailingSeparator);
    }

    let mut normalized = String::with_capacity(value.len());
    for segment in value.split('/').filter(|segment| !segment.is_empty()) {
        if segment == "." || segment == ".." {
            return Err(LexicalError::DotSegment);
        }
        normalized.push('/');
        normalized.push_str(segment);
    }
    if normalized.is_empty() {
        return Err(LexicalError::Empty);
    }
    if normalized.len() > MAX_UNIX_SOCKET_PATH_BYTES {
        return Err(LexicalError::TooLong);
    }
    *path = normalized.into();
    Ok(())
}

pub(crate) fn validate_file_path(
    kind: &'static str,
    name: &str,
    field: &'static str,
    path: &Path,
) -> Result<(), ConfigError> {
    let invalid = |detail| ConfigError::InvalidFilePath {
        kind,
        name: name.into(),
        field,
        detail,
    };
    validate_absolute_file_path(path).map_err(|error| invalid(file_path_detail(error)))
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
            ListenerBind::Unix { path, .. } => {
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
        if !pool.endpoints.is_empty() {
            if !pool.servers.is_empty() {
                return Err(ConfigError::InvalidUpstreamServer {
                    pool: pool.name.clone(),
                    server: "<collection>".into(),
                    field: "servers",
                    detail: "must not be combined with legacy endpoints",
                });
            }
            pool.servers = std::mem::take(&mut pool.endpoints)
                .into_iter()
                .enumerate()
                .map(|(index, endpoint)| UpstreamServer {
                    name: format!("endpoint-{}", index + 1),
                    endpoint,
                    max_connections: None,
                    dns_resolution: DnsResolutionPolicy::OnConnect,
                })
                .collect();
        }
        for server in &mut pool.servers {
            normalize_upstream_endpoint(&pool.name, &mut server.endpoint)?;
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
    if path.to_str().is_some_and(|value| {
        value.starts_with('/') && value.bytes().any(|byte| byte.is_ascii_control())
    }) {
        return Err(invalid("path must not contain NUL or control bytes"));
    }
    normalize_unix_socket_path(path).map_err(|error| invalid(unix_path_detail(error)))
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

pub(crate) fn normalize_upstream_server_names(upstream_pools: &mut [UpstreamPool]) {
    for tls in upstream_pools
        .iter_mut()
        .filter_map(|pool| pool.tls.as_mut())
    {
        tls.server_name.make_ascii_lowercase();
    }
}

pub(crate) fn is_valid_certificate_dns_name(dns_name: &str) -> bool {
    dns_name.parse::<IpAddr>().is_ok() || canonical_certificate_dns_name(dns_name).is_ok()
}

pub(crate) fn is_valid_dns_name(name: &str) -> bool {
    canonical_dns_name(name).is_ok()
}

const fn file_path_detail(error: LexicalError) -> &'static str {
    match error {
        LexicalError::InvalidUtf8 => "path must be valid UTF-8",
        LexicalError::TooLong => "path exceeds 4096 bytes",
        LexicalError::RelativePath => "path must be absolute",
        LexicalError::Nul => "path must not contain NUL",
        LexicalError::RepeatedSeparator => "path must not contain repeated `/` separators",
        LexicalError::TrailingSeparator => "path must identify a file, not end with `/`",
        LexicalError::DotSegment => "path must not contain `.` or `..` segments",
        _ => unreachable!(),
    }
}

const fn unix_path_detail(error: LexicalError) -> &'static str {
    match error {
        LexicalError::InvalidUtf8 => "path must be valid UTF-8",
        LexicalError::Empty => "path must identify a socket",
        LexicalError::TooLong => "path exceeds 107 bytes",
        LexicalError::RelativePath => "path must be absolute",
        LexicalError::Nul => "path must not contain NUL or control bytes",
        LexicalError::TrailingSeparator => "path must identify a socket, not end with `/`",
        LexicalError::DotSegment => "path must not contain `.` or `..` segments",
        _ => unreachable!(),
    }
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

pub(crate) fn normalize_http_token(method: &mut str) -> Result<(), LexicalError> {
    if method.is_empty() {
        return Err(LexicalError::Empty);
    }
    if method.len() > MAX_HTTP_METHOD_BYTES {
        return Err(LexicalError::TooLong);
    }
    if !method
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
    {
        return Err(LexicalError::InvalidHttpToken);
    }
    method.make_ascii_uppercase();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{LexicalError, normalize_http_token};

    #[test]
    fn normalizes_bounded_http_tokens() {
        for (authored, canonical) in [
            ("get", "GET"),
            ("M-SEARCH", "M-SEARCH"),
            ("x1", "X1"),
            ("foo!", "FOO!"),
            ("!#$%&'*+-.^_`|~", "!#$%&'*+-.^_`|~"),
        ] {
            let mut method = authored.to_owned();
            normalize_http_token(&mut method).expect("valid HTTP token");
            assert_eq!(method, canonical);
        }
    }

    #[test]
    fn rejects_invalid_http_tokens_without_mutating_them() {
        for (authored, expected) in [
            ("", LexicalError::Empty),
            ("FOO@", LexicalError::InvalidHttpToken),
            ("GÉT", LexicalError::InvalidHttpToken),
            ("GET\u{1}", LexicalError::InvalidHttpToken),
            ("X12345678901234567890123456789012", LexicalError::TooLong),
        ] {
            let mut method = authored.to_owned();
            assert_eq!(normalize_http_token(&mut method), Err(expected));
            assert_eq!(method, authored);
        }
    }
}
