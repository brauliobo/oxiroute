impl Lowerer {
    #[expect(
        clippy::too_many_lines,
        reason = "matcher, action, and provenance must be accepted or rejected as one route"
    )]
    #[allow(clippy::too_many_arguments)]
    fn lower_location(
        &self,
        http: &EffectiveHttp,
        location: &EffectiveLocation,
        hosts: &[Option<HttpHostSelector>],
        service_name: &str,
        http_index: usize,
        route_ordinal: usize,
        downstream_tls: bool,
        nginx_host_fallback: Option<&str>,
    ) -> Result<LoweredLocation, Vec<LowerIssue>> {
        let mut issues = Vec::new();
        if !matches!(location.kind, LocationKind::Exact | LocationKind::Prefix) {
            issues.push(issue(
                &location.origin,
                E_UNSUPPORTED_FEATURE,
                "nginx location kind is unsupported by canonical routing",
            ));
        }
        let path_prefix = location.path.as_ref().and_then(|path| utf8(&path.value));
        let canonical_path = path_prefix.and_then(canonicalize_http_path);
        if path_prefix.is_none()
            || canonical_path
                .as_ref()
                .is_none_or(|canonical| canonical.as_ref() != path_prefix.expect("checked path"))
        {
            issues.push(issue(
                &location.origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "nginx location path has ambiguous canonical normalization",
            ));
        }
        if !location.children.is_empty() {
            issues.push(issue(
                &location.origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "nested nginx location selection is not one canonical route precedence domain",
            ));
        }
        let body_bytes =
            self.body_policy(location.origin.occurrence, &location.origin, &mut issues);
        let access_policy = self.lower_access_policy(location, &mut issues);
        let selector = match location.kind {
            LocationKind::Exact => HttpPathSelector::Exact {
                value: path_prefix.unwrap_or("/").into(),
            },
            LocationKind::Prefix => HttpPathSelector::RawPrefix {
                value: path_prefix.unwrap_or("/").into(),
            },
            _ => HttpPathSelector::RawPrefix { value: "/".into() },
        };
        let return_policy = self.effective_policy(location.origin.occurrence, b"return");
        let (action, pool, timeouts, mut origins) = if let Some(value) = return_policy {
            let return_status = match value.arguments.as_slice() {
                [payload]
                    if payload.starts_with(b"http://") || payload.starts_with(b"https://") =>
                {
                    Some(302)
                }
                [status] | [status, _] => utf8(status).and_then(|status| status.parse().ok()),
                _ => None,
            };
            if return_status.is_some_and(|status| {
                self.error_page_matches_status(location.origin.occurrence, status)
            }) {
                issues.push(issue(
                    value.origins.last().unwrap_or(&location.origin),
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "nginx return status triggers an error_page redirect outside local action semantics",
                ));
            }
            let mut header_origins = Vec::new();
            let headers = self.lower_literal_headers(location, &mut header_origins, &mut issues);
            let action = self.lower_return(
                &value,
                &location.origin,
                nginx_host_fallback,
                headers,
                &mut issues,
            );
            let mut origins = value.origins;
            origins.extend(header_origins);
            (
                action.unwrap_or(HttpRouteAction::FixedResponse {
                    status: 500,
                    body: String::new(),
                    headers: Vec::new(),
                }),
                None,
                None,
                origins,
            )
        } else if let Some(proxy) = &location.proxy_pass {
            match self.lower_proxy(
                http,
                location,
                proxy,
                service_name,
                http_index,
                route_ordinal,
                downstream_tls,
                nginx_host_fallback,
                &mut issues,
            ) {
                Some(proxy) => (
                    HttpRouteAction::Proxy {
                        upstream_pool: proxy.pool.pool.name.clone(),
                        policy: proxy.policy,
                    },
                    Some(proxy.pool),
                    Some(proxy.timeouts),
                    proxy.origins,
                ),
                None => (
                    HttpRouteAction::FixedResponse {
                        status: 500,
                        body: String::new(),
                        headers: Vec::new(),
                    },
                    None,
                    None,
                    Vec::new(),
                ),
            }
        } else if let Some((action, origins)) = self.lower_static(location, &mut issues) {
            (action, None, None, origins)
        } else {
            (
                HttpRouteAction::FixedResponse {
                    status: 500,
                    body: String::new(),
                    headers: Vec::new(),
                },
                None,
                None,
                Vec::new(),
            )
        };
        origins.push(location.origin.clone());
        if !issues.is_empty() {
            return Err(issues);
        }
        Ok(LoweredLocation {
            routes: hosts
                .iter()
                .cloned()
                .enumerate()
                .map(|(index, host)| LoweredRoute {
                    route: HttpRoute {
                        host,
                        path: selector.clone(),
                        methods: Vec::new(),
                        access_policy: access_policy.clone(),
                        policy: HttpRoutePolicy {
                            max_request_body_bytes: body_bytes,
                            connect_timeout_ms: timeouts
                                .map_or(NGINX_DEFAULT_PROXY_TIMEOUT_MS, |value| value.connect),
                            read_timeout_ms: timeouts
                                .map_or(NGINX_DEFAULT_PROXY_TIMEOUT_MS, |value| value.read),
                            write_timeout_ms: timeouts
                                .map_or(NGINX_DEFAULT_PROXY_TIMEOUT_MS, |value| value.write),
                            request_buffering: self
                                .effective_policy(
                                    location.origin.occurrence,
                                    b"proxy_request_buffering",
                                )
                                .is_none_or(|value| {
                                    value.arguments.as_slice() != [b"off".to_vec()]
                                }),
                            response_buffering: false,
                        },
                        action: action.clone(),
                    },
                    pool: (index == 0).then(|| pool.clone()).flatten(),
                    origins: origins.clone(),
                })
                .collect(),
        })
    }

    fn validate_route_policy(
        &self,
        scope: OccurrenceId,
        fallback_origin: &DirectiveOrigin,
    ) -> Result<(), Vec<LowerIssue>> {
        let mut issues = Vec::new();
        let version = self.effective_policy(scope, b"proxy_http_version");
        if version
            .as_ref()
            .is_some_and(|value| value.arguments.as_slice() != [b"1.1".to_vec()])
        {
            let origin = version
                .as_ref()
                .and_then(|value| value.origins.last())
                .unwrap_or(fallback_origin);
            issues.push(issue(
                origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "proxy_http_version must be 1.1 for the canonical upstream HTTP policy",
            ));
        }
        let response_buffering = self.effective_policy(scope, b"proxy_buffering");
        if response_buffering
            .as_ref()
            .is_none_or(|value| value.arguments.as_slice() != [b"off".to_vec()])
        {
            issues.push(issue(
                response_buffering
                    .as_ref()
                    .and_then(|value| value.origins.last())
                    .unwrap_or(fallback_origin),
                E_SEMANTICS_NOT_REPRESENTABLE,
                "proxy_buffering must explicitly be off to match unbuffered runtime semantics",
            ));
        }
        if issues.is_empty() {
            Ok(())
        } else {
            Err(issues)
        }
    }

    fn body_policy(
        &self,
        scope: OccurrenceId,
        fallback_origin: &DirectiveOrigin,
        issues: &mut Vec<LowerIssue>,
    ) -> Option<u64> {
        let Some(value) = self.effective_policy(scope, b"client_max_body_size") else {
            return Some(NGINX_DEFAULT_BODY_BYTES);
        };
        if value
            .arguments
            .first()
            .is_some_and(|argument| argument == b"0")
        {
            return None;
        }
        if let Some(bytes) = value
            .arguments
            .first()
            .and_then(|argument| parse_size(argument))
        {
            Some(bytes)
        } else {
            issues.push(issue(
                value.origins.last().unwrap_or(fallback_origin),
                E_INVALID_VALUE,
                "client_max_body_size is not a canonical byte limit",
            ));
            Some(NGINX_DEFAULT_BODY_BYTES)
        }
    }

    fn lower_access_policy(
        &self,
        location: &EffectiveLocation,
        issues: &mut Vec<LowerIssue>,
    ) -> Option<oxiroute_config::HttpAccessPolicy> {
        if let Some(conditional) = self.graph.expanded_occurrences.iter().find(|occurrence| {
            occurrence.parent == Some(location.origin.occurrence)
                && occurrence.directive.name.value == b"if"
                && occurrence.directive.arguments.iter().any(|argument| {
                    argument
                        .value
                        .windows(b"<redacted>".len())
                        .any(|window| window == b"<redacted>")
                })
        }) {
            let server = self
                .resolution
                .http_blocks
                .iter()
                .flat_map(|http| &http.servers)
                .find(|server| {
                    self.is_descendant(location.origin.occurrence, server.origin.occurrence)
                });
            let overlay = server.and_then(|server| {
                server.server_names.iter().find_map(|name| {
                    self.bearer_token_overlays
                        .get(&name.normalized)
                        .cloned()
                        .map(|path| (name.normalized.clone(), path))
                })
            });
            let Some((overlay_name, token_file_path)) = overlay else {
                issues.push(issue(
                    &self.origin(conditional.id),
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "redacted nginx authorization rule requires an explicit bearer token file overlay",
                ));
                return None;
            };
            if !token_file_path.is_absolute() {
                issues.push(issue(
                    &self.origin(conditional.id),
                    E_INVALID_VALUE,
                    "bearer token overlay path must be absolute",
                ));
                return None;
            }
            self.used_bearer_token_overlays
                .borrow_mut()
                .insert(overlay_name);
            return Some(HttpAccessPolicy::BearerTokenFile {
                token_file_path,
                header_name: "authorization".into(),
                realm: None,
            });
        }
        let basic = self.effective_policy(location.origin.occurrence, b"auth_basic")?;
        if basic.arguments.as_slice() == [b"off".to_vec()] {
            return None;
        }
        let Some(file) = self.effective_policy(location.origin.occurrence, b"auth_basic_user_file")
        else {
            issues.push(issue(
                basic.origins.last().unwrap_or(&location.origin),
                E_INVALID_VALUE,
                "nginx Basic authentication requires an htpasswd file",
            ));
            return None;
        };
        let Some(realm) = basic.arguments.first().and_then(|value| utf8(value)) else {
            issues.push(issue(
                basic.origins.last().unwrap_or(&location.origin),
                E_INVALID_VALUE,
                "nginx Basic authentication realm is not UTF-8",
            ));
            return None;
        };
        let Some(path) = file
            .arguments
            .first()
            .and_then(|value| absolute_file_path(value))
        else {
            issues.push(issue(
                file.origins.last().unwrap_or(&location.origin),
                E_INVALID_VALUE,
                "nginx htpasswd path is not a canonical absolute file",
            ));
            return None;
        };
        self.used_htpasswd_overlays
            .borrow_mut()
            .extend(file.origins.iter().map(|origin| origin.occurrence));
        Some(HttpAccessPolicy::BasicHtpasswdFile {
            htpasswd_file_path: path,
            realm: realm.into(),
        })
    }

    fn lower_return(
        &self,
        value: &PolicyValue,
        fallback: &DirectiveOrigin,
        nginx_host_fallback: Option<&str>,
        mut headers: Vec<HttpLiteralHeader>,
        issues: &mut Vec<LowerIssue>,
    ) -> Option<HttpRouteAction> {
        let origin = value.origins.last().unwrap_or(fallback);
        let (status, payload) = match value.arguments.as_slice() {
            [payload] if payload.starts_with(b"http://") || payload.starts_with(b"https://") => {
                (302, payload.as_slice())
            }
            [status] => (utf8(status)?.parse::<u16>().ok()?, b"".as_slice()),
            [status, payload] => (utf8(status)?.parse::<u16>().ok()?, payload.as_slice()),
            _ => return None,
        };
        let Some(payload) = utf8(payload) else {
            issues.push(issue(
                origin,
                E_INVALID_VALUE,
                "nginx return payload is not UTF-8",
            ));
            return None;
        };
        if (matches!(status, 301 | 302 | 307 | 308) || (status == 404 && payload.is_empty()))
            && let Some(server) = self.default_error_server.as_deref()
        {
            headers.extend(nginx_default_headers(server));
            self.used_default_error_overlay.set(true);
        }
        if matches!(status, 301 | 302 | 307 | 308) {
            if payload.is_empty() {
                issues.push(issue(
                    origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "nginx redirect return requires an explicit location",
                ));
                return None;
            }
            let location = if payload.contains('$') {
                HttpRedirectLocation::RequestTemplate {
                    value: payload.into(),
                    nginx_host_fallback: nginx_host_fallback.map(str::to_owned),
                }
            } else {
                HttpRedirectLocation::Literal {
                    value: payload.into(),
                }
            };
            return Some(HttpRouteAction::Redirect {
                status,
                location,
                headers,
            });
        }
        Some(HttpRouteAction::FixedResponse {
            status,
            body: if status == 404 && payload.is_empty() {
                self.default_error_server
                    .as_deref()
                    .map_or_else(String::new, |server| {
                        nginx_error_body(404, "Not Found", server)
                    })
            } else {
                payload.into()
            },
            headers,
        })
    }

    fn lower_static(
        &self,
        location: &EffectiveLocation,
        issues: &mut Vec<LowerIssue>,
    ) -> Option<(HttpRouteAction, Vec<DirectiveOrigin>)> {
        let root = self.effective_policy(location.origin.occurrence, b"root");
        let alias = self.effective_policy(location.origin.occurrence, b"alias");
        let (directory, path_mapping, mut origins) = match (root, alias) {
            (Some(root), None) => (root, HttpStaticPathMapping::Root, Vec::new()),
            (None, Some(alias)) => (alias, HttpStaticPathMapping::Alias, Vec::new()),
            (Some(root), Some(alias)) => {
                issues.push(issue(
                    alias.origins.last().unwrap_or(&location.origin),
                    E_INVALID_VALUE,
                    "nginx static location cannot combine root and alias",
                ));
                (root, HttpStaticPathMapping::Root, Vec::new())
            }
            (None, None) => {
                issues.push(issue(
                    &location.origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "nginx location has no proxy, return, or static root action",
                ));
                return None;
            }
        };
        origins.extend(directory.origins.clone());
        let Some(root_directory) = directory
            .arguments
            .first()
            .and_then(|value| canonical_directory(value))
        else {
            issues.push(issue(
                directory.origins.last().unwrap_or(&location.origin),
                E_INVALID_VALUE,
                "nginx static root is not a canonical absolute directory",
            ));
            return None;
        };

        let mut index_files = Vec::new();
        for policy in self.effective_list_policy_chain(location.origin.occurrence, b"index") {
            origins.extend(policy.origins.clone());
            for value in policy.arguments {
                if let Some(value) = utf8(&value) {
                    index_files.push(value.into());
                }
            }
        }
        if index_files.is_empty() {
            index_files.push("index.html".into());
        }

        let try_files = self.lower_try_files(location, &mut origins, issues);
        let mime = self.lower_static_mime(location, &mut origins, issues);
        let headers = self.lower_literal_headers(location, &mut origins, issues);
        let mut error_responses = self.lower_error_responses(location, &mut origins, issues);
        if let Some(server) = self.default_error_server.clone()
            && !self.error_page_matches_status(location.origin.occurrence, 404)
        {
            error_responses.push(nginx_default_404(&server));
            self.used_default_error_overlay.set(true);
        }
        let autoindex = self.policy_enabled(location.origin.occurrence, b"autoindex", false);
        let autoindex_exact_size =
            self.policy_enabled(location.origin.occurrence, b"autoindex_exact_size", true);
        let autoindex_local_time =
            self.policy_enabled(location.origin.occurrence, b"autoindex_localtime", false);
        let etag = match self.effective_policy(location.origin.occurrence, b"etag") {
            None => true,
            Some(policy) => {
                origins.extend(policy.origins.clone());
                match policy.arguments.as_slice() {
                    [value] if value == b"on" => true,
                    [value] if value == b"off" => false,
                    _ => {
                        issues.push(issue(
                            policy.origins.last().unwrap_or(&location.origin),
                            E_INVALID_VALUE,
                            "etag must be `on` or `off`",
                        ));
                        true
                    }
                }
            }
        };

        Some((
            HttpRouteAction::StaticFiles {
                root_directory,
                path_mapping,
                index_files,
                internal_index_redirects: true,
                directory_redirects: true,
                spa_fallback: None,
                try_files,
                autoindex,
                autoindex_exact_size,
                autoindex_local_time,
                etag,
                mime,
                headers,
                error_responses,
            },
            origins,
        ))
    }

}
