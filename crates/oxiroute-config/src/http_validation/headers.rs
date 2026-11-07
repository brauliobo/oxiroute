#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn validate_request_mutations(
    service: &str,
    route_index: usize,
    mutations: &mut [HttpRequestHeaderMutation],
) -> Result<(), ConfigError> {
    if mutations.len() > MAX_HTTP_HEADER_MUTATIONS {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.request_headers",
            "must contain at most 32 header mutations",
        ));
    }
    let mut names = HashSet::with_capacity(mutations.len());
    for mutation in mutations {
        validate_request_mutation(service, route_index, mutation)?;
        if !names.insert(mutation.name().to_owned()) {
            return Err(invalid_route(
                service,
                route_index,
                "action.policy.request_headers",
                format!(
                    "contains duplicate or conflicting mutation for `{}`",
                    mutation.name()
                ),
            ));
        }
    }
    Ok(())
}

fn validate_request_mutation(
    service: &str,
    route_index: usize,
    mutation: &mut HttpRequestHeaderMutation,
) -> Result<(), ConfigError> {
    normalize_header_name_syntax(mutation.name_mut()).map_err(|detail| {
        invalid_route(
            service,
            route_index,
            "action.policy.request_headers",
            detail,
        )
    })?;
    let is_x_forwarded_for = mutation.name() == "x-forwarded-for";
    if let HttpRequestHeaderMutation::Set { value, .. } = mutation {
        match value {
            HttpRequestHeaderValue::Literal { value } => {
                validate_header_value(value).map_err(|detail| {
                    invalid_route(
                        service,
                        route_index,
                        "action.policy.request_headers",
                        detail,
                    )
                })?;
            }
            HttpRequestHeaderValue::AppendedXForwardedFor {
                max_bytes,
                except_source_cidrs,
            } => validate_forwarded_for_value(
                service,
                route_index,
                is_x_forwarded_for,
                *max_bytes,
                except_source_cidrs,
            )?,
            HttpRequestHeaderValue::IncomingHeader { name, max_bytes } => {
                normalize_header_name_syntax(name).map_err(|detail| {
                    invalid_route(
                        service,
                        route_index,
                        "action.policy.request_headers",
                        detail,
                    )
                })?;
                validate_dynamic_header_bound(*max_bytes).map_err(|detail| {
                    invalid_route(
                        service,
                        route_index,
                        "action.policy.request_headers",
                        detail,
                    )
                })?;
            }
            HttpRequestHeaderValue::IncomingAuthority
            | HttpRequestHeaderValue::NormalizedHost
            | HttpRequestHeaderValue::ClientIp
            | HttpRequestHeaderValue::DownstreamScheme
            | HttpRequestHeaderValue::SelectedUpstreamHost => {}
            HttpRequestHeaderValue::NginxHost { fallback } => {
                validate_nginx_host_fallback(fallback).map_err(|detail| {
                    invalid_route(
                        service,
                        route_index,
                        "action.policy.request_headers",
                        detail,
                    )
                })?;
            }
        }
    }
    if is_forbidden_header(mutation.name()) && !is_pingora_managed_upgrade_mutation(mutation) {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.request_headers",
            "header is hop-by-hop, framing, or managed by upstream Host policy",
        ));
    }
    let has_forbidden_incoming_source = match mutation {
        HttpRequestHeaderMutation::Set {
            value: HttpRequestHeaderValue::IncomingHeader { name, .. },
            ..
        } => is_forbidden_header(name),
        _ => false,
    };
    if has_forbidden_incoming_source && !is_pingora_managed_upgrade_mutation(mutation) {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.request_headers",
            "incoming header is hop-by-hop or framing",
        ));
    }
    Ok(())
}

