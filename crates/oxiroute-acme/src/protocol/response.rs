#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn renewal_information_endpoint(
    base: &str,
    certificate_id: &str,
) -> Result<String, AcmeError> {
    if base.contains(['?', '#']) {
        return Err(AcmeError::InvalidRenewalInformation);
    }
    let separator = if base.ends_with('/') { "" } else { "/" };
    let endpoint = format!("{base}{separator}{certificate_id}");
    (endpoint.len() <= MAX_ACME_URL_BYTES)
        .then_some(endpoint)
        .ok_or(AcmeError::InvalidRenewalInformation)
}

pub(super) fn parse_renewal_timestamp(value: &str) -> Result<u64, AcmeError> {
    if value.is_empty() || value.len() > 64 || !value.is_ascii() {
        return Err(AcmeError::InvalidRenewalInformation);
    }
    let timestamp = OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|_| AcmeError::InvalidRenewalInformation)?
        .unix_timestamp();
    u64::try_from(timestamp).map_err(|_| AcmeError::InvalidRenewalInformation)
}

pub(super) fn validate_nonce(nonce: &str) -> Result<(), AcmeError> {
    if nonce.is_empty()
        || nonce.len() > 256
        || !nonce.is_ascii()
        || nonce.bytes().any(|byte| byte <= b' ')
    {
        return Err(AcmeError::MalformedResponse);
    }
    Ok(())
}

pub(super) fn bounded_json(bytes: &[u8], limit: usize) -> Result<Value, AcmeError> {
    if bytes.len() > limit {
        return Err(AcmeError::ResponseTooLarge { limit });
    }
    serde_json::from_slice(bytes).map_err(|_| AcmeError::MalformedResponse)
}

pub(super) fn required_string(value: &Value, name: &str) -> Result<String, AcmeError> {
    value
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= MAX_ACME_URL_BYTES)
        .map(str::to_owned)
        .ok_or(AcmeError::MissingField)
}

pub(super) fn optional_string(value: &Value, name: &str) -> Result<Option<String>, AcmeError> {
    match value.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .filter(|value| !value.is_empty() && value.len() <= MAX_ACME_URL_BYTES)
            .map(str::to_owned)
            .map(Some)
            .ok_or(AcmeError::MalformedResponse),
    }
}

pub(super) fn string_array(value: &Value, name: &str) -> Result<Vec<String>, AcmeError> {
    value
        .get(name)
        .and_then(Value::as_array)
        .ok_or(AcmeError::MissingField)?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or(AcmeError::MalformedResponse)
        })
        .collect()
}

pub(super) fn problem_or_status(response: &HttpResponse) -> AcmeError {
    parse_json_body(&response.body)
        .and_then(|value| value.get("type").and_then(Value::as_str).map(str::to_owned))
        .map_or(
            AcmeError::UnexpectedStatus {
                status: response.status,
            },
            |problem_type| AcmeError::Problem { problem_type },
        )
}

pub(super) fn is_bad_nonce(response: &HttpResponse) -> bool {
    response.status == 400
        && parse_json_body(&response.body)
            .and_then(|value| value.get("type").and_then(Value::as_str).map(str::to_owned))
            .is_some_and(|problem_type| problem_type.ends_with("badNonce"))
}

fn parse_json_body(body: &[u8]) -> Option<Value> {
    (body.len() <= MAX_ACME_BODY_BYTES)
        .then(|| serde_json::from_slice::<Value>(body).ok())
        .flatten()
}

pub(super) fn validate_response_url(
    requested_url: &str,
    response: &HttpResponse,
    policy: &OriginPolicy,
) -> Result<(), AcmeError> {
    if response.url.is_empty() {
        return Err(AcmeError::MalformedResponse);
    }
    if origin_of(requested_url)? != origin_of(&response.url)? {
        return Err(AcmeError::UntrustedRedirect);
    }
    policy.permits(requested_url)?;
    policy.permits(&response.url)?;
    Ok(())
}

pub(super) fn origin_of(url: &str) -> Result<String, AcmeError> {
    if url.len() > MAX_ACME_URL_BYTES || !url.starts_with("https://") {
        return Err(AcmeError::InvalidDirectoryUrl);
    }
    let rest = &url[8..];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty() || authority.contains('@') || authority.bytes().any(|byte| byte <= b' ')
    {
        return Err(AcmeError::InvalidDirectoryUrl);
    }
    let (host, port) = if authority.starts_with('[') {
        let end = authority.find(']').ok_or(AcmeError::InvalidDirectoryUrl)?;
        let port = authority[end + 1..].strip_prefix(':');
        if end + 1 < authority.len() && port.is_none() {
            return Err(AcmeError::InvalidDirectoryUrl);
        }
        (&authority[1..end], port)
    } else {
        if authority.matches(':').count() > 1 {
            return Err(AcmeError::InvalidDirectoryUrl);
        }
        authority
            .rsplit_once(':')
            .map_or((authority, None), |(host, port)| (host, Some(port)))
    };
    if host.is_empty() || host.len() > 253 || host.eq_ignore_ascii_case("localhost") {
        return Err(AcmeError::InvalidDirectoryUrl);
    }
    if let Some(port) = port
        && (port.is_empty() || port.parse::<u16>().is_err())
    {
        return Err(AcmeError::InvalidDirectoryUrl);
    }
    Ok(format!("https://{}", authority.to_ascii_lowercase()))
}

pub(super) fn is_private_origin(origin: &str) -> bool {
    let authority = origin.strip_prefix("https://").unwrap_or_default();
    let host = authority
        .strip_prefix('[')
        .and_then(|value| value.split(']').next())
        .or_else(|| authority.split(':').next())
        .unwrap_or_default();
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(address)) => {
            address.is_loopback()
                || address.is_private()
                || address.is_link_local()
                || address.is_unspecified()
        }
        Ok(IpAddr::V6(address)) => {
            address.is_loopback()
                || address.is_unspecified()
                || address.is_unique_local()
                || address.is_unicast_link_local()
        }
        Err(_) => false,
    }
}
