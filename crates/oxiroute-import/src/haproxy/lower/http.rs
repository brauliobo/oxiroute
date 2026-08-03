use std::collections::{HashMap, HashSet};

use oxiroute_config::{
    HttpHostSelector, HttpLiteralHeader, HttpPathSelector, HttpProxyPolicy, HttpRedirectLocation,
    HttpRequestHeaderValue, HttpRoute, HttpRouteAction, HttpRoutePolicy, HttpService,
};

use crate::ProvenanceSpan;

use super::{Lowerer, Representability};
use crate::haproxy::{
    AclCriterion, AclDefinition, BackendReference, ConditionPolarity, EffectiveFrontend,
    EffectiveListen, EffectiveSection, EffectiveValue, HttpRequestRule, SectionId, UseBackend,
};

use super::provenance::{
    CanonicalPath, deduplicate_sources, extend_sources, provenance_sources, section_sources,
};

pub(super) struct LoweredHttpService {
    service: HttpService,
    sources: Vec<ProvenanceSpan>,
    routes: Vec<Vec<ProvenanceSpan>>,
}

#[derive(Clone)]
struct LoweredRoute {
    route: HttpRoute,
    matcher: RouteMatcher,
    target: Option<SectionId>,
    sources: Vec<ProvenanceSpan>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RouteMatcher {
    host: Option<HttpHostSelector>,
    path_prefix: String,
}

impl RouteMatcher {
    fn score(&self) -> (u8, usize) {
        (u8::from(self.host.is_some()), self.path_prefix.len())
    }
}

impl Lowerer<'_> {
    pub(super) fn lower_http_frontend(
        &mut self,
        frontend: &EffectiveFrontend,
        name: &str,
    ) -> Option<LoweredHttpService> {
        if frontend
            .settings
            .http_request_rules
            .iter()
            .any(|rule| is_terminal_rule(&rule.value))
        {
            if has_conditional_terminal_rule(&frontend.settings.http_request_rules) {
                return self.lower_conditional_terminal_http_service(
                    &frontend.section,
                    &frontend.settings,
                    &frontend.acls,
                    frontend.settings.default_backend.as_ref(),
                    name,
                );
            }
            return self.lower_terminal_http_service(&frontend.section, &frontend.settings, name);
        }
        let routes = self.lower_routes(
            &frontend.section,
            &frontend.acls,
            &frontend.use_backends,
            frontend.settings.default_backend.as_ref(),
            None,
        );
        let target_ids = routes
            .iter()
            .filter_map(|route| route.target)
            .collect::<HashSet<_>>();
        if routes.is_empty() {
            return None;
        }
        let (connect_timeout_ms, upstream_io_timeout_ms, mut sources) =
            self.lower_http_policy(&frontend.section, &frontend.settings, &target_ids)?;
        let policies = self.lower_proxy_policies(&frontend.settings, &target_ids)?;
        let routes = routes
            .into_iter()
            .map(|mut route| {
                route.route.policy = HttpRoutePolicy {
                    max_request_body_bytes: None,
                    connect_timeout_ms,
                    read_timeout_ms: upstream_io_timeout_ms,
                    write_timeout_ms: upstream_io_timeout_ms,
                    request_buffering: false,
                    response_buffering: false,
                };
                if let Some(target) = route.target {
                    route.route.action = proxy_action(
                        self.section_name(target)
                            .expect("lowered route target has a canonical name"),
                        policies
                            .get(&target)
                            .expect("every route target has a policy")
                            .clone(),
                    );
                }
                route.sources.extend(sources.clone());
                deduplicate_sources(&mut route.sources);
                route
            })
            .collect::<Vec<_>>();
        let route_sources = routes
            .iter()
            .map(|route| route.sources.clone())
            .collect::<Vec<_>>();
        sources.extend(section_sources(&frontend.section));
        for route in &routes {
            sources.extend(route.sources.clone());
        }
        deduplicate_sources(&mut sources);
        Some(LoweredHttpService {
            service: HttpService {
                name: name.to_owned(),
                routes: routes.into_iter().map(|route| route.route).collect(),
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

    pub(super) fn lower_http_listen(
        &mut self,
        listen: &EffectiveListen,
        name: &str,
    ) -> Option<LoweredHttpService> {
        if listen
            .settings
            .http_request_rules
            .iter()
            .any(|rule| is_terminal_rule(&rule.value))
        {
            if has_conditional_terminal_rule(&listen.settings.http_request_rules) {
                return self.lower_conditional_terminal_http_service(
                    &listen.section,
                    &listen.settings,
                    &listen.acls,
                    None,
                    name,
                );
            }
            return self.lower_terminal_http_service(&listen.section, &listen.settings, name);
        }
        let routes = self.lower_routes(
            &listen.section,
            &listen.acls,
            &listen.use_backends,
            listen.settings.default_backend.as_ref(),
            Some(listen.section.id),
        );
        let target_ids = routes
            .iter()
            .filter_map(|route| route.target)
            .collect::<HashSet<_>>();
        if routes.is_empty() {
            return None;
        }
        let (connect_timeout_ms, upstream_io_timeout_ms, mut sources) =
            self.lower_http_policy(&listen.section, &listen.settings, &target_ids)?;
        let policies = self.lower_proxy_policies(&listen.settings, &target_ids)?;
        let routes = routes
            .into_iter()
            .map(|mut route| {
                route.route.policy = HttpRoutePolicy {
                    max_request_body_bytes: None,
                    connect_timeout_ms,
                    read_timeout_ms: upstream_io_timeout_ms,
                    write_timeout_ms: upstream_io_timeout_ms,
                    request_buffering: false,
                    response_buffering: false,
                };
                if let Some(target) = route.target {
                    route.route.action = proxy_action(
                        self.section_name(target)
                            .expect("lowered route target has a canonical name"),
                        policies
                            .get(&target)
                            .expect("every route target has a policy")
                            .clone(),
                    );
                }
                route.sources.extend(sources.clone());
                deduplicate_sources(&mut route.sources);
                route
            })
            .collect::<Vec<_>>();
        let route_sources = routes
            .iter()
            .map(|route| route.sources.clone())
            .collect::<Vec<_>>();
        sources.extend(section_sources(&listen.section));
        for route in &routes {
            sources.extend(route.sources.clone());
        }
        deduplicate_sources(&mut sources);
        Some(LoweredHttpService {
            service: HttpService {
                name: name.to_owned(),
                routes: routes.into_iter().map(|route| route.route).collect(),
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

    pub(super) fn commit_http_service(&mut self, candidate: LoweredHttpService) {
        let service_index = self.draft.http_services.len();
        let service_path = CanonicalPath::indexed("http_services", service_index);
        self.record(service_path.clone(), candidate.sources.clone());
        self.record(
            service_path.field("automatic_response_headers"),
            candidate.sources.clone(),
        );
        self.record(
            service_path.field("upstream_io_timeout_ms"),
            candidate.sources.clone(),
        );
        self.record(
            service_path.field("max_request_body_bytes"),
            candidate.sources.clone(),
        );
        self.draft.http_services.push(candidate.service);
        let routes_path = service_path.field("routes");
        for (route_index, sources) in candidate.routes.into_iter().enumerate() {
            let route_path = routes_path.index(route_index);
            self.record(route_path.clone(), sources.clone());
            let route = self.draft.http_services[service_index].routes[route_index].clone();
            if route.host.is_some() {
                let host_path = route_path.field("host");
                self.record(host_path.clone(), sources.clone());
                self.record(host_path.field("kind"), sources.clone());
                self.record(host_path.field("value"), sources.clone());
            }
            self.record(route_path.field("path"), sources.clone());
            self.record(route_path.field("path").field("kind"), sources.clone());
            self.record(route_path.field("path").field("value"), sources.clone());
            self.record(route_path.field("methods"), sources.clone());
            self.record(route_path.field("action"), sources.clone());
            self.record(route_path.field("action").field("type"), sources.clone());
            let action_path = route_path.field("action");
            match &route.action {
                HttpRouteAction::Proxy { .. } => {
                    self.record(action_path.field("upstream_pool"), sources.clone());
                    let policy_path = action_path.field("policy");
                    self.record(policy_path.clone(), sources.clone());
                    for field in [
                        "upstream_host",
                        "request_headers",
                        "response_headers",
                        "response_cookie_path_rewrites",
                        "retry",
                    ] {
                        self.record(policy_path.field(field), sources.clone());
                    }
                    self.record(
                        policy_path.field("retry").field("max_retries"),
                        sources.clone(),
                    );
                    self.record(
                        policy_path.field("retry").field("triggers"),
                        sources.clone(),
                    );
                }
                HttpRouteAction::FixedResponse { .. } => {
                    for field in ["status", "body", "headers"] {
                        self.record(action_path.field(field), sources.clone());
                    }
                }
                HttpRouteAction::Redirect { .. } => {
                    self.record(action_path.field("status"), sources.clone());
                    self.record(action_path.field("location"), sources.clone());
                }
                HttpRouteAction::StaticFiles { .. } => unreachable!("HAProxy has no static action"),
            }
            self.record_http_action_items(&route_path, &route, &sources);
        }
    }

    fn record_http_action_items(
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

    fn lower_terminal_http_service(
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

    fn lower_conditional_terminal_http_service(
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

    fn lower_conditional_fixed_response_routes(
        &mut self,
        rule: &EffectiveValue<HttpRequestRule>,
        acls: &[EffectiveValue<AclDefinition>],
    ) -> Option<(Vec<HttpRoute>, Vec<Vec<ProvenanceSpan>>)> {
        let HttpRequestRule::FixedResponse {
            status,
            body,
            content_type,
            condition: Some(condition),
        } = &rule.value
        else {
            self.block_value(
                rule,
                "only a conditional HAProxy fixed response has an exact canonical route action",
            );
            return None;
        };
        if condition.polarity != ConditionPolarity::If || condition.condition_negated {
            self.block_value(
                rule,
                "negated HAProxy fixed-response conditions have no simple canonical route matcher",
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
        let definitions = acls
            .iter()
            .map(|acl| (acl.provenance.origin, acl))
            .collect::<HashMap<_, _>>();
        let mut routes = Vec::new();
        let mut route_sources = Vec::new();
        for occurrence in &condition.condition.definitions {
            let Some(acl) = definitions.get(occurrence) else {
                self.block_value(rule, "HAProxy fixed-response ACL definition is unavailable");
                return None;
            };
            if acl.value.criterion != AclCriterion::PathExact {
                self.block_value(
                    acl,
                    "HAProxy fixed-response condition is not an exact path ACL",
                );
                return None;
            }
            for value in &acl.value.values {
                let Some(value) = std::str::from_utf8(value)
                    .ok()
                    .filter(|value| value.is_ascii() && value.starts_with('/'))
                else {
                    self.block_value(
                        acl,
                        "HAProxy exact path ACL is not an absolute canonical ASCII path",
                    );
                    return None;
                };
                let path = if acl.value.case_insensitive {
                    HttpPathSelector::AsciiCaseInsensitiveExact {
                        value: value.into(),
                    }
                } else {
                    HttpPathSelector::Exact {
                        value: value.into(),
                    }
                };
                let mut sources = provenance_sources(&rule.provenance);
                extend_sources(&mut sources, &acl.provenance);
                routes.push(HttpRoute {
                    host: None,
                    path,
                    methods: Vec::new(),
                    access_policy: None,
                    policy: HttpRoutePolicy::default(),
                    action: HttpRouteAction::FixedResponse {
                        status: *status,
                        body: body.into(),
                        headers: headers.clone(),
                    },
                });
                route_sources.push(sources);
            }
        }
        Some((routes, route_sources))
    }

    fn lower_routes(
        &mut self,
        section: &EffectiveSection,
        acls: &[EffectiveValue<AclDefinition>],
        rules: &[EffectiveValue<UseBackend>],
        default_backend: Option<&EffectiveValue<BackendReference>>,
        implicit_backend: Option<SectionId>,
    ) -> Vec<LoweredRoute> {
        let (mut routes, conditional_routes_representable) =
            self.lower_conditional_routes(acls, rules);
        let default_representable =
            self.append_default_route(section, default_backend, implicit_backend, &mut routes);
        if !conditional_routes_representable
            || !default_representable
            || !self.route_precedence_equivalent(section, &routes)
        {
            return Vec::new();
        }
        deduplicate_routes(routes)
    }

    fn lower_conditional_routes(
        &mut self,
        acls: &[EffectiveValue<AclDefinition>],
        rules: &[EffectiveValue<UseBackend>],
    ) -> (Vec<LoweredRoute>, bool) {
        let definitions = acls
            .iter()
            .map(|acl| (acl.provenance.origin, acl))
            .collect::<HashMap<_, _>>();
        let mut routes = Vec::new();
        let mut decision = Representability::new(true);
        for rule in rules {
            if rule.value.polarity != ConditionPolarity::If || rule.value.condition_negated {
                self.block_value(
                    rule,
                    "negated HAProxy use_backend conditions have no simple canonical route matcher",
                );
                decision.require(false);
                continue;
            }
            if !self.lowered_pools.contains(&rule.value.backend.target) {
                self.block_value(
                    rule,
                    "HAProxy use_backend target did not lower to a complete canonical pool",
                );
                decision.require(false);
                continue;
            }
            let Some(pool) = self.section_name(rule.value.backend.target) else {
                self.block_value(rule, "HAProxy use_backend target has no canonical name");
                decision.require(false);
                continue;
            };
            for occurrence in &rule.value.condition.definitions {
                let Some(acl) = definitions.get(occurrence) else {
                    self.block_value(rule, "HAProxy use_backend ACL definition is unavailable");
                    decision.require(false);
                    continue;
                };
                for value in &acl.value.values {
                    let Some(matcher) = self.lower_acl_matcher(acl, value) else {
                        decision.require(false);
                        continue;
                    };
                    let mut sources = provenance_sources(&rule.provenance);
                    extend_sources(&mut sources, &acl.provenance);
                    routes.push(LoweredRoute {
                        route: HttpRoute {
                            host: matcher.host.clone(),
                            path: HttpPathSelector::RawPrefix {
                                value: matcher.path_prefix.clone(),
                            },
                            methods: Vec::new(),
                            access_policy: None,
                            policy: HttpRoutePolicy::default(),
                            action: proxy_action(pool.clone(), HttpProxyPolicy::default()),
                        },
                        matcher,
                        target: Some(rule.value.backend.target),
                        sources,
                    });
                }
            }
        }
        (routes, decision.is_complete())
    }

    fn lower_acl_matcher(
        &mut self,
        acl: &EffectiveValue<AclDefinition>,
        value: &[u8],
    ) -> Option<RouteMatcher> {
        let value = std::str::from_utf8(value).ok();
        match acl.value.criterion {
            AclCriterion::HostExact => {
                let Some(value) = value else {
                    self.block_value(
                        acl,
                        "HAProxy hdr(host) value is not canonical UTF-8 authority text",
                    );
                    return None;
                };
                Some(RouteMatcher {
                    host: Some(if acl.value.case_insensitive {
                        HttpHostSelector::AsciiCaseInsensitiveExactAuthority {
                            value: value.to_owned(),
                        }
                    } else {
                        HttpHostSelector::ExactAuthority {
                            value: value.to_owned(),
                        }
                    }),
                    path_prefix: "/".into(),
                })
            }
            AclCriterion::PathExact => {
                self.block_value(
                    acl,
                    "HAProxy exact path ACL is supported only for conditional fixed responses",
                );
                None
            }
            AclCriterion::PathPrefix => {
                if acl.value.case_insensitive {
                    self.block_value(
                        acl,
                        "case-insensitive HAProxy path prefix matching is not canonical",
                    );
                    return None;
                }
                let Some(value) = value.filter(|value| value.starts_with('/')) else {
                    self.block_value(
                        acl,
                        "HAProxy path_beg value is not an absolute canonical path",
                    );
                    return None;
                };
                Some(RouteMatcher {
                    host: None,
                    path_prefix: value.to_owned(),
                })
            }
        }
    }

    fn append_default_route(
        &mut self,
        section: &EffectiveSection,
        default_backend: Option<&EffectiveValue<BackendReference>>,
        implicit_backend: Option<SectionId>,
        routes: &mut Vec<LoweredRoute>,
    ) -> bool {
        if let Some(reference) = default_backend {
            if !self.lowered_pools.contains(&reference.value.target) {
                self.block_value(
                    reference,
                    "HAProxy default_backend target did not lower to a complete canonical pool; no fallback route will be inserted",
                );
                return false;
            }
            if let Some(pool) = self.section_name(reference.value.target) {
                routes.push(LoweredRoute {
                    route: HttpRoute {
                        host: None,
                        path: HttpPathSelector::RawPrefix { value: "/".into() },
                        methods: Vec::new(),
                        access_policy: None,
                        policy: HttpRoutePolicy::default(),
                        action: proxy_action(pool, HttpProxyPolicy::default()),
                    },
                    matcher: RouteMatcher {
                        host: None,
                        path_prefix: "/".into(),
                    },
                    target: Some(reference.value.target),
                    sources: provenance_sources(&reference.provenance),
                });
            } else {
                self.block_value(
                    reference,
                    "HAProxy default_backend target has no canonical name",
                );
                return false;
            }
        } else if let Some(target) = implicit_backend {
            if !self.lowered_pools.contains(&target) {
                self.block_section(
                    section,
                    "HAProxy listen backend did not lower to a complete canonical pool; no fallback route will be inserted",
                );
                return false;
            }
            let Some(pool) = self.section_name(target) else {
                self.block_section(section, "HAProxy listen backend has no canonical name");
                return false;
            };
            routes.push(LoweredRoute {
                route: HttpRoute {
                    host: None,
                    path: HttpPathSelector::RawPrefix { value: "/".into() },
                    methods: Vec::new(),
                    access_policy: None,
                    policy: HttpRoutePolicy::default(),
                    action: proxy_action(pool, HttpProxyPolicy::default()),
                },
                matcher: RouteMatcher {
                    host: None,
                    path_prefix: "/".into(),
                },
                target: Some(target),
                sources: section_sources(section),
            });
        } else {
            routes.push(LoweredRoute {
                route: HttpRoute {
                    host: None,
                    path: HttpPathSelector::RawPrefix { value: "/".into() },
                    methods: Vec::new(),
                    access_policy: None,
                    policy: HttpRoutePolicy::default(),
                    action: HttpRouteAction::FixedResponse {
                        status: 503,
                        body: String::new(),
                        headers: Vec::new(),
                    },
                },
                matcher: RouteMatcher {
                    host: None,
                    path_prefix: "/".into(),
                },
                target: None,
                sources: section_sources(section),
            });
        }
        true
    }

    fn route_precedence_equivalent(
        &mut self,
        section: &EffectiveSection,
        routes: &[LoweredRoute],
    ) -> bool {
        for (earlier_index, earlier) in routes.iter().enumerate() {
            for later in &routes[earlier_index + 1..] {
                if !matchers_overlap(&earlier.matcher, &later.matcher)
                    || route_pool(&earlier.route) == route_pool(&later.route)
                {
                    continue;
                }
                if earlier.matcher == later.matcher {
                    self.block_section(
                        section,
                        "HAProxy first-match duplicate routes are rejected by canonical route validation",
                    );
                    return false;
                }
                if earlier.matcher.score() < later.matcher.score() {
                    self.block_section(
                        section,
                        "HAProxy first-match use_backend order disagrees with canonical host/longest-prefix precedence",
                    );
                    return false;
                }
            }
        }
        true
    }
}

fn matchers_overlap(left: &RouteMatcher, right: &RouteMatcher) -> bool {
    let hosts_overlap = match (&left.host, &right.host) {
        (Some(left), Some(right)) => match (host_value(left), host_value(right)) {
            (Some(left), Some(right)) => left == right,
            _ => true,
        },
        _ => true,
    };
    hosts_overlap && raw_prefixes_overlap(&left.path_prefix, &right.path_prefix)
}

fn host_value(selector: &HttpHostSelector) -> Option<&str> {
    match selector {
        HttpHostSelector::NormalizedHost { value } | HttpHostSelector::ExactAuthority { value } => {
            Some(value)
        }
        HttpHostSelector::AsciiCaseInsensitiveExactAuthority { value } => Some(value),
        HttpHostSelector::NginxLeadingWildcard { .. }
        | HttpHostSelector::NginxLeadingDot { .. } => None,
    }
}

fn raw_prefixes_overlap(left: &str, right: &str) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn deduplicate_routes(routes: Vec<LoweredRoute>) -> Vec<LoweredRoute> {
    let mut deduplicated: Vec<LoweredRoute> = Vec::new();
    for route in routes {
        if let Some(existing) = deduplicated.iter_mut().find(|existing| {
            existing.matcher == route.matcher
                && route_pool(&existing.route) == route_pool(&route.route)
        }) {
            existing.sources.extend(route.sources);
        } else {
            deduplicated.push(route);
        }
    }
    deduplicated
}

fn proxy_action(upstream_pool: String, policy: HttpProxyPolicy) -> HttpRouteAction {
    HttpRouteAction::Proxy {
        upstream_pool,
        policy,
    }
}

fn is_terminal_rule(rule: &HttpRequestRule) -> bool {
    matches!(
        rule,
        HttpRequestRule::Redirect { .. } | HttpRequestRule::FixedResponse { .. }
    )
}

fn has_conditional_terminal_rule(rules: &[EffectiveValue<HttpRequestRule>]) -> bool {
    rules.iter().any(|rule| {
        matches!(
            rule.value,
            HttpRequestRule::FixedResponse {
                condition: Some(_),
                ..
            }
        )
    })
}

fn route_pool(route: &HttpRoute) -> Option<&str> {
    match &route.action {
        HttpRouteAction::Proxy { upstream_pool, .. } => Some(upstream_pool),
        _ => None,
    }
}
