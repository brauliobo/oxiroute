#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn validate_matcher(
    service: &str,
    route_index: usize,
    route: &mut HttpRoute,
) -> Result<(), ConfigError> {
    if let Some(host) = &mut route.host {
        validate_host_selector(service, route_index, host)?;
    }
    let requires_ascii = matches!(
        route.path,
        HttpPathSelector::AsciiCaseInsensitiveExact { .. }
    );
    let path = route.path.value_mut();
    if requires_ascii && !path.is_ascii() {
        return Err(invalid_route(
            service,
            route_index,
            "path",
            "ASCII case-insensitive exact matching requires an ASCII path",
        ));
    }
    let valid = path.starts_with('/')
        && path
            .parse::<PathAndQuery>()
            .is_ok_and(|parsed| parsed.query().is_none() && parsed.path() == path);
    if !valid {
        return Err(invalid_route(
            service,
            route_index,
            "path",
            "must be an absolute path without query or fragment",
        ));
    }
    let Some(canonical) = canonicalize_http_path(path) else {
        return Err(invalid_route(
            service,
            route_index,
            "path",
            "must have one unambiguous path interpretation",
        ));
    };
    *path = canonical.into_owned();

    if route.methods.len() > MAX_HTTP_METHODS_PER_ROUTE {
        return Err(invalid_route(
            service,
            route_index,
            "methods",
            "must contain at most 16 methods",
        ));
    }
    let mut methods = HashSet::with_capacity(route.methods.len());
    for method in &mut route.methods {
        if normalize_http_token(method).is_err() {
            return Err(invalid_route(
                service,
                route_index,
                "methods",
                "each method must be an HTTP token of at most 32 bytes",
            ));
        }
        if !methods.insert(method.clone()) {
            return Err(invalid_route(
                service,
                route_index,
                "methods",
                format!("contains duplicate method `{method}`"),
            ));
        }
    }
    route.methods.sort_unstable();
    Ok(())
}

fn validate_host_selector(
    service: &str,
    route_index: usize,
    selector: &mut HttpHostSelector,
) -> Result<(), ConfigError> {
    match selector {
        HttpHostSelector::NormalizedHost { value } => {
            let original = value.clone();
            let mut host = if original.parse::<std::net::IpAddr>().is_ok() {
                original
            } else {
                let authority = parse_authority(&original)
                    .map_err(|detail| invalid_route(service, route_index, "host", detail))?;
                authority
                    .host()
                    .strip_prefix('[')
                    .and_then(|host| host.strip_suffix(']'))
                    .unwrap_or_else(|| authority.host())
                    .to_owned()
            };
            if !normalize_host(&mut host) {
                return Err(invalid_route(
                    service,
                    route_index,
                    "host",
                    "normalized_host must be an exact DNS name, one-label wildcard, or IP address",
                ));
            }
            *value = host;
        }
        HttpHostSelector::ExactAuthority { value } => {
            if value.len() > MAX_HTTP_AUTHORITY_BYTES || value.contains('*') {
                return Err(invalid_route(
                    service,
                    route_index,
                    "host",
                    "exact_authority must be a non-wildcard authority of at most 255 bytes",
                ));
            }
            parse_authority(value)
                .map_err(|detail| invalid_route(service, route_index, "host", detail))?;
        }
        HttpHostSelector::AsciiCaseInsensitiveExactAuthority { value } => {
            if !value.is_ascii() || value.len() > MAX_HTTP_AUTHORITY_BYTES || value.contains('*') {
                return Err(invalid_route(
                    service,
                    route_index,
                    "host",
                    "ascii_case_insensitive_exact_authority must be a non-wildcard ASCII authority of at most 255 bytes",
                ));
            }
            parse_authority(value)
                .map_err(|detail| invalid_route(service, route_index, "host", detail))?;
            value.make_ascii_lowercase();
        }
        HttpHostSelector::NginxLeadingWildcard { value }
        | HttpHostSelector::NginxLeadingDot { value } => {
            value.make_ascii_lowercase();
            if !is_valid_dns_name(value) {
                return Err(invalid_route(
                    service,
                    route_index,
                    "host",
                    "nginx wildcard suffix must be a canonical DNS name",
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn parse_authority(value: &str) -> Result<Authority, &'static str> {
    if value.is_empty() || value.len() > MAX_HTTP_AUTHORITY_BYTES || value.contains('@') {
        return Err("must be a bounded authority without userinfo");
    }
    let authority = value
        .parse::<Authority>()
        .map_err(|_| "must be a valid authority")?;
    if authority.host().is_empty() || authority_has_invalid_port(&authority) {
        return Err("must contain a host and an optional numeric port");
    }
    Ok(authority)
}

pub(super) fn validate_access_policy(
    service: &str,
    route_index: usize,
    policy: &mut Option<HttpAccessPolicy>,
) -> Result<(), ConfigError> {
    let Some(policy) = policy.as_mut() else {
        return Ok(());
    };
    let identity = format!("{service} route {route_index}");
    match policy {
        HttpAccessPolicy::BearerTokenFile {
            token_file_path,
            header_name,
            realm,
        } => {
            validate_file_path(
                "HTTP access policy",
                &identity,
                "token_file_path",
                token_file_path,
            )?;
            normalize_header_name(header_name).map_err(|detail| {
                invalid_route(service, route_index, "access_policy.header_name", detail)
            })?;
            if let Some(realm) = realm {
                validate_realm(realm).map_err(|detail| {
                    invalid_route(service, route_index, "access_policy.realm", detail)
                })?;
            }
        }
        HttpAccessPolicy::BasicHtpasswdFile {
            htpasswd_file_path,
            realm,
        } => {
            validate_file_path(
                "HTTP access policy",
                &identity,
                "htpasswd_file_path",
                htpasswd_file_path,
            )?;
            validate_realm(realm).map_err(|detail| {
                invalid_route(service, route_index, "access_policy.realm", detail)
            })?;
        }
    }
    Ok(())
}

pub(crate) fn validate_realm(realm: &str) -> Result<(), &'static str> {
    let valid = !realm.is_empty()
        && realm.len() <= MAX_HTTP_ACCESS_REALM_BYTES
        && realm.is_ascii()
        && realm
            .bytes()
            .all(|byte| byte == b' ' || (byte.is_ascii_graphic() && !matches!(byte, b'"' | b'\\')));
    if valid {
        Ok(())
    } else {
        Err("must be 1..=128 safe ASCII bytes without quotes or backslashes")
    }
}
