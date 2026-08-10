impl Lowerer {
    pub(super) fn lower_bind(
        &mut self,
        http: &EffectiveHttp,
        bind: &EffectiveBind,
        http_index: usize,
        bind_index: usize,
    ) -> BindBlock {
        let servers = bind
            .servers
            .iter()
            .filter_map(|occurrence| {
                http.servers
                    .iter()
                    .find(|server| server.origin.occurrence == *occurrence)
            })
            .collect::<Vec<_>>();
        let listener_bind = listener_bind(bind, &servers);
        let mut issues = self.semantic_bind_issues(http, &servers);
        let (gzip, gzip_origins) = match self.lower_gzip(bind, &servers) {
            Ok(gzip) => gzip,
            Err(gzip_issues) => {
                issues.extend(gzip_issues);
                (None, GzipOrigins::default())
            }
        };
        let mut uses_default_access_log = false;
        let mut disables_access_log = false;
        for server in &servers {
            match self.effective_policy(server.origin.occurrence, b"access_log") {
                None if self.default_access_log_path.is_some() => uses_default_access_log = true,
                None => issues.push(issue(
                    &server.origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "omitted nginx access_log enables the unrepresented default combined log",
                )),
                Some(access_log)
                    if access_log.arguments.first().is_some_and(|value| value == b"off") =>
                {
                    disables_access_log = true;
                }
                Some(access_log) => issues.push(issue(
                    access_log.origins.last().unwrap_or(&server.origin),
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "nginx formatted access_log output is not equivalent to canonical JSON access logging",
                )),
            }
        }
        if uses_default_access_log && disables_access_log {
            issues.push(issue(
                servers.first().map_or(&http.origin, |server| &server.origin),
                E_SEMANTICS_NOT_REPRESENTABLE,
                "one canonical HTTP service cannot mix migrated default and disabled nginx access logs",
            ));
        }
        if listener_bind.is_none() {
            issues.push(issue(
                servers
                    .first()
                    .map_or(&http.origin, |server| &server.origin),
                E_SEMANTICS_NOT_REPRESENTABLE,
                "nginx listener is not an explicit socket or canonical Unix address",
            ));
        }
        if !issues.is_empty() {
            return BindBlock {
                bind: listener_bind,
                issues,
                candidate: None,
            };
        }
        let listener_bind = listener_bind.expect("checked explicit bind");

        let service_name = format!("nginx-http-service-{http_index}-{bind_index}");
        let downstream_tls = servers.iter().any(|server| {
            matching_listen(server, &bind.endpoint)
                .options
                .iter()
                .any(|option| option.value == b"ssl")
        });
        let candidate = self.lower_bind_routes(
            http,
            bind,
            &servers,
            listener_bind.clone(),
            service_name,
            http_index,
            bind_index,
            downstream_tls,
            uses_default_access_log,
            gzip,
            gzip_origins,
            &mut issues,
        );
        BindBlock {
            bind: Some(listener_bind),
            issues,
            candidate,
        }
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "one atomic bind lowering keeps partial canonical services from escaping"
    )]
    fn lower_bind_routes(
        &mut self,
        http: &EffectiveHttp,
        bind: &EffectiveBind,
        servers: &[&EffectiveServer],
        listener_bind: ListenerBind,
        service_name: String,
        http_index: usize,
        bind_index: usize,
        downstream_tls: bool,
        uses_default_access_log: bool,
        gzip: Option<HttpGzipPolicy>,
        gzip_origins: GzipOrigins,
        issues: &mut Vec<LowerIssue>,
    ) -> Option<BindCandidate> {
        let mut routes = Vec::new();
        let mut pools = Vec::new();
        let mut pool_names = HashSet::new();
        let mut route_origins = Vec::new();
        let mut all_origins = Vec::new();
        let (downstream_timeouts, downstream_timeout_origins) =
            self.lower_downstream_timeouts(bind, servers, issues);
        for server in servers {
            if !Self::server_participates(bind, server) {
                continue;
            }
            let hosts = match Self::route_hosts(server, bind) {
                Ok(hosts) => hosts,
                Err(server_issues) => {
                    issues.extend(server_issues);
                    continue;
                }
            };
            let nginx_host_fallback = server.server_names.iter().find_map(|name| {
                (name.kind == ServerNameKind::Exact)
                    .then(|| utf8(&name.normalized))
                    .flatten()
                    .and_then(canonical_exact_host)
                    .map(|host| match host.parse::<IpAddr>() {
                        Ok(IpAddr::V6(ip)) => format!("[{ip}]"),
                        _ => host,
                    })
            });
            all_origins.push(server.origin.clone());
            let mut has_local_catch_all = false;
            for location in &server.locations {
                let synthetic_name = self
                    .occurrence(location.origin.occurrence)
                    .map(|occurrence| occurrence.directive.name.value.as_slice());
                let location_hosts = if synthetic_name == Some(b"if") {
                    let host = self
                        .occurrence(location.origin.occurrence)
                        .and_then(|occurrence| certbot_host_condition(&occurrence.directive))
                        .and_then(|host| utf8(host).and_then(canonical_exact_host));
                    let Some(host) = host else {
                        issues.push(issue(
                            &location.origin,
                            E_INVALID_VALUE,
                            "nginx host redirect condition is not a canonical exact host",
                        ));
                        continue;
                    };
                    vec![Some(HttpHostSelector::NormalizedHost { value: host })]
                } else if synthetic_name == Some(b"return") && hosts.contains(&None) {
                    vec![None]
                } else {
                    hosts.clone()
                };
                let top_level_catch_all = location.kind == LocationKind::Prefix
                    && location
                        .path
                        .as_ref()
                        .is_some_and(|path| path.value == b"/");
                has_local_catch_all |= top_level_catch_all;
                match self.lower_location(
                    http,
                    location,
                    &location_hosts,
                    &service_name,
                    http_index,
                    routes.len(),
                    downstream_tls,
                    nginx_host_fallback.as_deref(),
                ) {
                    Ok(lowered) => {
                        for mut route in lowered.routes {
                            route.origins.push(server.origin.clone());
                            route.origins.extend(
                                bind.names
                                    .iter()
                                    .filter(|name| name.server == server.origin.occurrence)
                                    .map(|name| name.name.origin.clone()),
                            );
                            route_origins.push(route.origins.clone());
                            all_origins.extend(route.origins.clone());
                            if let Some(pool) = route.pool
                                && pool_names.insert(pool.pool.name.clone())
                            {
                                pools.push(pool);
                            }
                            routes.push(route.route);
                        }
                    }
                    Err(location_issues) => issues.extend(location_issues),
                }
            }
            if !has_local_catch_all {
                let server_kind = if server.origin.occurrence == bind.default_server {
                    "default server"
                } else {
                    "non-default server"
                };
                issues.push(issue(
                    &server.origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    format!(
                        "nginx {server_kind} static or implicit fallback requires a representable location / catch-all"
                    ),
                ));
            }
        }
        let tls = match self.lower_tls(
            http,
            bind,
            servers,
            format!("nginx-tls-profile-{http_index}-{bind_index}"),
        ) {
            Ok(tls) => tls,
            Err(tls_issues) => {
                issues.extend(tls_issues);
                None
            }
        };
        if !issues.is_empty() || routes.is_empty() {
            return None;
        }
        let tls_profile = tls.as_ref().map(|tls| tls.profile.clone());
        let certificates = tls
            .as_ref()
            .map_or_else(Vec::new, |tls| tls.certificates.clone());
        if let Some(tls) = tls {
            all_origins.extend(tls.origins);
        }
        all_origins.extend(gzip_origins.all());
        all_origins.extend(downstream_timeout_origins);
        let access_log = if uses_default_access_log {
            self.used_default_access_log_overlay = true;
            Some(AccessLogPolicy::File {
                path: self
                    .default_access_log_path
                    .clone()
                    .expect("default access-log migration path"),
            })
        } else {
            Some(AccessLogPolicy::Disabled)
        };
        let upstream_io_timeout_ms = routes
            .iter()
            .filter_map(|route| {
                matches!(&route.action, HttpRouteAction::Proxy { .. }).then_some(
                    route
                        .policy
                        .connect_timeout_ms
                        .max(route.policy.read_timeout_ms)
                        .max(route.policy.write_timeout_ms),
                )
            })
            .max()
            .unwrap_or(NGINX_DEFAULT_PROXY_TIMEOUT_MS);
        Some(BindCandidate {
            listener: Listener {
                name: format!("nginx-http-listener-{http_index}-{bind_index}"),
                bind: listener_bind,
                protocol: Protocol::Http,
                service: Some(service_name.clone()),
                tls_profile: tls_profile.as_ref().map(|profile| profile.name.clone()),
                proxy_protocol: None,
                max_connections: None,
                downstream_timeouts,
            },
            service: HttpService {
                name: service_name,
                routes,
                automatic_response_headers: true,
                upstream_io_timeout_ms,
                max_request_body_bytes: Some(NGINX_DEFAULT_BODY_BYTES),
                gzip,
                access_log,
            },
            pools,
            certificates,
            tls_profile,
            origins: all_origins,
            gzip_origins,
            route_origins,
        })
    }

    fn lower_gzip(
        &self,
        bind: &EffectiveBind,
        servers: &[&EffectiveServer],
    ) -> Result<(Option<HttpGzipPolicy>, GzipOrigins), Vec<LowerIssue>> {
        let mut effective: Option<Option<HttpGzipPolicy>> = None;
        let mut origins = GzipOrigins::default();
        let mut issues = Vec::new();
        for server in servers
            .iter()
            .copied()
            .filter(|server| Self::server_participates(bind, server))
        {
            if let Some((policy, policy_origins)) = self.effective_gzip(server, &mut issues) {
                if effective
                    .as_ref()
                    .is_some_and(|existing| existing.as_ref() != policy.as_ref())
                {
                    let mismatch_origins = policy_origins.all();
                    issues.push(issue(
                        mismatch_origins.last().unwrap_or(&server.origin),
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "virtual servers on one nginx bind have different effective gzip policies",
                    ));
                } else if effective.is_none() {
                    effective = Some(policy.clone());
                }
                origins.extend(policy_origins);
            }
        }
        if issues.is_empty() {
            Ok((effective.unwrap_or(None), origins))
        } else {
            Err(issues)
        }
    }

    fn lower_downstream_timeouts(
        &self,
        bind: &EffectiveBind,
        servers: &[&EffectiveServer],
        issues: &mut Vec<LowerIssue>,
    ) -> (DownstreamTimeoutPolicy, Vec<DirectiveOrigin>) {
        let mut values = Vec::new();
        let mut origins = Vec::new();
        let mut invalid = false;
        for server in servers
            .iter()
            .copied()
            .filter(|server| Self::server_participates(bind, server))
        {
            let Some(policy) =
                self.effective_policy(server.origin.occurrence, b"keepalive_timeout")
            else {
                values.push(None);
                continue;
            };
            origins.extend(policy.origins.clone());
            let origin = policy.origins.last().unwrap_or(&server.origin);
            let value = match policy.arguments.as_slice() {
                [timeout] => {
                    if let Some(milliseconds) = parse_duration_ms(timeout) {
                        Some(milliseconds)
                    } else {
                        invalid = true;
                        issues.push(issue(
                            origin,
                            E_INVALID_VALUE,
                            "keepalive_timeout must be a positive finite nginx duration",
                        ));
                        None
                    }
                }
                [_, _] => {
                    invalid = true;
                    issues.push(issue(
                        origin,
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "keepalive_timeout header timeout is not represented by the canonical listener policy",
                    ));
                    None
                }
                _ => {
                    invalid = true;
                    issues.push(issue(
                        origin,
                        E_INVALID_VALUE,
                        "keepalive_timeout requires one positive finite timeout",
                    ));
                    None
                }
            };
            values.push(value);
        }

        let distinct = values.iter().copied().collect::<HashSet<_>>();
        if !invalid && distinct.len() > 1 {
            let origin = origins.first().or_else(|| {
                servers
                    .iter()
                    .find(|server| Self::server_participates(bind, server))
                    .map(|server| &server.origin)
            });
            if let Some(origin) = origin {
                issues.push(issue(
                    origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "virtual servers on one nginx bind have different effective keepalive_timeout policies",
                ));
            }
            invalid = true;
        }

        (
            DownstreamTimeoutPolicy {
                client_timeout_ms: None,
                request_timeout_ms: None,
                keepalive_timeout_ms: (!invalid)
                    .then(|| distinct.into_iter().next().flatten())
                    .flatten(),
            },
            origins,
        )
    }

    #[expect(
        clippy::too_many_lines,
        reason = "effective nginx gzip inheritance, validation, and provenance are one policy"
    )]
    fn effective_gzip(
        &self,
        server: &EffectiveServer,
        issues: &mut Vec<LowerIssue>,
    ) -> Option<(Option<HttpGzipPolicy>, GzipOrigins)> {
        let gzip = self.effective_policy(server.origin.occurrence, b"gzip");
        let level = self.effective_policy(server.origin.occurrence, b"gzip_comp_level");
        let types = self.effective_policy(server.origin.occurrence, b"gzip_types");
        let min_length = self.effective_policy(server.origin.occurrence, b"gzip_min_length");
        let min_http_version =
            self.effective_policy(server.origin.occurrence, b"gzip_http_version");
        let proxied = self.effective_policy(server.origin.occurrence, b"gzip_proxied");
        let vary = self.effective_policy(server.origin.occurrence, b"gzip_vary");

        let compression_level = level
            .as_ref()
            .map_or(Some(NGINX_DEFAULT_GZIP_LEVEL), |policy| {
                let parsed = policy
                    .arguments
                    .first()
                    .and_then(|value| utf8(value))
                    .and_then(|value| value.parse::<u8>().ok())
                    .filter(|value| (1..=9).contains(value));
                if parsed.is_none() {
                    issues.push(issue(
                        policy.origins.last().unwrap_or(&server.origin),
                        E_INVALID_VALUE,
                        "gzip_comp_level must be an integer between 1 and 9",
                    ));
                }
                parsed
            });

        let mut content_types = vec![NGINX_IMPLICIT_GZIP_TYPE.to_owned()];
        if let Some(policy) = &types {
            for value in &policy.arguments {
                let Some(value) = utf8(value) else {
                    issues.push(issue(
                        policy.origins.last().unwrap_or(&server.origin),
                        E_INVALID_VALUE,
                        "gzip_types values must be UTF-8 MIME types",
                    ));
                    continue;
                };
                if value == "*" {
                    issues.push(issue(
                        policy.origins.last().unwrap_or(&server.origin),
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "gzip_types wildcard compression is not representable canonically",
                    ));
                    continue;
                }
                if !valid_gzip_type(value) {
                    issues.push(issue(
                        policy.origins.last().unwrap_or(&server.origin),
                        E_INVALID_VALUE,
                        "gzip_types values must be concrete MIME types",
                    ));
                    continue;
                }
                let value = value.to_ascii_lowercase();
                if !content_types.contains(&value) {
                    content_types.push(value);
                }
            }
        }
        if content_types.len() > 64 {
            issues.push(issue(
                types
                    .as_ref()
                    .and_then(|policy| policy.origins.last())
                    .unwrap_or(&server.origin),
                E_SEMANTICS_NOT_REPRESENTABLE,
                "effective gzip_types exceeds the canonical limit of 64 MIME types",
            ));
        }

        let minimum_length_bytes =
            min_length
                .as_ref()
                .map_or(Some(NGINX_DEFAULT_GZIP_MIN_LENGTH_BYTES), |policy| {
                    let parsed = policy
                        .arguments
                        .first()
                        .and_then(|value| parse_nonnegative_size(value));
                    if parsed.is_none() {
                        issues.push(issue(
                            policy.origins.last().unwrap_or(&server.origin),
                            E_INVALID_VALUE,
                            "gzip_min_length must be a nonnegative nginx size",
                        ));
                    }
                    parsed
                });
        let minimum_http_version =
            min_http_version
                .as_ref()
                .map_or(Some(HttpGzipMinimumVersion::Http11), |policy| match policy
                    .arguments
                    .as_slice()
                {
                    [value] if value == b"1.0" => Some(HttpGzipMinimumVersion::Http10),
                    [value] if value == b"1.1" => Some(HttpGzipMinimumVersion::Http11),
                    _ => {
                        issues.push(issue(
                            policy.origins.last().unwrap_or(&server.origin),
                            E_INVALID_VALUE,
                            "gzip_http_version must be `1.0` or `1.1`",
                        ));
                        None
                    }
                });
        let disable_on_via = proxied.as_ref().map_or(Some(true), |policy| {
            if policy.arguments.as_slice() == [b"off".to_vec()] {
                return Some(true);
            }
            let valid = policy
                .arguments
                .iter()
                .all(|argument| NGINX_GZIP_PROXIED_MODES.contains(&argument.as_slice()));
            issues.push(issue(
                policy.origins.last().unwrap_or(&server.origin),
                if valid {
                    E_SEMANTICS_NOT_REPRESENTABLE
                } else {
                    E_INVALID_VALUE
                },
                if valid {
                    "gzip_proxied modes other than `off` are not representable canonically"
                } else {
                    "gzip_proxied contains an invalid mode"
                },
            ));
            None
        });
        let vary_enabled =
            vary.as_ref()
                .map_or(Some(false), |policy| match policy.arguments.as_slice() {
                    [value] if value == b"off" => Some(false),
                    [value] if value == b"on" => Some(true),
                    _ => {
                        issues.push(issue(
                            policy.origins.last().unwrap_or(&server.origin),
                            E_INVALID_VALUE,
                            "gzip_vary must be `on` or `off`",
                        ));
                        None
                    }
                });

        let enabled = match gzip.as_ref().map(|policy| policy.arguments.as_slice()) {
            None => false,
            Some([value]) if value == b"off" => false,
            Some([value]) if value == b"on" => true,
            Some(_) => {
                let policy = gzip.as_ref().expect("invalid explicit gzip policy");
                issues.push(issue(
                    policy.origins.last().unwrap_or(&server.origin),
                    E_INVALID_VALUE,
                    "gzip must be `on` or `off`",
                ));
                return None;
            }
        };
        let gzip_origins = gzip
            .as_ref()
            .map_or_else(Vec::new, |policy| policy.origins.clone());
        if !enabled {
            return Some((
                None,
                GzipOrigins {
                    gzip: gzip_origins,
                    ..GzipOrigins::default()
                },
            ));
        }
        let (
            Some(level_value),
            Some(min_length_value),
            Some(min_version_value),
            Some(disable_value),
            Some(vary_value),
        ) = (
            compression_level,
            minimum_length_bytes,
            minimum_http_version,
            disable_on_via,
            vary_enabled,
        )
        else {
            return None;
        };
        let field_origins = |policy: Option<&PolicyValue>| {
            policy.map_or_else(|| gzip_origins.clone(), |policy| policy.origins.clone())
        };
        let level_origins = field_origins(level.as_ref());
        let content_type_origins = field_origins(types.as_ref());
        let min_length_origins = field_origins(min_length.as_ref());
        let min_version_origins = field_origins(min_http_version.as_ref());
        let proxied_origins = field_origins(proxied.as_ref());
        let vary_origins = field_origins(vary.as_ref());
        Some((
            Some(HttpGzipPolicy {
                level: level_value,
                content_types,
                min_length_bytes: min_length_value,
                min_http_version: min_version_value,
                disable_on_via: disable_value,
                vary: vary_value,
            }),
            GzipOrigins {
                gzip: gzip_origins,
                level: level_origins,
                content_types: content_type_origins,
                min_length_bytes: min_length_origins,
                min_http_version: min_version_origins,
                disable_on_via: proxied_origins,
                vary: vary_origins,
            },
        ))
    }

    fn server_participates(bind: &EffectiveBind, server: &EffectiveServer) -> bool {
        server.origin.occurrence == bind.default_server
            || bind
                .names
                .iter()
                .any(|name| name.server == server.origin.occurrence)
    }

    fn semantic_bind_issues(
        &self,
        http: &EffectiveHttp,
        servers: &[&EffectiveServer],
    ) -> Vec<LowerIssue> {
        let server_ids = servers
            .iter()
            .map(|server| server.origin.occurrence)
            .collect::<HashSet<_>>();
        self.blocking_decisions()
            .filter_map(|(decision, code)| {
                let affects_server = server_ids
                    .iter()
                    .any(|server| self.is_descendant(decision.occurrence, *server));
                let global = self.is_global_http_occurrence(decision.occurrence, http);
                (affects_server || global).then(|| LowerIssue {
                    origin: self.origin(decision.occurrence),
                    code,
                    message: "blocking nginx directive affects this service".into(),
                    emit: false,
                })
            })
            .collect()
    }

    fn is_global_http_occurrence(&self, occurrence: OccurrenceId, http: &EffectiveHttp) -> bool {
        if occurrence == http.origin.occurrence {
            return true;
        }
        let Some(top) = self.child_below(occurrence, http.origin.occurrence) else {
            return false;
        };
        let Some(expanded) = self.occurrence(top) else {
            return false;
        };
        !matches!(
            expanded.directive.name.value.as_slice(),
            b"server" | b"upstream"
        )
    }

    fn route_hosts(
        server: &EffectiveServer,
        bind: &EffectiveBind,
    ) -> Result<Vec<Option<HttpHostSelector>>, Vec<LowerIssue>> {
        let mut hosts = Vec::new();
        let mut issues = Vec::new();
        for name in bind
            .names
            .iter()
            .filter(|name| name.server == server.origin.occurrence)
            .map(|name| &name.name)
        {
            if name.normalized == b"_" {
                continue;
            }
            let host = match name.kind {
                ServerNameKind::Exact => utf8(&name.normalized)
                    .and_then(canonical_exact_host)
                    .map(|value| HttpHostSelector::NormalizedHost { value }),
                ServerNameKind::LeadingWildcard => utf8(&name.normalized)
                    .and_then(|name| name.strip_prefix("*."))
                    .filter(|suffix| canonical_dns_name(suffix))
                    .map(|value| HttpHostSelector::NginxLeadingWildcard {
                        value: value.into(),
                    }),
                ServerNameKind::LeadingWildcardAndExact => utf8(&name.normalized)
                    .and_then(|name| name.strip_prefix('.'))
                    .filter(|suffix| canonical_dns_name(suffix))
                    .map(|value| HttpHostSelector::NginxLeadingDot {
                        value: value.into(),
                    }),
                ServerNameKind::TrailingWildcard => {
                    issues.push(issue(
                        &name.origin,
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "nginx trailing wildcard has no canonical host selector",
                    ));
                    None
                }
                ServerNameKind::Regex | ServerNameKind::Variable | ServerNameKind::Invalid => None,
            };
            if let Some(host) = host {
                if !hosts.contains(&Some(host.clone())) {
                    hosts.push(Some(host));
                }
            } else if issues.is_empty() {
                issues.push(issue(
                    &name.origin,
                    E_INVALID_VALUE,
                    "server_name is not a canonical exact or one-label wildcard host",
                ));
            }
        }
        if server.origin.occurrence == bind.default_server {
            hosts.push(None);
        } else if hosts.is_empty() && issues.is_empty() {
            issues.push(issue(
                &server.origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "non-default nginx server has no canonical host identity",
            ));
        }
        if issues.is_empty() {
            Ok(hosts)
        } else {
            Err(issues)
        }
    }

}