fn validate_forwarded_for_value(
    service: &str,
    route_index: usize,
    is_x_forwarded_for: bool,
    max_bytes: u64,
    except_source_cidrs: &mut [String],
) -> Result<(), ConfigError> {
    let field = "action.policy.request_headers";
    validate_dynamic_header_bound(max_bytes)
        .map_err(|detail| invalid_route(service, route_index, field, detail))?;
    if !is_x_forwarded_for {
        return Err(invalid_route(
            service,
            route_index,
            field,
            "appended X-Forwarded-For values require the x-forwarded-for header name",
        ));
    }
    if except_source_cidrs.len() > 16 {
        return Err(invalid_route(
            service,
            route_index,
            field,
            "X-Forwarded-For source exceptions must contain at most 16 CIDRs",
        ));
    }
    let mut unique = HashSet::with_capacity(except_source_cidrs.len());
    for cidr in except_source_cidrs {
        *cidr = crate::forward_validation::normalize_cidr(cidr).ok_or_else(|| {
            invalid_route(
                service,
                route_index,
                field,
                format!("invalid canonical source exception CIDR `{cidr}`"),
            )
        })?;
        if !unique.insert(cidr.clone()) {
            return Err(invalid_route(
                service,
                route_index,
                field,
                format!("duplicate source exception CIDR `{cidr}`"),
            ));
        }
    }
    Ok(())
}

fn is_pingora_managed_upgrade_mutation(mutation: &HttpRequestHeaderMutation) -> bool {
    matches!(
        mutation,
        HttpRequestHeaderMutation::Set {
            name,
            value: HttpRequestHeaderValue::IncomingHeader { name: source, .. },
        } if name == "upgrade" && source == "upgrade"
    ) || matches!(
        mutation,
        HttpRequestHeaderMutation::Set {
            name,
            value: HttpRequestHeaderValue::Literal { value },
        } if name == "connection" && value.eq_ignore_ascii_case("upgrade")
    )
}

fn validate_dynamic_header_bound(max_bytes: u64) -> Result<(), &'static str> {
    if max_bytes == 0 || max_bytes > MAX_HTTP_HEADER_VALUE_BYTES as u64 {
        return Err("dynamic header max_bytes must be between 1 and 8192");
    }
    Ok(())
}

pub(super) fn validate_response_mutations(
    service: &str,
    route_index: usize,
    mutations: &mut [HttpResponseHeaderMutation],
) -> Result<(), ConfigError> {
    if mutations.len() > MAX_HTTP_HEADER_MUTATIONS {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.response_headers",
            "must contain at most 32 header mutations",
        ));
    }
    let mut operations = HashMap::with_capacity(mutations.len());
    for mutation in mutations {
        normalize_header_name(mutation.name_mut()).map_err(|detail| {
            invalid_route(
                service,
                route_index,
                "action.policy.response_headers",
                detail,
            )
        })?;
        let operation = response_mutation_kind(mutation);
        if let Some(previous) = operations.get(mutation.name()) {
            let compatible = matches!(
                (*previous, operation),
                (
                    ResponseMutationKind::Add | ResponseMutationKind::Remove,
                    ResponseMutationKind::Add,
                )
            );
            if !compatible {
                return Err(invalid_route(
                    service,
                    route_index,
                    "action.policy.response_headers",
                    format!(
                        "contains duplicate or conflicting mutation for `{}`",
                        mutation.name()
                    ),
                ));
            }
        } else {
            operations.insert(mutation.name().to_owned(), operation);
        }
        if let HttpResponseHeaderMutation::Set { value, .. }
        | HttpResponseHeaderMutation::Add { value, .. } = mutation
        {
            validate_header_value(value).map_err(|detail| {
                invalid_route(
                    service,
                    route_index,
                    "action.policy.response_headers",
                    detail,
                )
            })?;
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum ResponseMutationKind {
    Set,
    Add,
    Remove,
}

fn response_mutation_kind(mutation: &HttpResponseHeaderMutation) -> ResponseMutationKind {
    match mutation {
        HttpResponseHeaderMutation::Set { .. } => ResponseMutationKind::Set,
        HttpResponseHeaderMutation::Add { .. } => ResponseMutationKind::Add,
        HttpResponseHeaderMutation::Remove { .. } => ResponseMutationKind::Remove,
    }
}

pub(crate) fn normalize_header_name(name: &mut String) -> Result<(), &'static str> {
    normalize_header_name_syntax(name)?;
    if is_forbidden_header(name) {
        return Err("header is hop-by-hop, framing, or managed by upstream Host policy");
    }
    Ok(())
}

fn normalize_header_name_syntax(name: &mut String) -> Result<(), &'static str> {
    if name.is_empty() || name.len() > MAX_HTTP_HEADER_NAME_BYTES {
        return Err("header name must be 1..=64 bytes");
    }
    let header = HeaderName::from_bytes(name.as_bytes()).map_err(|_| "header name is invalid")?;
    let normalized = header.as_str();
    *name = normalized.into();
    Ok(())
}

