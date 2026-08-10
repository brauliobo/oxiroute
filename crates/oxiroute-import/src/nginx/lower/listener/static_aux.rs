impl Lowerer {
    fn lower_try_files(
        &self,
        location: &EffectiveLocation,
        origins: &mut Vec<DirectiveOrigin>,
        issues: &mut Vec<LowerIssue>,
    ) -> Vec<HttpStaticTryFile> {
        let mut lowered = Vec::new();
        for policy in self.effective_list_policy_chain(location.origin.occurrence, b"try_files") {
            origins.extend(policy.origins.clone());
            let last = policy.arguments.len().saturating_sub(1);
            for (index, value) in policy.arguments.into_iter().enumerate() {
                if index == last && !value.starts_with(b"=") {
                    issues.push(issue(
                        policy.origins.last().unwrap_or(&location.origin),
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "nginx try_files URI fallback requires authenticated internal rerouting",
                    ));
                    continue;
                }
                let item = match value.as_slice() {
                    b"$uri" => Some(HttpStaticTryFile::RequestPath),
                    b"$uri/" => Some(HttpStaticTryFile::RequestPathDirectory),
                    value if value.starts_with(b"=") => utf8(&value[1..])
                        .and_then(|status| status.parse::<u16>().ok())
                        .map(|status| HttpStaticTryFile::Status { status }),
                    value if !value.contains(&b'$') => {
                        utf8(value).map(|path| HttpStaticTryFile::Relative {
                            path: PathBuf::from(path.trim_start_matches('/')),
                        })
                    }
                    _ => None,
                };
                if let Some(item) = item {
                    lowered.push(item);
                } else {
                    issues.push(issue(
                        policy.origins.last().unwrap_or(&location.origin),
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "nginx try_files entry is outside the canonical static lookup subset",
                    ));
                }
            }
        }
        lowered
    }

    fn lower_static_mime(
        &self,
        location: &EffectiveLocation,
        origins: &mut Vec<DirectiveOrigin>,
        issues: &mut Vec<LowerIssue>,
    ) -> HttpStaticMimePolicy {
        let default_type = self
            .effective_policy(location.origin.occurrence, b"default_type")
            .and_then(|policy| {
                origins.extend(policy.origins.clone());
                policy
                    .arguments
                    .first()
                    .and_then(|value| utf8(value))
                    .map(str::to_owned)
            })
            .or_else(|| Some("text/plain".to_owned()));
        let mut types = Vec::<HttpMimeType>::new();
        let Some(types_occurrence) = self.effective_types_occurrence(location.origin.occurrence)
        else {
            return HttpStaticMimePolicy {
                default_type,
                types,
            };
        };
        origins.push(self.origin(types_occurrence));
        for mapping in self
            .graph
            .expanded_occurrences
            .iter()
            .filter(|occurrence| occurrence.parent == Some(types_occurrence))
        {
            let Some(content_type) = utf8(&mapping.directive.name.value) else {
                issues.push(issue(
                    &self.origin(mapping.id),
                    E_INVALID_VALUE,
                    "nginx MIME content type is not UTF-8",
                ));
                continue;
            };
            origins.push(self.origin(mapping.id));
            for extension in &mapping.directive.arguments {
                let Some(extension) = utf8(&extension.value) else {
                    issues.push(issue(
                        &self.origin(mapping.id),
                        E_INVALID_VALUE,
                        "nginx MIME extension is not UTF-8",
                    ));
                    continue;
                };
                let entry = HttpMimeType {
                    extension: extension.to_ascii_lowercase(),
                    content_type: content_type.to_owned(),
                };
                if let Some(existing) = types
                    .iter_mut()
                    .find(|candidate| candidate.extension == entry.extension)
                {
                    *existing = entry;
                } else {
                    types.push(entry);
                }
            }
        }
        HttpStaticMimePolicy {
            default_type,
            types,
        }
    }

    fn effective_types_occurrence(&self, scope: OccurrenceId) -> Option<OccurrenceId> {
        let mut current = Some(scope);
        while let Some(scope) = current {
            if let Some(types) = self
                .graph
                .expanded_occurrences
                .iter()
                .rev()
                .find(|occurrence| {
                    occurrence.parent == Some(scope) && occurrence.directive.name.value == b"types"
                })
            {
                return Some(types.id);
            }
            current = self
                .occurrence(scope)
                .and_then(|occurrence| occurrence.parent);
        }
        None
    }

    fn lower_literal_headers(
        &self,
        location: &EffectiveLocation,
        origins: &mut Vec<DirectiveOrigin>,
        issues: &mut Vec<LowerIssue>,
    ) -> Vec<HttpLiteralHeader> {
        let mut headers = Vec::new();
        for policy in self.effective_list_policy_chain(location.origin.occurrence, b"add_header") {
            origins.extend(policy.origins.clone());
            let ([name, value] | [name, value, _]) = policy.arguments.as_slice() else {
                continue;
            };
            let always = policy
                .arguments
                .get(2)
                .is_some_and(|value| value == b"always");
            if policy.arguments.len() == 3 && !always {
                issues.push(issue(
                    policy.origins.last().unwrap_or(&location.origin),
                    E_INVALID_VALUE,
                    "nginx add_header third argument must be always",
                ));
                continue;
            }
            let (Some(name), Some(value)) = (utf8(name), utf8(value)) else {
                issues.push(issue(
                    policy.origins.last().unwrap_or(&location.origin),
                    E_INVALID_VALUE,
                    "nginx static response header is not UTF-8",
                ));
                continue;
            };
            if let Err(message) = validate_literal_response_header(name, value) {
                issues.push(issue(
                    policy.origins.last().unwrap_or(&location.origin),
                    E_INVALID_VALUE,
                    message,
                ));
                continue;
            }
            if headers.len() >= NGINX_MAX_LITERAL_HEADERS {
                issues.push(issue(
                    policy.origins.last().unwrap_or(&location.origin),
                    E_INVALID_VALUE,
                    "nginx add_header declarations exceed the canonical response-header bound",
                ));
                continue;
            }
            headers.push(HttpLiteralHeader {
                name: name.into(),
                value: value.into(),
                always,
            });
        }
        headers
    }

    fn lower_error_responses(
        &self,
        location: &EffectiveLocation,
        origins: &mut Vec<DirectiveOrigin>,
        issues: &mut Vec<LowerIssue>,
    ) -> Vec<HttpStaticErrorResponse> {
        let mut responses = Vec::new();
        let mut seen_statuses = HashSet::new();
        for policy in self.effective_list_policy_chain(location.origin.occurrence, b"error_page") {
            origins.extend(policy.origins.clone());
            let origin = policy.origins.last().unwrap_or(&location.origin);
            let Some(file) = policy.arguments.last().and_then(|value| utf8(value)) else {
                issues.push(issue(
                    origin,
                    E_INVALID_VALUE,
                    "nginx error_page target must be UTF-8",
                ));
                continue;
            };
            let statuses = policy.arguments[..policy.arguments.len().saturating_sub(1)]
                .iter()
                .map(|value| {
                    utf8(value)
                        .and_then(|value| value.parse::<u16>().ok())
                        .filter(|status| (400..=599).contains(status))
                })
                .collect::<Option<Vec<_>>>();
            let statuses_valid = statuses.as_ref().is_some_and(|statuses| {
                let mut local = HashSet::new();
                !statuses.is_empty()
                    && statuses.len() <= NGINX_MAX_STATIC_ERROR_STATUSES
                    && statuses.iter().all(|status| local.insert(*status))
            });
            let canonical_file = canonicalize_http_path(file)
                .filter(|canonical| canonical.as_ref() == file)
                .filter(|_| file.starts_with('/'))
                .filter(|_| file.len() <= NGINX_MAX_STATIC_ERROR_TARGET_BYTES);
            let (Some(statuses), Some(canonical_file)) = (statuses, canonical_file) else {
                issues.push(issue(
                    origin,
                    E_INVALID_VALUE,
                    "nginx error_page requires a bounded 400..=599 status list and an absolute canonical URI target",
                ));
                continue;
            };
            if !statuses_valid || statuses.iter().any(|status| seen_statuses.contains(status)) {
                issues.push(issue(
                    origin,
                    E_INVALID_VALUE,
                    "nginx error_page statuses must be unique bounded 400..=599 values",
                ));
                continue;
            }
            if responses.len() >= NGINX_MAX_STATIC_ERROR_RESPONSES {
                issues.push(issue(
                    origin,
                    E_INVALID_VALUE,
                    "nginx error_page declarations exceed the canonical response bound",
                ));
                continue;
            }
            seen_statuses.extend(statuses.iter().copied());
            responses.push(HttpStaticErrorResponse {
                statuses,
                file: Some(PathBuf::from(canonical_file.trim_start_matches('/'))),
                body: None,
                headers: Vec::new(),
                internal_redirect: Some(canonical_file.into_owned()),
            });
        }
        responses
    }

    fn error_page_matches_status(&self, scope: OccurrenceId, status: u16) -> bool {
        self.effective_list_policy_chain(scope, b"error_page")
            .iter()
            .any(|policy| {
                policy.arguments[..policy.arguments.len().saturating_sub(1)]
                    .iter()
                    .any(|value| {
                        utf8(value).and_then(|value| value.parse::<u16>().ok()) == Some(status)
                    })
            })
    }

    fn policy_enabled(&self, scope: OccurrenceId, name: &[u8], default: bool) -> bool {
        self.effective_policy(scope, name)
            .and_then(|policy| policy.arguments.first().cloned())
            .map_or(default, |value| value == b"on")
    }
}

