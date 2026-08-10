impl Lowerer {
    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "proxy admission, policy, pool, and provenance form one atomic route action"
    )]
    fn lower_proxy(
        &self,
        http: &EffectiveHttp,
        location: &EffectiveLocation,
        proxy: &crate::nginx::EffectiveProxyPass,
        service_name: &str,
        http_index: usize,
        route_ordinal: usize,
        downstream_tls: bool,
        nginx_host_fallback: Option<&str>,
        issues: &mut Vec<LowerIssue>,
    ) -> Option<LoweredProxy> {
        if let Some(error_page) = self
            .effective_list_policy_chain(location.origin.occurrence, b"error_page")
            .last()
        {
            issues.push(issue(
                error_page.origins.last().unwrap_or(&location.origin),
                E_SEMANTICS_NOT_REPRESENTABLE,
                "nginx proxy error_page handling is not represented for generated proxy errors",
            ));
        }
        let upstream_path_rewrite = if let Some(replacement) = proxy.replacement_uri.as_deref() {
            let Some(from) = location.path.as_ref().and_then(|path| utf8(&path.value)) else {
                issues.push(issue(
                    &proxy.origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "proxy_pass URI replacement requires a canonical location path",
                ));
                return None;
            };
            let Some(to) = utf8(replacement) else {
                issues.push(issue(
                    &proxy.origin,
                    E_INVALID_VALUE,
                    "proxy_pass URI replacement is not UTF-8",
                ));
                return None;
            };
            let valid = to.starts_with('/')
                && to
                    .parse::<http::uri::PathAndQuery>()
                    .is_ok_and(|parsed| parsed.query().is_none() && parsed.path() == to);
            let Some(to) = valid
                .then(|| canonicalize_http_path(to))
                .flatten()
                .map(std::borrow::Cow::into_owned)
            else {
                issues.push(issue(
                    &proxy.origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "proxy_pass URI replacement must be a canonical absolute path",
                ));
                return None;
            };
            Some(HttpProxyPathRewrite {
                from: from.into(),
                to,
            })
        } else {
            None
        };
        let upstream_tls = match self.validate_proxy_origin(http, proxy, downstream_tls) {
            Ok(tls) => tls,
            Err(origin_issues) => {
                issues.extend(origin_issues);
                None
            }
        };
        if let Err(policy_issues) =
            self.validate_route_policy(location.origin.occurrence, &location.origin)
        {
            issues.extend(policy_issues);
        }
        let pool = Self::proxy_pool(
            http,
            proxy,
            service_name,
            http_index,
            route_ordinal,
            upstream_tls,
            issues,
        )?;
        let (upstream_host, request_headers) =
            self.lower_proxy_headers(location, proxy, nginx_host_fallback, issues);
        self.validate_response_controls(location, proxy, issues);
        let mut response_header_origins = Vec::new();
        let mut response_headers =
            self.lower_response_headers(location, &mut response_header_origins, issues);
        if let Some(server) = self.default_error_server.as_deref() {
            response_headers.extend([
                HttpResponseHeaderMutation::Set {
                    name: "server".into(),
                    value: server.into(),
                    always: true,
                },
                HttpResponseHeaderMutation::Set {
                    name: "content-type".into(),
                    value: "text/html".into(),
                    always: true,
                },
            ]);
            self.used_default_error_overlay.set(true);
        }
        let response_cookie_path_rewrites = self.lower_cookie_rewrites(location, issues);
        let response_cookie_attributes = self.lower_cookie_attributes(location, issues);
        let retry = self.lower_proxy_retry(location, pool.pool.servers.len(), issues);
        let timeouts = self.proxy_timeouts(location.origin.occurrence, &location.origin, issues);
        if !issues.is_empty() {
            return None;
        }
        let mut origins = vec![proxy.origin.clone(), location.origin.clone()];
        origins.extend(response_header_origins);
        for name in [
            b"client_max_body_size".as_slice(),
            b"proxy_connect_timeout".as_slice(),
            b"proxy_read_timeout".as_slice(),
            b"proxy_send_timeout".as_slice(),
            b"proxy_http_version".as_slice(),
            b"proxy_buffering".as_slice(),
            b"proxy_request_buffering".as_slice(),
            b"proxy_set_header".as_slice(),
            b"proxy_hide_header".as_slice(),
            b"proxy_pass_header".as_slice(),
            b"proxy_ignore_headers".as_slice(),
            b"proxy_cookie_path".as_slice(),
            b"proxy_cookie_flags".as_slice(),
            b"proxy_next_upstream".as_slice(),
            b"proxy_next_upstream_tries".as_slice(),
        ] {
            for value in self.effective_list_policy_chain(location.origin.occurrence, name) {
                origins.extend(value.origins);
            }
        }
        match proxy.upstream {
            crate::nginx::UpstreamReference::Resolved(occurrence) => {
                if let Some(upstream) = http
                    .upstreams
                    .iter()
                    .find(|upstream| upstream.origin.occurrence == occurrence)
                {
                    origins.push(upstream.origin.clone());
                    origins.extend(upstream.servers.iter().map(|server| server.origin.clone()));
                }
            }
            crate::nginx::UpstreamReference::Direct
            | crate::nginx::UpstreamReference::Unresolved
            | crate::nginx::UpstreamReference::Variable => {}
        }
        Some(LoweredProxy {
            pool,
            policy: HttpProxyPolicy {
                upstream_host,
                upstream_path_rewrite,
                request_headers,
                response_headers,
                response_cookie_path_rewrites,
                response_cookie_attributes,
                retry,
                cache: None,
            },
            timeouts,
            origins,
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "endpoint, weighted policy, pool construction, and provenance stay atomic"
    )]
    fn proxy_pool(
        http: &EffectiveHttp,
        proxy: &crate::nginx::EffectiveProxyPass,
        service_name: &str,
        http_index: usize,
        route_ordinal: usize,
        upstream_tls: Option<UpstreamTls>,
        issues: &mut Vec<LowerIssue>,
    ) -> Option<PoolCandidate> {
        let (endpoints, origin, endpoint_origins, weight_origins, algorithm) =
            match proxy.upstream {
                crate::nginx::UpstreamReference::Direct => (
                    proxy
                        .direct_endpoint
                        .as_ref()
                        .map(|endpoint| {
                            canonical_proxy_endpoint(endpoint, proxy, upstream_tls.is_some())
                        })
                        .into_iter()
                        .collect(),
                    proxy.origin.clone(),
                    vec![proxy.origin.clone()],
                    None,
                    UpstreamAlgorithm::RoundRobin,
                ),
                crate::nginx::UpstreamReference::Resolved(occurrence) => {
                    let upstream = http
                        .upstreams
                        .iter()
                        .find(|upstream| upstream.origin.occurrence == occurrence)
                        .expect("resolved upstream occurrence is retained");
                    let has_weights = upstream
                        .servers
                        .iter()
                        .any(|server| server.weight.is_some());
                    let mut endpoints = Vec::new();
                    let mut endpoint_origins = Vec::new();
                    let mut weight_origins = has_weights.then(Vec::new);
                    for server in &upstream.servers {
                        let Some(endpoint) = server.endpoint.as_ref() else {
                            continue;
                        };
                        endpoints.push(canonical_endpoint(endpoint));
                        endpoint_origins.push(server.origin.clone());
                        if let Some(origins) = &mut weight_origins {
                            origins.push(server.weight.as_ref().map_or_else(
                                || server.origin.clone(),
                                |weight| weight.origin.clone(),
                            ));
                        }
                    }
                    (
                        endpoints,
                        upstream.origin.clone(),
                        endpoint_origins,
                        weight_origins,
                        if has_weights {
                            UpstreamAlgorithm::WeightedRoundRobin {
                                weights: upstream
                                    .servers
                                    .iter()
                                    .filter_map(|server| {
                                        server.endpoint.as_ref().map(|_| {
                                            server.weight.as_ref().map_or(1, |weight| weight.value)
                                        })
                                    })
                                    .collect(),
                            }
                        } else {
                            UpstreamAlgorithm::RoundRobin
                        },
                    )
                }
                crate::nginx::UpstreamReference::Unresolved
                | crate::nginx::UpstreamReference::Variable => (
                    Vec::new(),
                    proxy.origin.clone(),
                    Vec::new(),
                    None,
                    UpstreamAlgorithm::RoundRobin,
                ),
            };
        if endpoints.is_empty() {
            issues.push(issue(
                &proxy.origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "nginx proxy origin has no canonical endpoint set",
            ));
            return None;
        }
        let name = match proxy.upstream {
            crate::nginx::UpstreamReference::Resolved(occurrence) => {
                let upstream_index = http
                    .upstreams
                    .iter()
                    .position(|upstream| upstream.origin.occurrence == occurrence)
                    .expect("resolved upstream occurrence is retained");
                format!("nginx-http-upstream-{http_index}-{upstream_index}")
            }
            crate::nginx::UpstreamReference::Direct
            | crate::nginx::UpstreamReference::Unresolved
            | crate::nginx::UpstreamReference::Variable => {
                format!("{service_name}-pool-{route_ordinal}")
            }
        };
        let servers = endpoints
            .into_iter()
            .enumerate()
            .map(|(index, endpoint)| {
                let dns_resolution = if matches!(endpoint, UpstreamEndpoint::Dns { .. }) {
                    DnsResolutionPolicy::Startup
                } else {
                    DnsResolutionPolicy::OnConnect
                };
                UpstreamServer {
                    name: format!("endpoint-{}", index + 1),
                    endpoint,
                    max_connections: None,
                    dns_resolution,
                }
            })
            .collect();
        Some(PoolCandidate {
            pool: UpstreamPool {
                name,
                servers,
                endpoints: Vec::new(),
                algorithm,
                health_check: None,
                passive_health: None,
                tls: upstream_tls,
                http_versions: HttpVersionPolicy::default(),
                queue_timeout_ms: None,
                connect_timeout_ms: None,
                server_timeout_ms: None,
                connection_reuse: UpstreamConnectionReuse::Never,
            },
            origin,
            endpoint_origins,
            weight_origins,
        })
    }

    fn lower_proxy_headers(
        &self,
        location: &EffectiveLocation,
        proxy: &crate::nginx::EffectiveProxyPass,
        nginx_host_fallback: Option<&str>,
        issues: &mut Vec<LowerIssue>,
    ) -> (HttpUpstreamHost, Vec<HttpRequestHeaderMutation>) {
        let policies =
            self.effective_list_policy_chain(location.origin.occurrence, b"proxy_set_header");
        let mut host = None;
        let mut headers = Vec::new();
        for policy in policies {
            let [name, value] = policy.arguments.as_slice() else {
                continue;
            };
            let origin = policy.origins.last().unwrap_or(&location.origin);
            let Some(name) = utf8(name) else {
                issues.push(issue(
                    origin,
                    E_INVALID_VALUE,
                    "proxy_set_header name is not UTF-8",
                ));
                continue;
            };
            if name.eq_ignore_ascii_case("host") {
                if host.is_some() {
                    issues.push(issue(
                        origin,
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "proxy_set_header defines Host more than once",
                    ));
                    continue;
                }
                host = Self::proxy_value(value, proxy, nginx_host_fallback)
                    .and_then(NginxProxyValue::into_upstream_host);
                if host.is_none() {
                    issues.push(issue(
                        origin,
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "proxy_set_header Host value has no canonical upstream Host policy",
                    ));
                }
                continue;
            }
            let mutation = if value.is_empty() {
                Some(HttpRequestHeaderMutation::Remove { name: name.into() })
            } else {
                Self::proxy_value(value, proxy, nginx_host_fallback).map(|value| {
                    HttpRequestHeaderMutation::Set {
                        name: name.into(),
                        value: value.into_request_header_value(),
                    }
                })
            };
            if let Some(mutation) = mutation {
                headers.push(mutation);
            } else {
                issues.push(issue(
                    origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "proxy_set_header value uses an unsupported nginx variable or template",
                ));
            }
        }
        let default_host = Self::proxy_value(b"$proxy_host", proxy, nginx_host_fallback)
            .and_then(NginxProxyValue::into_upstream_host)
            .expect("validated proxy authority is a canonical Host value");
        (host.unwrap_or(default_host), headers)
    }

    fn proxy_value(
        value: &[u8],
        proxy: &crate::nginx::EffectiveProxyPass,
        nginx_host_fallback: Option<&str>,
    ) -> Option<NginxProxyValue> {
        match value {
            b"$http_host" => Some(NginxProxyValue::IncomingAuthority),
            b"$host" => nginx_host_fallback
                .map(str::to_owned)
                .map(NginxProxyValue::NginxHost),
            b"$remote_addr" => Some(NginxProxyValue::ClientIp),
            b"$proxy_add_x_forwarded_for" => Some(NginxProxyValue::AppendedXForwardedFor),
            b"$scheme" => Some(NginxProxyValue::DownstreamScheme),
            b"$http_upgrade" => Some(NginxProxyValue::IncomingUpgrade),
            b"$proxy_host" => utf8(&proxy.authority)
                .map(str::to_owned)
                .map(NginxProxyValue::Literal),
            value if !value.contains(&b'$') => {
                utf8(value).map(str::to_owned).map(NginxProxyValue::Literal)
            }
            _ => None,
        }
    }

    fn lower_response_headers(
        &self,
        location: &EffectiveLocation,
        origins: &mut Vec<DirectiveOrigin>,
        issues: &mut Vec<LowerIssue>,
    ) -> Vec<HttpResponseHeaderMutation> {
        let mut hidden = NGINX_DEFAULT_HIDDEN_RESPONSE_HEADERS
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        if self.default_error_server.is_some() {
            hidden.retain(|name| !name.eq_ignore_ascii_case("server"));
        }
        for policy in
            self.effective_list_policy_chain(location.origin.occurrence, b"proxy_hide_header")
        {
            let Some(name) = policy.arguments.first().and_then(|value| utf8(value)) else {
                issues.push(issue(
                    policy.origins.last().unwrap_or(&location.origin),
                    E_INVALID_VALUE,
                    "proxy_hide_header name is not UTF-8",
                ));
                continue;
            };
            if !hidden
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(name))
            {
                hidden.push(name.to_owned());
            }
        }
        for policy in
            self.effective_list_policy_chain(location.origin.occurrence, b"proxy_pass_header")
        {
            let Some(name) = policy.arguments.first().and_then(|value| utf8(value)) else {
                issues.push(issue(
                    policy.origins.last().unwrap_or(&location.origin),
                    E_INVALID_VALUE,
                    "proxy_pass_header name is not UTF-8",
                ));
                continue;
            };
            if name.eq_ignore_ascii_case("date") {
                issues.push(issue(
                    policy.origins.last().unwrap_or(&location.origin),
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "proxy_pass_header Date cannot be represented because Pingora replaces every downstream Date header",
                ));
                continue;
            }
            hidden.retain(|candidate| !candidate.eq_ignore_ascii_case(name));
        }
        let mut mutations = hidden
            .into_iter()
            .map(|name| HttpResponseHeaderMutation::Remove { name })
            .collect::<Vec<_>>();
        for header in self.lower_literal_headers(location, origins, issues) {
            mutations.push(HttpResponseHeaderMutation::Add {
                name: header.name,
                value: header.value,
                always: header.always,
            });
        }
        mutations
    }

    fn validate_response_controls(
        &self,
        location: &EffectiveLocation,
        proxy: &crate::nginx::EffectiveProxyPass,
        issues: &mut Vec<LowerIssue>,
    ) {
        let policies =
            self.effective_list_policy_chain(location.origin.occurrence, b"proxy_ignore_headers");
        if policies.is_empty() {
            if self.x_accel_controls_absent {
                return;
            }
            issues.push(issue(
                &proxy.origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "proxy_ignore_headers must explicitly disable every nginx X-Accel response control",
            ));
            return;
        }
        let ignored = policies
            .iter()
            .flat_map(|policy| &policy.arguments)
            .collect::<Vec<_>>();
        if !NGINX_RESPONSE_CONTROL_HEADERS.iter().all(|required| {
            ignored
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(required))
        }) {
            issues.push(issue(
                policies
                    .last()
                    .and_then(|policy| policy.origins.last())
                    .unwrap_or(&proxy.origin),
                E_SEMANTICS_NOT_REPRESENTABLE,
                "explicit proxy_ignore_headers omits an nginx X-Accel response control",
            ));
        }
    }

    fn lower_cookie_rewrites(
        &self,
        location: &EffectiveLocation,
        issues: &mut Vec<LowerIssue>,
    ) -> Vec<HttpCookiePathRewrite> {
        self.effective_list_policy_chain(location.origin.occurrence, b"proxy_cookie_path")
            .into_iter()
            .filter_map(|policy| {
                let [from, to] = policy.arguments.as_slice() else { return None };
                let literal = !from.contains(&b'$') && !to.contains(&b'$');
                let pair = utf8(from).zip(utf8(to));
                if !literal || pair.is_none() {
                    issues.push(issue(policy.origins.last().unwrap_or(&location.origin), E_SEMANTICS_NOT_REPRESENTABLE, "proxy_cookie_path regex, variables, and non-UTF-8 values are not canonical rewrites"));
                    return None;
                }
                let (from, to) = pair.expect("checked cookie paths");
                let to = to.split(';').next().unwrap_or(to).trim_end();
                Some(HttpCookiePathRewrite { from: from.into(), to: to.into() })
            })
            .collect()
    }

    fn lower_cookie_attributes(
        &self,
        location: &EffectiveLocation,
        issues: &mut Vec<LowerIssue>,
    ) -> Vec<HttpCookieAttributePolicy> {
        let mut attributes = Vec::new();
        let mut names = HashSet::new();
        for policy in
            self.effective_list_policy_chain(location.origin.occurrence, b"proxy_cookie_flags")
        {
            let origin = policy.origins.last().unwrap_or(&location.origin);
            let Some((name, flags)) = policy.arguments.split_first() else {
                continue;
            };
            let Some(name) = utf8(name) else {
                issues.push(issue(
                    origin,
                    E_INVALID_VALUE,
                    "proxy_cookie_flags cookie name is not UTF-8",
                ));
                continue;
            };
            if name.is_empty() || name.starts_with('~') || name.contains('$') {
                issues.push(issue(
                    origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "proxy_cookie_flags requires an exact static cookie name",
                ));
                continue;
            }

            let mut secure = None;
            let mut http_only = None;
            let mut same_site = None;
            let mut valid = true;
            for flag in flags {
                match flag.as_slice() {
                    b"secure" if secure.is_none() => secure = Some(true),
                    b"nosecure" if secure.is_none() => secure = Some(false),
                    b"httponly" if http_only.is_none() => http_only = Some(true),
                    b"nohttponly" if http_only.is_none() => http_only = Some(false),
                    b"samesite=strict" if same_site.is_none() => {
                        same_site = Some(HttpSameSite::Strict);
                    }
                    b"samesite=lax" if same_site.is_none() => {
                        same_site = Some(HttpSameSite::Lax);
                    }
                    b"samesite=none" if same_site.is_none() => {
                        same_site = Some(HttpSameSite::None);
                    }
                    b"secure" | b"nosecure" | b"httponly" | b"nohttponly" | b"samesite=strict"
                    | b"samesite=lax" | b"samesite=none" => {
                        valid = false;
                        issues.push(issue(
                            origin,
                            E_SEMANTICS_NOT_REPRESENTABLE,
                            "proxy_cookie_flags repeats or conflicts on one cookie attribute",
                        ));
                    }
                    _ => {
                        valid = false;
                        issues.push(issue(
                            origin,
                            E_SEMANTICS_NOT_REPRESENTABLE,
                            "proxy_cookie_flags flag is outside the canonical secure, HttpOnly, and SameSite subset",
                        ));
                    }
                }
            }
            if flags.is_empty() {
                valid = false;
                issues.push(issue(
                    origin,
                    E_INVALID_VALUE,
                    "proxy_cookie_flags requires at least one cookie attribute flag",
                ));
            }
            if !valid {
                continue;
            }
            if !names.insert(name.to_owned()) {
                issues.push(issue(
                    origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "proxy_cookie_flags defines more than one policy for a cookie name",
                ));
                continue;
            }
            attributes.push(HttpCookieAttributePolicy {
                name: name.to_owned(),
                secure,
                http_only,
                same_site,
            });
        }
        attributes
    }

    fn lower_proxy_retry(
        &self,
        location: &EffectiveLocation,
        endpoint_count: usize,
        issues: &mut Vec<LowerIssue>,
    ) -> HttpRetryPolicy {
        let Some(policy) =
            self.effective_policy(location.origin.occurrence, b"proxy_next_upstream")
        else {
            if endpoint_count > 1 {
                issues.push(issue(
                    &location.origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "nginx defaults proxy_next_upstream to error timeout, whose request/write/read failure breadth is wider than canonical connect retries",
                ));
            }
            return HttpRetryPolicy {
                max_retries: 0,
                ..HttpRetryPolicy::default()
            };
        };
        if policy.arguments.as_slice() == [b"off".to_vec()] {
            return HttpRetryPolicy {
                max_retries: 0,
                ..HttpRetryPolicy::default()
            };
        }
        let mut triggers = Vec::new();
        for trigger in &policy.arguments {
            match trigger.as_slice() {
                b"error" | b"timeout" => issues.push(issue(
                    policy.origins.last().unwrap_or(&location.origin),
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "nginx proxy_next_upstream error/timeout includes post-connect request, response-header, and I/O failures beyond canonical connect retry triggers",
                )),
                _ => issues.push(issue(
                    policy.origins.last().unwrap_or(&location.origin),
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "proxy_next_upstream trigger is broader than the canonical safe subset",
                )),
            }
        }
        triggers.sort_unstable_by_key(|trigger| match trigger {
            HttpRetryTrigger::ConnectFailure => 0,
            HttpRetryTrigger::ConnectTimeout => 1,
            HttpRetryTrigger::RefusedStream => 2,
            HttpRetryTrigger::EmptyResponse => 3,
            HttpRetryTrigger::ResponseTimeout => 4,
            HttpRetryTrigger::JunkResponse => 5,
        });
        triggers.dedup();
        let configured_tries =
            self.effective_policy(location.origin.occurrence, b"proxy_next_upstream_tries");
        let tries = configured_tries
            .as_ref()
            .and_then(|value| value.arguments.first())
            .and_then(|value| utf8(value))
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(endpoint_count);
        if tries == 0 || tries > 3 {
            issues.push(issue(
                configured_tries
                    .as_ref()
                    .and_then(|value| value.origins.last())
                    .unwrap_or(&location.origin),
                E_SEMANTICS_NOT_REPRESENTABLE,
                "proxy_next_upstream attempts exceed the canonical two-retry bound",
            ));
        }
        HttpRetryPolicy {
            max_retries: u8::try_from(tries.saturating_sub(1).min(2))
                .expect("retry count is bounded to two"),
            triggers,
            ..HttpRetryPolicy::default()
        }
    }

    fn proxy_timeouts(
        &self,
        scope: OccurrenceId,
        origin: &DirectiveOrigin,
        issues: &mut Vec<LowerIssue>,
    ) -> ProxyTimeouts {
        let connect = collect_result(
            self.proxy_timeout(scope, b"proxy_connect_timeout", origin),
            issues,
        )
        .unwrap_or(NGINX_DEFAULT_PROXY_TIMEOUT_MS);
        let read = collect_result(
            self.proxy_timeout(scope, b"proxy_read_timeout", origin),
            issues,
        )
        .unwrap_or(NGINX_DEFAULT_PROXY_TIMEOUT_MS);
        let send = collect_result(
            self.proxy_timeout(scope, b"proxy_send_timeout", origin),
            issues,
        )
        .unwrap_or(NGINX_DEFAULT_PROXY_TIMEOUT_MS);
        ProxyTimeouts {
            connect,
            read,
            write: send,
        }
    }

    fn proxy_timeout(
        &self,
        scope: OccurrenceId,
        name: &[u8],
        fallback_origin: &DirectiveOrigin,
    ) -> Result<u64, LowerIssue> {
        self.effective_policy(scope, name)
            .map_or(Ok(NGINX_DEFAULT_PROXY_TIMEOUT_MS), |value| {
                parse_duration_ms(&value.arguments[0]).ok_or_else(|| {
                    issue(
                        value.origins.last().unwrap_or(fallback_origin),
                        E_INVALID_VALUE,
                        "nginx timeout is not a finite canonical millisecond value",
                    )
                })
            })
    }
}
