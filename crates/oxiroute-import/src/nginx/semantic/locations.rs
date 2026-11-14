#[allow(clippy::wildcard_imports)]
use super::*;

impl Resolver<'_> {
    pub(super) fn resolve_location(
        &mut self,
        directive: &ExpandedDirective,
        inherited_proxy: Option<&EffectiveProxyPass>,
        upstreams: &HashMap<Vec<u8>, OccurrenceId>,
    ) -> EffectiveLocation {
        let (modifier, path, kind) = self.location_header(directive);
        let static_path_invalid = path
            .as_ref()
            .is_some_and(|path| is_supported_location(kind) && !path.value.starts_with(b"/"));
        let outcome = if directive.children.is_none() || path.is_none() || static_path_invalid {
            Some((E_INVALID_VALUE, "location requires a matcher and a block"))
        } else {
            match kind {
                LocationKind::Exact | LocationKind::Prefix | LocationKind::PrefixNoRegex => None,
                LocationKind::Variable => Some((
                    E_UNSUPPORTED_FEATURE,
                    "variables in location matchers are unsupported",
                )),
                LocationKind::Regex | LocationKind::RegexInsensitive => {
                    Some((E_UNSUPPORTED_FEATURE, "regex locations are unsupported"))
                }
                LocationKind::Named => {
                    Some((E_UNSUPPORTED_FEATURE, "named locations are unsupported"))
                }
                LocationKind::Invalid => Some((E_INVALID_VALUE, "invalid nginx location matcher")),
            }
        };
        self.finish_occurrence(directive.occurrence, outcome);

        let children = directive.children.as_deref().unwrap_or_default();
        let mut proxy_pass = None;
        for child in children {
            if child.directive.name.value != b"proxy_pass" {
                continue;
            }
            let parsed = self.resolve_proxy_pass(child, upstreams);
            if proxy_pass.is_some() {
                self.block(
                    child.occurrence,
                    E_DUPLICATE_IDENTITY,
                    "duplicate proxy_pass scalar in one location",
                );
            } else {
                proxy_pass = parsed;
            }
        }

        let (effective_proxy, proxy_pass_inherited) = if let Some(proxy_pass) = proxy_pass {
            (Some(proxy_pass), false)
        } else {
            (inherited_proxy.cloned(), inherited_proxy.is_some())
        };
        let mut nested_locations = Vec::new();
        let mut location_identities = HashMap::new();
        let mut scalar_policies = HashMap::new();
        for child in children {
            match child.directive.name.value.as_slice() {
                b"proxy_pass" => {}
                b"location" => {
                    let location =
                        self.resolve_location(child, effective_proxy.as_ref(), upstreams);
                    if is_supported_location(location.kind)
                        && let Some(path) = &location.path
                    {
                        let identity = (location.kind.route_identity(), path.value.clone());
                        if let Some(first) = location_identities.get(&identity).copied() {
                            self.block_related(
                                location.origin.occurrence,
                                E_DUPLICATE_IDENTITY,
                                "duplicate nested location identity",
                                first,
                            );
                        } else {
                            location_identities.insert(identity, location.origin.occurrence);
                        }
                    }
                    nested_locations.push(location);
                }
                b"types" => self.resolve_types(child),
                b"if" => {
                    self.resolve_supported_if(child, true);
                }
                name if is_location_policy(name) => {
                    self.resolve_policy(child);
                    self.reject_duplicate_scalar(child, &mut scalar_policies);
                }
                _ => self.block_subtree(child, "directive is unsupported in an nginx location"),
            }
        }

        EffectiveLocation {
            origin: Self::origin(directive),
            modifier,
            path,
            kind,
            proxy_pass: effective_proxy,
            proxy_pass_inherited,
            children: nested_locations,
        }
    }

    fn location_header(
        &self,
        directive: &ExpandedDirective,
    ) -> (Option<NginxValue>, Option<NginxValue>, LocationKind) {
        let arguments = &directive.directive.arguments;
        match arguments.as_slice() {
            [path] if path.value.starts_with(b"@") => {
                (None, Some(self.value(path)), LocationKind::Named)
            }
            [path] => {
                let kind = if has_variable(&path.value) {
                    LocationKind::Variable
                } else {
                    LocationKind::Prefix
                };
                (None, Some(self.value(path)), kind)
            }
            [modifier, path] => {
                let kind = if has_variable(&path.value) {
                    LocationKind::Variable
                } else {
                    match modifier.value.as_slice() {
                        b"=" => LocationKind::Exact,
                        b"^~" => LocationKind::PrefixNoRegex,
                        b"~" => LocationKind::Regex,
                        b"~*" => LocationKind::RegexInsensitive,
                        _ => LocationKind::Invalid,
                    }
                };
                (Some(self.value(modifier)), Some(self.value(path)), kind)
            }
            _ => (None, None, LocationKind::Invalid),
        }
    }

    fn resolve_proxy_pass(
        &mut self,
        directive: &ExpandedDirective,
        upstreams: &HashMap<Vec<u8>, OccurrenceId>,
    ) -> Option<EffectiveProxyPass> {
        let Some(word) = directive.directive.arguments.first() else {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "proxy_pass requires one URL and a semicolon",
            );
            return None;
        };
        let value = self.value(word);
        let parsed = parse_proxy_pass(&value.value);
        let invalid_url = parsed.is_none();
        let (scheme, authority, replacement_uri) =
            parsed.unwrap_or_else(|| (ProxyPassScheme::Unsupported(Vec::new()), Vec::new(), None));
        let variable = has_variable(&value.value);
        let bounded_downstream_scheme = scheme == ProxyPassScheme::Downstream
            && !has_variable(&authority)
            && replacement_uri
                .as_deref()
                .is_none_or(|uri| !has_variable(uri));
        let default_port = if scheme == ProxyPassScheme::Https {
            443
        } else {
            80
        };
        let direct_endpoint = parse_static_endpoint(&authority, default_port);
        let upstream = if variable && !bounded_downstream_scheme {
            UpstreamReference::Variable
        } else if let Some(upstream) = upstreams.get(&ascii_lowercase(&authority)).copied() {
            UpstreamReference::Resolved(upstream)
        } else if direct_endpoint.is_some() {
            UpstreamReference::Direct
        } else {
            UpstreamReference::Unresolved
        };

        let outcome =
            if directive.directive.children.is_some() || directive.directive.arguments.len() != 1 {
                Some((
                    E_INVALID_VALUE,
                    "proxy_pass requires one URL and a semicolon",
                ))
            } else if variable && !bounded_downstream_scheme {
                Some((
                    E_UNSUPPORTED_FEATURE,
                    "variables in proxy_pass are unsupported",
                ))
            } else if invalid_url {
                Some((
                    E_INVALID_VALUE,
                    "proxy_pass requires a static http or https URL",
                ))
            } else if matches!(scheme, ProxyPassScheme::Unsupported(_)) {
                Some((
                    E_UNSUPPORTED_FEATURE,
                    "proxy_pass URL scheme is unsupported",
                ))
            } else if upstream == UpstreamReference::Unresolved {
                Some((
                    E_UNRESOLVED_REFERENCE,
                    "proxy_pass upstream is not declared",
                ))
            } else {
                None
            };
        self.finish_occurrence(directive.occurrence, outcome);

        Some(EffectiveProxyPass {
            origin: Self::origin(directive),
            value,
            scheme,
            authority,
            replacement_uri,
            upstream,
            direct_endpoint,
        })
    }

    pub(super) fn resolve_policy(&mut self, directive: &ExpandedDirective) {
        let name = directive.directive.name.value.as_slice();
        let argument_count_valid =
            policy_argument_count_valid(name, directive.directive.arguments.len());
        let outcome = if directive.directive.children.is_some() || !argument_count_valid {
            Some((
                E_INVALID_VALUE,
                "supported nginx policy has an invalid directive shape",
            ))
        } else if !policy_allows_variables(name)
            && directive
                .directive
                .arguments
                .iter()
                .any(|argument| has_variable(&argument.value))
        {
            Some((
                E_UNSUPPORTED_FEATURE,
                "variables in nginx policy values are unsupported",
            ))
        } else if name == b"expires"
            && directive
                .directive
                .arguments
                .first()
                .is_none_or(|argument| argument.value != b"off")
        {
            Some((
                E_UNSUPPORTED_FEATURE,
                "nginx expires value is unsupported; only `expires off` is supported by the canonical HTTP response policy",
            ))
        } else {
            None
        };
        self.finish_occurrence(directive.occurrence, outcome);
    }

    pub(super) fn resolve_types(&mut self, directive: &ExpandedDirective) {
        if !directive.directive.arguments.is_empty() || directive.children.is_none() {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "types must be an argument-free block",
            );
            for child in directive.children.as_deref().unwrap_or_default() {
                self.structural_subtree(child);
            }
            return;
        }
        self.resolved(directive.occurrence);
        for mapping in directive.children.as_deref().unwrap_or_default() {
            let valid = mapping.directive.children.is_none()
                && !has_variable(&mapping.directive.name.value)
                && mapping
                    .directive
                    .arguments
                    .iter()
                    .all(|extension| !has_variable(&extension.value));
            if valid {
                self.resolved(mapping.occurrence);
            } else {
                self.block(
                    mapping.occurrence,
                    E_INVALID_VALUE,
                    "nginx MIME mapping requires a static content type and one or more extensions",
                );
            }
        }
    }

    /// `bearer_gate` admits the bearer-token `if`, which only a location can lower into an access policy.
    pub(super) fn resolve_supported_if(
        &mut self,
        directive: &ExpandedDirective,
        bearer_gate: bool,
    ) -> bool {
        let certbot_host_redirect = certbot_host_condition(&directive.directive).is_some();
        let children = directive.children.as_deref().unwrap_or_default();
        let bearer_gate = bearer_gate && bearer_gate_condition(&directive.directive);
        let supported = directive.children.is_some()
            && (certbot_host_redirect || bearer_gate)
            && children.iter().all(|child| {
                child.directive.name.value == b"return"
                    && (!bearer_gate
                        || child
                            .directive
                            .arguments
                            .iter()
                            .map(|word| word.value.as_slice())
                            .eq([b"401".as_slice()]))
            });
        if supported {
            self.resolved(directive.occurrence);
            for child in children {
                self.resolve_policy(child);
            }
        } else {
            self.block_subtree(
                directive,
                "nginx if condition is outside the bounded host-redirect or bearer-authorization subset",
            );
        }
        supported && certbot_host_redirect
    }

    /// `^~` equals a plain prefix only while no regex, named, or variable location can take precedence.
    pub(super) fn reject_prefix_no_regex_with_special_locations(
        &mut self,
        locations: &[EffectiveLocation],
    ) {
        if !locations.iter().any(Self::has_special_location) {
            return;
        }
        for location in locations
            .iter()
            .filter(|location| location.kind == LocationKind::PrefixNoRegex)
        {
            self.block(
                location.origin.occurrence,
                E_UNSUPPORTED_FEATURE,
                "the nginx ^~ location modifier is only a plain prefix in a server without regex, named, or variable locations",
            );
        }
    }

    fn has_special_location(location: &EffectiveLocation) -> bool {
        is_special_location(location.kind)
            || location.children.iter().any(Self::has_special_location)
    }

    pub(super) fn synthetic_if_location(directive: &ExpandedDirective) -> EffectiveLocation {
        EffectiveLocation {
            origin: Self::origin(directive),
            modifier: None,
            path: Some(NginxValue {
                value: b"/".to_vec(),
                raw: b"/".to_vec(),
                span: directive.directive.span,
            }),
            kind: LocationKind::Prefix,
            proxy_pass: None,
            proxy_pass_inherited: false,
            children: Vec::new(),
        }
    }

    pub(super) fn reject_duplicate_scalar(
        &mut self,
        directive: &ExpandedDirective,
        scalar_policies: &mut HashMap<Vec<u8>, OccurrenceId>,
    ) {
        if !is_scalar_policy(&directive.directive.name.value) {
            return;
        }
        if let Some(first) =
            scalar_policies.insert(directive.directive.name.value.clone(), directive.occurrence)
        {
            self.block_related(
                directive.occurrence,
                E_DUPLICATE_IDENTITY,
                "duplicate nginx scalar directive in one context",
                first,
            );
        }
    }
}