fn is_forbidden_header(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "content-length"
            | "host"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "proxy-connection"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

pub(super) fn validate_header_value(value: &str) -> Result<(), &'static str> {
    if value.len() > MAX_HTTP_HEADER_VALUE_BYTES {
        return Err("header value exceeds 8192 bytes");
    }
    HeaderValue::from_str(value).map_err(|_| "header value contains invalid bytes")?;
    Ok(())
}

pub(super) fn validate_cookie_rewrites(
    service: &str,
    route_index: usize,
    rewrites: &[HttpCookiePathRewrite],
) -> Result<(), ConfigError> {
    if rewrites.len() > MAX_HTTP_COOKIE_PATH_REWRITES {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.response_cookie_path_rewrites",
            "must contain at most 16 rewrites",
        ));
    }
    let mut from_paths = HashSet::with_capacity(rewrites.len());
    for rewrite in rewrites {
        validate_cookie_path(&rewrite.from).map_err(|detail| {
            invalid_route(
                service,
                route_index,
                "action.policy.response_cookie_path_rewrites",
                detail,
            )
        })?;
        validate_cookie_path(&rewrite.to).map_err(|detail| {
            invalid_route(
                service,
                route_index,
                "action.policy.response_cookie_path_rewrites",
                detail,
            )
        })?;
        if !from_paths.insert(rewrite.from.as_str()) {
            return Err(invalid_route(
                service,
                route_index,
                "action.policy.response_cookie_path_rewrites",
                format!("contains duplicate source path `{}`", rewrite.from),
            ));
        }
    }
    Ok(())
}

pub(super) fn validate_cookie_attributes(
    service: &str,
    route_index: usize,
    policies: &mut [HttpCookieAttributePolicy],
) -> Result<(), ConfigError> {
    if policies.len() > MAX_HTTP_COOKIE_ATTRIBUTE_RULES {
        return Err(invalid_route(
            service,
            route_index,
            "action.policy.response_cookie_attributes",
            "must contain at most 16 policies",
        ));
    }
    let mut names = HashSet::with_capacity(policies.len());
    for policy in policies {
        if policy.name.is_empty()
            || policy.name.len() > 256
            || policy
                .name
                .bytes()
                .any(|byte| byte.is_ascii_control() || matches!(byte, b'=' | b';' | b','))
        {
            return Err(invalid_route(
                service,
                route_index,
                "action.policy.response_cookie_attributes",
                "cookie name must be 1..=256 bytes without separators or controls",
            ));
        }
        if policy.secure.is_none() && policy.http_only.is_none() && policy.same_site.is_none() {
            return Err(invalid_route(
                service,
                route_index,
                "action.policy.response_cookie_attributes",
                "each policy must set at least one attribute",
            ));
        }
        if !names.insert(policy.name.clone()) {
            return Err(invalid_route(
                service,
                route_index,
                "action.policy.response_cookie_attributes",
                "must not contain duplicate cookie names",
            ));
        }
    }
    Ok(())
}

fn validate_cookie_path(path: &str) -> Result<(), &'static str> {
    if path.is_empty() || path.len() > MAX_HTTP_COOKIE_PATH_BYTES || !path.starts_with('/') {
        return Err("cookie path must be an absolute value of at most 1024 bytes");
    }
    if path
        .bytes()
        .any(|byte| byte == 0 || byte == b';' || byte.is_ascii_control())
    {
        return Err("cookie path contains an invalid byte");
    }
    Ok(())
}
