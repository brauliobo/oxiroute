#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn validate_action(
    service: &str,
    route_index: usize,
    action: &mut HttpRouteAction,
    route_policy: crate::model::HttpRoutePolicy,
    pools: &HashMap<&str, &UpstreamPool>,
    cache_stores: &HashMap<String, crate::cache_validation::CacheStoreBounds>,
) -> Result<(), ConfigError> {
    match action {
        HttpRouteAction::Proxy {
            upstream_pool,
            policy,
        } => {
            let pool = pools.get(upstream_pool.as_str()).ok_or_else(|| {
                ConfigError::UnknownRouteUpstreamPool {
                    service: service.into(),
                    route: route_index,
                    pool: upstream_pool.clone(),
                }
            })?;
            validate_proxy_policy(
                service,
                route_index,
                policy,
                route_policy,
                pool,
                pools,
                cache_stores,
            )
        }
        HttpRouteAction::FixedResponse {
            status,
            body,
            headers,
        } => validate_fixed_response(service, route_index, *status, body, headers),
        HttpRouteAction::Redirect {
            status,
            location,
            headers,
        } => validate_redirect(service, route_index, *status, location, headers),
        HttpRouteAction::StaticFiles {
            root_directory,
            index_files,
            spa_fallback,
            try_files,
            mime,
            headers,
            error_responses,
            ..
        } => validate_static_files(
            service,
            route_index,
            &mut StaticFilesValidation {
                root_directory,
                index_files,
                spa_fallback: spa_fallback.as_deref(),
                try_files,
                mime,
                headers,
                error_responses,
            },
        ),
    }
}

fn validate_fixed_response(
    service: &str,
    route_index: usize,
    status: u16,
    body: &str,
    headers: &mut [HttpLiteralHeader],
) -> Result<(), ConfigError> {
    if !(200..=599).contains(&status) {
        return Err(invalid_route(
            service,
            route_index,
            "action.fixed_response.status",
            "must be between 200 and 599",
        ));
    }
    if body.len() > MAX_HTTP_FIXED_RESPONSE_BODY_BYTES {
        return Err(invalid_route(
            service,
            route_index,
            "action.fixed_response.body",
            "must not exceed 65536 UTF-8 bytes",
        ));
    }
    if matches!(status, 204 | 205 | 304) && !body.is_empty() {
        return Err(invalid_route(
            service,
            route_index,
            "action.fixed_response.body",
            format!("must be empty for body-forbidden status {status}"),
        ));
    }
    validate_literal_headers(
        service,
        route_index,
        "action.fixed_response.headers",
        headers,
    )
}

pub(super) fn validate_literal_headers(
    service: &str,
    route_index: usize,
    field: &'static str,
    headers: &mut [HttpLiteralHeader],
) -> Result<(), ConfigError> {
    if headers.len() > MAX_HTTP_LITERAL_HEADERS {
        return Err(invalid_route(
            service,
            route_index,
            field,
            "must contain at most 32 headers",
        ));
    }
    for header in headers {
        normalize_header_name(&mut header.name)
            .map_err(|detail| invalid_route(service, route_index, field, detail))?;
        validate_header_value(&header.value)
            .map_err(|detail| invalid_route(service, route_index, field, detail))?;
    }
    Ok(())
}

fn validate_redirect(
    service: &str,
    route_index: usize,
    status: u16,
    location: &HttpRedirectLocation,
    headers: &mut [HttpLiteralHeader],
) -> Result<(), ConfigError> {
    if !matches!(status, 301 | 302 | 307 | 308) {
        return Err(invalid_route(
            service,
            route_index,
            "action.redirect.status",
            "must be 301, 302, 307, or 308",
        ));
    }
    let (value, request_template) = match location {
        HttpRedirectLocation::Literal { value } => (value, false),
        HttpRedirectLocation::RequestTemplate {
            value,
            nginx_host_fallback,
        } => {
            if let Some(fallback) = nginx_host_fallback {
                validate_nginx_host_fallback(fallback).map_err(|detail| {
                    invalid_route(service, route_index, "action.redirect.location", detail)
                })?;
            }
            (value, true)
        }
    };
    if value.is_empty() || value.len() > MAX_HTTP_REDIRECT_LOCATION_BYTES {
        return Err(invalid_route(
            service,
            route_index,
            "action.redirect.location",
            "must be 1..=2048 bytes",
        ));
    }
    validate_header_value(value).map_err(|detail| {
        invalid_route(service, route_index, "action.redirect.location", detail)
    })?;
    if request_template && !is_valid_location_template(value) {
        return Err(invalid_route(
            service,
            route_index,
            "action.redirect.location",
            "supports only $scheme, $host, and $request_uri variables",
        ));
    }
    validate_literal_headers(service, route_index, "action.redirect.headers", headers)
}

fn is_valid_location_template(template: &str) -> bool {
    let mut remainder = template;
    while let Some((_, after_dollar)) = remainder.split_once('$') {
        let Some(variable) = ["scheme", "host", "request_uri"]
            .into_iter()
            .find(|variable| after_dollar.starts_with(variable))
        else {
            return false;
        };
        remainder = &after_dollar[variable.len()..];
    }
    true
}

/// Accepts what nginx sends as `$host` without a request host: its primary server name, which is a normalized
/// exact name or address, `_`, a `*.`/`.` leading or `.*` trailing wildcard, or empty (the header is then omitted).
pub(super) fn validate_nginx_host_fallback(value: &str) -> Result<(), &'static str> {
    const DETAIL: &str = "nginx host fallback must be empty or a normalized nginx server name without a port or userinfo";
    if value.is_empty() || value == "_" {
        return Ok(());
    }
    if value.len() > MAX_HTTP_AUTHORITY_BYTES || value.contains('@') {
        return Err(DETAIL);
    }
    if let Some(ip) = value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .and_then(|value| value.parse::<std::net::Ipv6Addr>().ok())
    {
        return (value == format!("[{ip}]")).then_some(()).ok_or(DETAIL);
    }
    if value.contains(':') {
        return Err(DETAIL);
    }
    let name = match (value.strip_prefix('.'), value.strip_suffix(".*")) {
        (Some(name), _) | (None, Some(name)) => name,
        (None, None) => value,
    };
    let mut normalized = name.to_owned();
    (normalize_host(&mut normalized) && normalized == name.to_ascii_lowercase())
        .then_some(())
        .ok_or(DETAIL)
}
