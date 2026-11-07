#[allow(clippy::wildcard_imports)]
use super::*;

impl Lowerer<'_> {
    pub(in super::super) fn lower_http_frontend(
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
                drain_deadline_ms: None,
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

    pub(in super::super) fn lower_http_listen(
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
                drain_deadline_ms: None,
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

    pub(in super::super) fn commit_http_service(&mut self, candidate: LoweredHttpService) {
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
                    self.record(
                        policy_path.field("retry").field("response_statuses"),
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
}
