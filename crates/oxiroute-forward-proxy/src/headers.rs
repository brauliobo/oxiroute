use http::{HeaderMap, HeaderName, HeaderValue, header};

use crate::Destination;

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum HeaderSanitizationError {
    #[error("Connection contains an invalid nominated header")]
    InvalidConnectionToken,
    #[error("normalized destination cannot be represented as a Host header")]
    InvalidHost,
}

/// Removes proxy and hop-by-hop fields and writes the normalized origin `Host` value.
///
/// # Errors
///
/// Returns [`HeaderSanitizationError`] if a `Connection` token or normalized host cannot be safely
/// represented as an HTTP header.
pub fn sanitize_request_headers(
    source: &HeaderMap,
    destination: &Destination,
) -> Result<HeaderMap, HeaderSanitizationError> {
    let mut connection_headers = Vec::new();
    for value in source.get_all(header::CONNECTION) {
        let text = value
            .to_str()
            .map_err(|_| HeaderSanitizationError::InvalidConnectionToken)?;
        for token in text.split(',') {
            let token = token.trim();
            if token.is_empty() {
                return Err(HeaderSanitizationError::InvalidConnectionToken);
            }
            connection_headers.push(
                HeaderName::from_bytes(token.as_bytes())
                    .map_err(|_| HeaderSanitizationError::InvalidConnectionToken)?,
            );
        }
    }

    let mut sanitized = source.clone();
    for name in connection_headers {
        sanitized.remove(name);
    }
    for name in [
        header::CONNECTION,
        HeaderName::from_static("keep-alive"),
        header::PROXY_AUTHENTICATE,
        header::PROXY_AUTHORIZATION,
        header::TE,
        header::TRAILER,
        header::TRANSFER_ENCODING,
        header::UPGRADE,
        HeaderName::from_static("proxy-connection"),
    ] {
        sanitized.remove(name);
    }
    let host = HeaderValue::from_str(&destination.authority())
        .map_err(|_| HeaderSanitizationError::InvalidHost)?;
    sanitized.insert(header::HOST, host);
    Ok(sanitized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Host, parse_connect_authority};

    #[test]
    fn removes_hop_by_hop_and_connection_nominated_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONNECTION, "keep-alive, x-private".parse().unwrap());
        headers.insert("x-private", "secret".parse().unwrap());
        headers.insert(header::PROXY_AUTHORIZATION, "Basic hidden".parse().unwrap());
        headers.insert("x-end-to-end", "kept".parse().unwrap());
        let destination = parse_connect_authority("example.com:443").unwrap();

        let sanitized = sanitize_request_headers(&headers, &destination).unwrap();

        assert!(!sanitized.contains_key(header::CONNECTION));
        assert!(!sanitized.contains_key("x-private"));
        assert!(!sanitized.contains_key(header::PROXY_AUTHORIZATION));
        assert_eq!(sanitized[header::HOST], "example.com:443");
        assert_eq!(sanitized["x-end-to-end"], "kept");
        assert!(matches!(destination.host, Host::Dns(_)));
    }
}
