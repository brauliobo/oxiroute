#[allow(clippy::wildcard_imports)]
use super::*;

impl Lowerer<'_> {
    pub(super) fn record_http_action_items(
        &mut self,
        route_path: &CanonicalPath,
        route: &HttpRoute,
        sources: &[ProvenanceSpan],
    ) {
        match &route.action {
            HttpRouteAction::Proxy { policy, .. } => {
                let policy_path = route_path.field("action").field("policy");
                self.record(
                    policy_path.field("upstream_host").field("type"),
                    sources.to_vec(),
                );
                if matches!(
                    policy.upstream_host,
                    oxiroute_config::HttpUpstreamHost::Literal { .. }
                ) {
                    self.record(
                        policy_path.field("upstream_host").field("value"),
                        sources.to_vec(),
                    );
                }
                for (index, mutation) in policy.request_headers.iter().enumerate() {
                    let path = policy_path.field("request_headers").index(index);
                    self.record(path.clone(), sources.to_vec());
                    self.record(path.field("operation"), sources.to_vec());
                    self.record(path.field("name"), sources.to_vec());
                    if let oxiroute_config::HttpRequestHeaderMutation::Set { value, .. } = mutation
                    {
                        self.record(path.field("value"), sources.to_vec());
                        self.record(path.field("value").field("type"), sources.to_vec());
                        if matches!(value, HttpRequestHeaderValue::Literal { .. }) {
                            self.record(path.field("value").field("value"), sources.to_vec());
                        }
                    }
                }
                for (index, mutation) in policy.response_headers.iter().enumerate() {
                    let path = policy_path.field("response_headers").index(index);
                    self.record(path.clone(), sources.to_vec());
                    self.record(path.field("operation"), sources.to_vec());
                    self.record(path.field("name"), sources.to_vec());
                    if matches!(
                        mutation,
                        oxiroute_config::HttpResponseHeaderMutation::Set { .. }
                    ) {
                        self.record(path.field("value"), sources.to_vec());
                        self.record(path.field("always"), sources.to_vec());
                    }
                }
                for index in 0..policy.response_cookie_path_rewrites.len() {
                    let path = policy_path
                        .field("response_cookie_path_rewrites")
                        .index(index);
                    self.record(path.clone(), sources.to_vec());
                    self.record(path.field("from"), sources.to_vec());
                    self.record(path.field("to"), sources.to_vec());
                }
                let retry_path = policy_path.field("retry");
                self.record(retry_path.field("method_safety"), sources.to_vec());
                self.record(retry_path.field("body_safety"), sources.to_vec());
                for index in 0..policy.retry.triggers.len() {
                    self.record(retry_path.field("triggers").index(index), sources.to_vec());
                }
                for index in 0..policy.retry.response_statuses.len() {
                    self.record(
                        retry_path.field("response_statuses").index(index),
                        sources.to_vec(),
                    );
                }
            }
            HttpRouteAction::FixedResponse { headers, .. } => {
                for index in 0..headers.len() {
                    let path = route_path.field("action").field("headers").index(index);
                    self.record(path.clone(), sources.to_vec());
                    self.record(path.field("name"), sources.to_vec());
                    self.record(path.field("value"), sources.to_vec());
                }
            }
            HttpRouteAction::Redirect { .. } => {
                let path = route_path.field("action").field("location");
                self.record(path.field("type"), sources.to_vec());
                self.record(path.field("value"), sources.to_vec());
            }
            HttpRouteAction::StaticFiles { .. } => unreachable!("HAProxy has no static action"),
        }
    }

    pub(super) fn lower_terminal_http_service(
        &mut self,
        section: &EffectiveSection,
        settings: &crate::haproxy::ProxySettings,
        name: &str,
    ) -> Option<LoweredHttpService> {
        if settings.http_request_rules.len() != 1 || !settings.http_response_rules.is_empty() {
            self.block_section(
                section,
                "HAProxy ordered terminal and header rules are not one canonical route action",
            );
            return None;
        }
        let rule = &settings.http_request_rules[0];
        let action = match &rule.value {
            HttpRequestRule::Redirect { status, location } => {
                let Ok(location) = std::str::from_utf8(location) else {
                    self.block_value(rule, "HAProxy redirect location is not canonical UTF-8");
                    return None;
                };
                HttpRouteAction::Redirect {
                    status: *status,
                    location: HttpRedirectLocation::Literal {
                        value: location.into(),
                    },
                    headers: Vec::new(),
                }
            }
            HttpRequestRule::FixedResponse {
                status,
                body,
                content_type,
                condition,
            } => {
                if condition.is_some() {
                    self.block_value(
                        rule,
                        "conditional HAProxy fixed response requires a representable fallback route",
                    );
                    return None;
                }
                let Ok(body) = std::str::from_utf8(body) else {
                    self.block_value(rule, "HAProxy fixed response body is not canonical UTF-8");
                    return None;
                };
                let headers = if let Some(content_type) = content_type {
                    let Ok(content_type) = std::str::from_utf8(content_type) else {
                        self.block_value(rule, "HAProxy fixed response content type is not UTF-8");
                        return None;
                    };
                    vec![HttpLiteralHeader {
                        name: "content-type".into(),
                        value: content_type.into(),
                        always: true,
                    }]
                } else {
                    Vec::new()
                };
                HttpRouteAction::FixedResponse {
                    status: *status,
                    body: body.into(),
                    headers,
                }
            }
            HttpRequestRule::SetHeader { .. } | HttpRequestRule::RemoveHeader { .. } => {
                return None;
            }
        };
        let mut sources = section_sources(section);
        extend_sources(&mut sources, &rule.provenance);
        Some(LoweredHttpService {
            service: HttpService {
                drain_deadline_ms: None,
                name: name.into(),
                routes: vec![HttpRoute {
                    host: None,
                    path: HttpPathSelector::RawPrefix { value: "/".into() },
                    methods: Vec::new(),
                    access_policy: None,
                    policy: HttpRoutePolicy {
                        max_request_body_bytes: None,
                        connect_timeout_ms: 30_000,
                        read_timeout_ms: 30_000,
                        write_timeout_ms: 30_000,
                        request_buffering: false,
                        response_buffering: false,
                    },
                    action,
                }],
                automatic_response_headers: false,
                upstream_io_timeout_ms: 30_000,
                max_request_body_bytes: None,
                gzip: None,
                access_log: None,
            },
            sources: sources.clone(),
            routes: vec![sources],
        })
    }

    pub(super) fn lower_conditional_terminal_http_service(
        &mut self,
        section: &EffectiveSection,
        settings: &crate::haproxy::ProxySettings,
        acls: &[EffectiveValue<AclDefinition>],
        default_backend: Option<&EffectiveValue<BackendReference>>,
        name: &str,
    ) -> Option<LoweredHttpService> {
        if settings.http_request_rules.len() != 1 || !settings.http_response_rules.is_empty() {
            self.block_section(
                section,
                "HAProxy ordered conditional terminal and header rules are not one canonical route action",
            );
            return None;
        }
        let rule = &settings.http_request_rules[0];
        let (mut routes, mut route_sources) =
            self.lower_conditional_fixed_response_routes(rule, acls)?;
        let Some(backend) = default_backend else {
            self.block_section(
                section,
                "HAProxy conditional fixed response requires an explicit fallback backend",
            );
            return None;
        };
        if !self.lowered_pools.contains(&backend.value.target) {
            self.block_value(
                backend,
                "HAProxy conditional fixed-response fallback backend did not lower to a complete canonical pool",
            );
            return None;
        }
        let pool = self.section_name(backend.value.target)?;
        let targets = HashSet::from([backend.value.target]);
        let (connect_timeout_ms, upstream_io_timeout_ms, mut sources) =
            self.lower_http_policy(section, settings, &targets)?;
        let policies = self.lower_proxy_policies(settings, &targets)?;
        routes.push(HttpRoute {
            host: None,
            path: HttpPathSelector::RawPrefix { value: "/".into() },
            methods: Vec::new(),
            access_policy: None,
            policy: HttpRoutePolicy {
                max_request_body_bytes: None,
                connect_timeout_ms,
                read_timeout_ms: upstream_io_timeout_ms,
                write_timeout_ms: upstream_io_timeout_ms,
                request_buffering: false,
                response_buffering: false,
            },
            action: proxy_action(
                pool,
                policies
                    .get(&backend.value.target)
                    .expect("fallback target has a policy")
                    .clone(),
            ),
        });
        route_sources.push(provenance_sources(&backend.provenance));
        sources.extend(section_sources(section));
        for route_source in &route_sources {
            sources.extend(route_source.clone());
        }
        deduplicate_sources(&mut sources);
        Some(LoweredHttpService {
            service: HttpService {
                drain_deadline_ms: None,
                name: name.into(),
                routes,
                automatic_response_headers: false,
                upstream_io_timeout_ms,
                max_request_body_bytes: None,
                gzip: None,
                access_log: None,
            },
            sources,
            routes: route_sources,
        })
    }
}
