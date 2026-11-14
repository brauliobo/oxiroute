#[allow(clippy::wildcard_imports)]
use super::*;

impl Resolver<'_> {
    pub(super) fn resolve_upstream(&mut self, directive: &ExpandedDirective) -> EffectiveUpstream {
        let name = directive
            .directive
            .arguments
            .first()
            .map(|word| self.value(word));
        if directive.directive.arguments.len() == 1 && directive.children.is_some() {
            self.resolved(directive.occurrence);
        } else {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "upstream requires one name and a block",
            );
        }

        let mut servers = Vec::new();
        let mut endpoint_identities = HashMap::new();
        for child in directive.children.as_deref().unwrap_or_default() {
            if child.directive.name.value != b"server" {
                self.block_subtree(child, "directive is unsupported in an nginx upstream");
                continue;
            }
            let server = self.resolve_upstream_server(child);
            if let Some(endpoint) = &server.endpoint {
                if let Some(first) = endpoint_identities.get(endpoint).copied() {
                    self.block_related(
                        child.occurrence,
                        E_DUPLICATE_IDENTITY,
                        "duplicate endpoint identity in nginx upstream",
                        first,
                    );
                } else {
                    endpoint_identities.insert(endpoint.clone(), child.occurrence);
                }
            }
            servers.push(server);
        }
        if servers.is_empty() {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "upstream requires at least one static server",
            );
        }

        EffectiveUpstream {
            origin: Self::origin(directive),
            name,
            servers,
        }
    }

    fn resolve_upstream_server(
        &mut self,
        directive: &ExpandedDirective,
    ) -> EffectiveUpstreamServer {
        let address = directive
            .directive
            .arguments
            .first()
            .map(|word| self.value(word));
        let parameters = directive
            .directive
            .arguments
            .iter()
            .skip(1)
            .map(|word| self.value(word))
            .collect::<Vec<_>>();
        let endpoint = address
            .as_ref()
            .and_then(|address| parse_static_endpoint(&address.value, 80));

        let mut weight = None;
        let mut weight_seen = false;
        let mut parameter_issue: Option<(DiagnosticCode, &'static str)> = None;
        for parameter in &parameters {
            let Some(value) = parameter.value.strip_prefix(b"weight=") else {
                parameter_issue.get_or_insert((
                    E_UNSUPPORTED_FEATURE,
                    "upstream server parameters are unsupported except for static weight",
                ));
                continue;
            };
            if weight_seen {
                parameter_issue.get_or_insert((
                    E_INVALID_VALUE,
                    "upstream server weight may be specified only once",
                ));
                continue;
            }
            weight_seen = true;
            if has_variable(value) {
                parameter_issue.get_or_insert((
                    E_UNSUPPORTED_FEATURE,
                    "dynamic upstream server weights are unsupported",
                ));
                continue;
            }
            let parsed = std::str::from_utf8(value)
                .ok()
                .filter(|value| {
                    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
                })
                .and_then(|value| value.parse::<u16>().ok())
                .filter(|weight| (1..=MAX_UPSTREAM_WEIGHT).contains(weight));
            let Some(value) = parsed else {
                parameter_issue.get_or_insert((
                    E_INVALID_VALUE,
                    "upstream server weight must be an integer between 1 and 100",
                ));
                continue;
            };
            let mut origin = Self::origin(directive);
            origin.span = parameter.span;
            weight = Some(EffectiveUpstreamWeight { value, origin });
        }

        let outcome =
            if directive.directive.children.is_some() || directive.directive.arguments.is_empty() {
                Some((
                    E_INVALID_VALUE,
                    "upstream server requires a static address and a semicolon",
                ))
            } else if address
                .as_ref()
                .is_some_and(|address| has_variable(&address.value))
            {
                Some((
                    E_UNSUPPORTED_FEATURE,
                    "variables in upstream server addresses are unsupported",
                ))
            } else if endpoint.is_none() {
                Some((E_INVALID_VALUE, "invalid static upstream server address"))
            } else {
                parameter_issue
            };
        self.finish_occurrence(directive.occurrence, outcome);

        EffectiveUpstreamServer {
            origin: Self::origin(directive),
            address,
            endpoint,
            weight,
            parameters,
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "server declarations, synthetic routes, and identity checks resolve in source order"
    )]
    pub(super) fn resolve_server(
        &mut self,
        directive: &ExpandedDirective,
        upstreams: &HashMap<Vec<u8>, OccurrenceId>,
    ) -> EffectiveServer {
        if directive.directive.arguments.is_empty() && directive.children.is_some() {
            self.resolved(directive.occurrence);
        } else {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "server in the http context must be an argument-free block",
            );
        }

        let children = directive.children.as_deref().unwrap_or_default();
        let mut listens = Vec::new();
        let mut server_names = Vec::new();
        let mut locations = Vec::new();
        let mut declaration_order = Vec::new();
        let mut saw_listen = false;
        let mut name_identities = HashMap::new();
        let mut location_identities = HashMap::new();
        let mut scalar_policies = HashMap::new();

        for child in children {
            match child.directive.name.value.as_slice() {
                b"listen" => {
                    saw_listen = true;
                    declaration_order.push(ServerDeclaration::Listen(child.occurrence));
                    listens.push(self.resolve_listen(child));
                }
                b"server_name" => {
                    declaration_order.push(ServerDeclaration::ServerName(child.occurrence));
                    for name in self.resolve_server_names(child) {
                        if is_supported_server_name(name.kind) {
                            let identity = (name.kind, name.normalized.clone());
                            if let Some(first) = name_identities.get(&identity).copied() {
                                self.block_related(
                                    name.origin.occurrence,
                                    E_DUPLICATE_IDENTITY,
                                    "duplicate server_name identity in virtual server",
                                    first,
                                );
                            } else {
                                name_identities.insert(identity, name.origin.occurrence);
                            }
                        }
                        server_names.push(name);
                    }
                }
                b"location" => {
                    declaration_order.push(ServerDeclaration::Location(child.occurrence));
                    let location = self.resolve_location(child, None, upstreams);
                    if is_supported_location(location.kind)
                        && let Some(path) = &location.path
                    {
                        let identity = (location.kind.route_identity(), path.value.clone());
                        if let Some(first) = location_identities.get(&identity).copied() {
                            self.block_related(
                                location.origin.occurrence,
                                E_DUPLICATE_IDENTITY,
                                "duplicate location identity in virtual server",
                                first,
                            );
                        } else {
                            location_identities.insert(identity, location.origin.occurrence);
                        }
                    }
                    locations.push(location);
                }
                b"types" => self.resolve_types(child),
                b"if" => {
                    if self.resolve_supported_if(child, false) {
                        declaration_order.push(ServerDeclaration::Location(child.occurrence));
                        locations.push(Self::synthetic_if_location(child));
                    }
                }
                b"return" => {
                    declaration_order.push(ServerDeclaration::Location(child.occurrence));
                    self.resolve_policy(child);
                    self.reject_duplicate_scalar(child, &mut scalar_policies);
                    locations.push(Self::synthetic_if_location(child));
                }
                name if is_server_policy(name) => {
                    self.resolve_policy(child);
                    self.reject_duplicate_scalar(child, &mut scalar_policies);
                }
                _ => self.block_subtree(child, "directive is unsupported in an nginx HTTP server"),
            }
        }

        self.reject_prefix_no_regex_with_special_locations(&locations);
        if !saw_listen {
            listens.push(EffectiveListen {
                origin: Self::origin(directive),
                value: None,
                endpoint: Some(ListenEndpoint::Socket {
                    address: b"*".to_vec(),
                    port: 80,
                }),
                options: Vec::new(),
                default_server: false,
                implicit: true,
            });
        }

        EffectiveServer {
            origin: Self::origin(directive),
            declaration_order,
            listens,
            server_names,
            locations,
        }
    }

    fn resolve_listen(&mut self, directive: &ExpandedDirective) -> EffectiveListen {
        let value = directive
            .directive
            .arguments
            .first()
            .map(|word| self.value(word));
        let options = directive
            .directive
            .arguments
            .iter()
            .skip(1)
            .map(|word| self.value(word))
            .collect::<Vec<_>>();
        let endpoint = value
            .as_ref()
            .and_then(|value| parse_listen_endpoint(&value.value));
        let default_server = options
            .iter()
            .any(|option| matches!(option.value.as_slice(), b"default_server" | b"default"));
        let option_count = |expected: &[u8]| {
            options
                .iter()
                .filter(|option| option.value == expected)
                .count()
        };
        let default_option_count = options
            .iter()
            .filter(|option| matches!(option.value.as_slice(), b"default_server" | b"default"))
            .count();
        let unsupported_option = options.iter().any(|option| {
            !matches!(
                option.value.as_slice(),
                b"default_server" | b"default" | b"ssl" | b"http2"
            )
        });
        let repeated_option = option_count(b"ssl") > 1 || option_count(b"http2") > 1;

        let outcome =
            if directive.directive.children.is_some() || directive.directive.arguments.is_empty() {
                Some((
                    E_INVALID_VALUE,
                    "listen requires an address and a semicolon",
                ))
            } else if directive
                .directive
                .arguments
                .iter()
                .any(|argument| has_variable(&argument.value))
            {
                Some((E_UNSUPPORTED_FEATURE, "variables in listen are unsupported"))
            } else if endpoint.is_none() {
                Some((
                    E_INVALID_VALUE,
                    "unsupported or invalid nginx listen address",
                ))
            } else if default_option_count > 1 {
                Some((E_INVALID_VALUE, "listen repeats the default server option"))
            } else if repeated_option {
                Some((E_INVALID_VALUE, "listen repeats a protocol option"))
            } else if unsupported_option {
                Some((E_UNSUPPORTED_FEATURE, "nginx listen option is unsupported"))
            } else {
                None
            };
        self.finish_occurrence(directive.occurrence, outcome);

        EffectiveListen {
            origin: Self::origin(directive),
            value,
            endpoint,
            options,
            default_server,
            implicit: false,
        }
    }

    fn resolve_server_names(&mut self, directive: &ExpandedDirective) -> Vec<EffectiveServerName> {
        let names = directive
            .directive
            .arguments
            .iter()
            .map(|word| {
                let value = self.value(word);
                EffectiveServerName {
                    normalized: ascii_lowercase(&value.value),
                    kind: server_name_kind(&value.value),
                    origin: Self::origin(directive),
                    value,
                }
            })
            .collect::<Vec<_>>();

        let outcome = if directive.directive.children.is_some() || names.is_empty() {
            Some((
                E_INVALID_VALUE,
                "server_name requires at least one name and a semicolon",
            ))
        } else if names
            .iter()
            .any(|name| name.kind == ServerNameKind::Variable)
        {
            Some((
                E_UNSUPPORTED_FEATURE,
                "variables in server_name are unsupported",
            ))
        } else if names.iter().any(|name| name.kind == ServerNameKind::Regex) {
            Some((E_UNSUPPORTED_FEATURE, "regex server names are unsupported"))
        } else if names
            .iter()
            .any(|name| name.kind == ServerNameKind::Invalid)
        {
            Some((E_INVALID_VALUE, "invalid nginx server_name wildcard"))
        } else {
            None
        };
        self.finish_occurrence(directive.occurrence, outcome);
        names
    }
}
