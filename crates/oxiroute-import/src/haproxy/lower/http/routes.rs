#[allow(clippy::wildcard_imports)]
use super::*;

impl Lowerer<'_> {
    pub(super) fn lower_conditional_fixed_response_routes(
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

    pub(super) fn lower_routes(
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

    #[allow(clippy::too_many_lines)]
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
            if rule.value.conditions.is_empty() || rule.value.conditions.len() > 2 {
                self.block_value(
                    rule,
                    "HAProxy use_backend ACL conjunctions support at most one exact Host and one path_beg criterion",
                );
                decision.require(false);
                continue;
            }
            let mut has_host = false;
            let mut has_path = false;
            let mut variants = vec![(
                RouteMatcher {
                    host: None,
                    path_prefix: "/".into(),
                },
                Vec::new(),
            )];
            for condition in &rule.value.conditions {
                for occurrence in &condition.definitions {
                    let Some(acl) = definitions.get(occurrence) else {
                        self.block_value(rule, "HAProxy use_backend ACL definition is unavailable");
                        decision.require(false);
                        continue;
                    };
                    let duplicate = match acl.value.criterion {
                        AclCriterion::HostExact => {
                            let duplicate = has_host;
                            has_host = true;
                            duplicate
                        }
                        AclCriterion::PathPrefix => {
                            let duplicate = has_path;
                            has_path = true;
                            duplicate
                        }
                        AclCriterion::PathExact => false,
                    };
                    if duplicate {
                        self.block_value(
                            acl,
                            "HAProxy use_backend ACL conjunctions cannot repeat the same Host or path criterion",
                        );
                        decision.require(false);
                        continue;
                    }
                    let acl_variants = acl
                        .value
                        .values
                        .iter()
                        .filter_map(|value| {
                            self.lower_acl_matcher(acl, value)
                                .map(|matcher| (matcher, provenance_sources(&acl.provenance)))
                        })
                        .collect::<Vec<_>>();
                    if acl_variants.is_empty() {
                        decision.require(false);
                        continue;
                    }
                    let mut combined = Vec::new();
                    for (base, base_sources) in &variants {
                        for (matcher, acl_sources) in &acl_variants {
                            let mut sources = base_sources.clone();
                            sources.extend(acl_sources.clone());
                            deduplicate_sources(&mut sources);
                            combined.push((merge_route_matchers(base, matcher), sources));
                        }
                    }
                    variants = combined;
                }
            }
            for (matcher, matcher_sources) in variants {
                let mut sources = provenance_sources(&rule.provenance);
                sources.extend(matcher_sources);
                deduplicate_sources(&mut sources);
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