fn validate_literal_response_header(name: &str, value: &str) -> Result<(), &'static str> {
    if name.is_empty() || name.len() > NGINX_MAX_HEADER_NAME_BYTES {
        return Err("nginx response header name must be 1..=64 bytes");
    }
    HeaderName::from_bytes(name.as_bytes()).map_err(|_| "nginx response header name is invalid")?;
    if matches!(
        name.to_ascii_lowercase().as_str(),
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
    ) {
        return Err("nginx response header is hop-by-hop, framing, or request-managed");
    }
    if value.len() > NGINX_MAX_HEADER_VALUE_BYTES {
        return Err("nginx response header value exceeds 8192 bytes");
    }
    HeaderValue::from_str(value)
        .map_err(|_| "nginx response header value contains invalid bytes")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_nginx_time_uses_seconds() {
        assert_eq!(parse_duration_ms(b"600"), Some(600_000));
        assert_eq!(parse_duration_ms(b"600ms"), Some(600));
        assert_eq!(parse_duration_ms(b"10m"), Some(600_000));
        assert_eq!(parse_duration_ms(b"1h30m"), Some(5_400_000));
        assert_eq!(parse_duration_ms(b"1m500ms"), Some(60_500));
        assert_eq!(parse_duration_ms(b"1h30"), Some(3_630_000));
        assert_eq!(parse_duration_ms(b"1h 30"), Some(3_630_000));
        assert_eq!(parse_duration_ms(b"1y"), None);
        assert_eq!(parse_duration_ms(b"1M"), None);
        assert_eq!(parse_duration_ms(b"1m1h"), None);
    }
}

