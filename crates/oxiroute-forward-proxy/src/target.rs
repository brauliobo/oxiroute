use std::{fmt, net::IpAddr};

use http::{Uri, uri::Authority};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForwardScheme {
    Http,
    Https,
}

impl ForwardScheme {
    #[must_use]
    pub const fn default_port(self) -> u16 {
        match self {
            Self::Http => 80,
            Self::Https => 443,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Host {
    Dns(String),
    Ip(IpAddr),
}

impl fmt::Display for Host {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dns(name) => formatter.write_str(name),
            Self::Ip(IpAddr::V4(address)) => address.fmt(formatter),
            Self::Ip(IpAddr::V6(address)) => write!(formatter, "[{address}]"),
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Destination {
    pub host: Host,
    pub port: u16,
}

impl Destination {
    #[must_use]
    pub fn authority(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForwardTarget {
    pub scheme: ForwardScheme,
    pub destination: Destination,
    pub origin_form: Uri,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TargetError {
    #[error("request target is not a valid URI")]
    InvalidUri,
    #[error("forward requests require an absolute-form target")]
    AbsoluteFormRequired,
    #[error("only http and https absolute-form targets are supported")]
    UnsupportedScheme,
    #[error("URI user information is forbidden")]
    UserInfoForbidden,
    #[error("destination authority is invalid")]
    InvalidAuthority,
    #[error("CONNECT requires host:port authority-form")]
    ConnectAuthorityRequired,
    #[error("destination port must be nonzero")]
    ZeroPort,
    #[error("DNS name is invalid or not normalized safely")]
    InvalidDnsName,
}

/// Parses an HTTP absolute-form target and normalizes its destination.
///
/// # Errors
///
/// Returns [`TargetError`] when the target is malformed, is not absolute-form, uses a scheme other
/// than HTTP(S), contains user information, or has an unsafe authority representation.
pub fn parse_absolute_form(raw: &str) -> Result<ForwardTarget, TargetError> {
    if raw
        .bytes()
        .any(|byte| byte.is_ascii_control() || byte == b' ')
    {
        return Err(TargetError::InvalidUri);
    }
    let uri = raw.parse::<Uri>().map_err(|_| TargetError::InvalidUri)?;
    let scheme = match uri.scheme_str() {
        Some(value) if value.eq_ignore_ascii_case("http") => ForwardScheme::Http,
        Some(value) if value.eq_ignore_ascii_case("https") => ForwardScheme::Https,
        Some(_) => return Err(TargetError::UnsupportedScheme),
        None => return Err(TargetError::AbsoluteFormRequired),
    };
    let authority = uri.authority().ok_or(TargetError::AbsoluteFormRequired)?;
    let destination = parse_authority(authority.as_str(), Some(scheme.default_port()), false)?;
    let path_and_query = uri
        .path_and_query()
        .map_or("/", http::uri::PathAndQuery::as_str);
    let origin_form = path_and_query
        .parse::<Uri>()
        .map_err(|_| TargetError::InvalidUri)?;

    Ok(ForwardTarget {
        scheme,
        destination,
        origin_form,
    })
}

/// Parses the strict authority-form target required by classic CONNECT.
///
/// # Errors
///
/// Returns [`TargetError`] unless the target contains a valid host and explicit nonzero port.
pub fn parse_connect_authority(raw: &str) -> Result<Destination, TargetError> {
    if raw.contains('/') || raw.contains('?') || raw.contains('#') || raw.contains('@') {
        return Err(TargetError::ConnectAuthorityRequired);
    }
    parse_authority(raw, None, true).map_err(|error| match error {
        TargetError::UserInfoForbidden | TargetError::ZeroPort | TargetError::InvalidDnsName => {
            error
        }
        _ => TargetError::ConnectAuthorityRequired,
    })
}

fn parse_authority(
    raw: &str,
    default_port: Option<u16>,
    require_port: bool,
) -> Result<Destination, TargetError> {
    if raw.contains('@') {
        return Err(TargetError::UserInfoForbidden);
    }
    if raw.matches(':').count() > 1 && !raw.starts_with('[') {
        return Err(TargetError::InvalidAuthority);
    }
    let explicit_port = explicit_port(raw)?;
    let authority = raw
        .parse::<Authority>()
        .map_err(|_| TargetError::InvalidAuthority)?;
    let port = match explicit_port {
        Some(port) => match port.parse::<u16>() {
            Ok(0) => return Err(TargetError::ZeroPort),
            Ok(port) => port,
            Err(_) => return Err(TargetError::InvalidAuthority),
        },
        None if require_port => return Err(TargetError::ConnectAuthorityRequired),
        None => default_port.ok_or(TargetError::InvalidAuthority)?,
    };
    let host = normalize_host(authority.host())?;
    Ok(Destination { host, port })
}

fn explicit_port(raw: &str) -> Result<Option<&str>, TargetError> {
    if raw.starts_with('[') {
        let closing = raw.find(']').ok_or(TargetError::InvalidAuthority)?;
        let suffix = &raw[closing + 1..];
        return if suffix.is_empty() {
            Ok(None)
        } else {
            suffix
                .strip_prefix(':')
                .filter(|port| !port.is_empty())
                .map(Some)
                .ok_or(TargetError::InvalidAuthority)
        };
    }
    Ok(raw.rsplit_once(':').map(|(_, port)| port))
}

fn normalize_host(raw: &str) -> Result<Host, TargetError> {
    let unbracketed = raw
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(raw);
    if let Ok(address) = unbracketed.parse::<IpAddr>() {
        return Ok(Host::Ip(address));
    }
    if !unbracketed.is_ascii() {
        return Err(TargetError::InvalidDnsName);
    }
    let normalized = unbracketed.to_ascii_lowercase();
    let normalized = normalized.strip_suffix('.').unwrap_or(&normalized);
    if normalized.is_empty() || normalized.len() > 253 {
        return Err(TargetError::InvalidDnsName);
    }
    if normalized.split('.').any(|label| {
        label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    }) {
        return Err(TargetError::InvalidDnsName);
    }
    Ok(Host::Dns(normalized.to_owned()))
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv6Addr};

    use super::*;

    #[test]
    fn parses_and_normalizes_absolute_form() {
        let target = parse_absolute_form("HTTP://Example.COM./a?b=1").expect("absolute target");
        assert_eq!(target.scheme, ForwardScheme::Http);
        assert_eq!(target.destination.authority(), "example.com:80");
        assert_eq!(target.origin_form, "/a?b=1");
    }

    #[test]
    fn rejects_origin_form_and_user_info() {
        assert_eq!(
            parse_absolute_form("/only-a-path"),
            Err(TargetError::AbsoluteFormRequired)
        );
        assert_eq!(
            parse_absolute_form("http://user@example.com/"),
            Err(TargetError::UserInfoForbidden)
        );
    }

    #[test]
    fn parses_bracketed_ipv6_connect_authority() {
        assert_eq!(
            parse_connect_authority("[2001:db8::1]:8443").expect("IPv6 authority"),
            Destination {
                host: Host::Ip(IpAddr::V6("2001:db8::1".parse::<Ipv6Addr>().expect("IPv6"))),
                port: 8443,
            }
        );
        assert!(parse_connect_authority("2001:db8::1:8443").is_err());
        assert!(parse_connect_authority("example.com").is_err());
    }

    #[test]
    fn rejects_ports_outside_the_socket_address_range() {
        assert_eq!(
            parse_absolute_form("http://example.com:65536/"),
            Err(TargetError::InvalidAuthority)
        );
        assert!(parse_connect_authority("example.com:65536").is_err());
    }
}