fn nginx_default_404(server: &str) -> HttpStaticErrorResponse {
    HttpStaticErrorResponse {
        statuses: vec![404],
        file: None,
        body: Some(nginx_error_body(404, "Not Found", server)),
        headers: nginx_default_headers(server),
        internal_redirect: None,
    }
}

fn nginx_error_body(status: u16, reason: &str, server: &str) -> String {
    format!(
        "<html>\r\n<head><title>{status} {reason}</title></head>\r\n<body>\r\n<center><h1>{status} {reason}</h1></center>\r\n<hr><center>{server}</center>\r\n</body>\r\n</html>\r\n"
    )
}

fn nginx_default_headers(server: &str) -> Vec<HttpLiteralHeader> {
    vec![
        HttpLiteralHeader {
            name: "server".into(),
            value: server.into(),
            always: true,
        },
        HttpLiteralHeader {
            name: "content-type".into(),
            value: "text/html".into(),
            always: true,
        },
    ]
}

fn canonical_proxy_endpoint(
    endpoint: &StaticEndpoint,
    proxy: &crate::nginx::EffectiveProxyPass,
    secure: bool,
) -> oxiroute_config::UpstreamEndpoint {
    let mut endpoint = canonical_endpoint(endpoint);
    if proxy.scheme == ProxyPassScheme::Downstream
        && secure
        && !authority_has_explicit_port(&proxy.authority)
    {
        match &mut endpoint {
            oxiroute_config::UpstreamEndpoint::Socket { address } => address.set_port(443),
            oxiroute_config::UpstreamEndpoint::Dns { port, .. } => *port = 443,
            oxiroute_config::UpstreamEndpoint::Unix { .. } => {}
        }
    }
    endpoint
}

#[allow(clippy::naive_bytecount)]
fn authority_has_explicit_port(authority: &[u8]) -> bool {
    if authority.starts_with(b"[") {
        return authority
            .iter()
            .position(|byte| *byte == b']')
            .is_some_and(|end| authority.get(end + 1) == Some(&b':'));
    }
    authority.iter().filter(|byte| **byte == b':').count() == 1
}

#[derive(Clone)]
struct LoweredRoute {
    route: HttpRoute,
    pool: Option<PoolCandidate>,
    origins: Vec<DirectiveOrigin>,
}

struct LoweredLocation {
    routes: Vec<LoweredRoute>,
}

struct LoweredProxy {
    pool: PoolCandidate,
    policy: HttpProxyPolicy,
    timeouts: ProxyTimeouts,
    origins: Vec<DirectiveOrigin>,
}

#[derive(Clone, Copy)]
struct ProxyTimeouts {
    connect: u64,
    read: u64,
    write: u64,
}

fn canonical_directory(value: &[u8]) -> Option<PathBuf> {
    let value = utf8(value)?;
    let normalized = if value == "/" {
        value
    } else {
        value.trim_end_matches('/')
    };
    let path = Path::new(normalized);
    (path.is_absolute()
        && !value.contains("//")
        && !value.split('/').any(|part| matches!(part, "." | "..")))
    .then(|| path.to_path_buf())
}
