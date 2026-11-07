#[allow(clippy::wildcard_imports)]
use super::*;

impl Analyzer<'_> {
    pub(super) fn acl(&mut self, expanded: &ExpandedDirective) {
        let [name, acl_type, values @ ..] = expanded.directive.arguments.as_slice() else {
            self.invalid(
                expanded,
                DirectiveFamily::Acl,
                DirectiveSemantics::AclUnsupported,
                "Squid acl requires a name, type, and value",
            );
            return;
        };
        let (acl_type, matchers, semantics, blocker) = match acl_type.value.as_slice() {
            b"src" => (
                AclType::Source,
                values
                    .iter()
                    .map(|value| bytes::ip_network(&value.value).map(AclMatcher::Source))
                    .collect::<Option<Vec<_>>>(),
                DirectiveSemantics::AclSource,
                SemanticBlockerKind::SourceAddressAcl,
            ),
            b"port" => (
                AclType::Port,
                values
                    .iter()
                    .map(|value| bytes::port_range(&value.value).map(AclMatcher::Port))
                    .collect::<Option<Vec<_>>>(),
                DirectiveSemantics::AclPort,
                SemanticBlockerKind::DestinationPortAcl,
            ),
            b"proxy_auth" => (
                AclType::ProxyAuth,
                Some(values.iter().map(proxy_auth_matcher).collect()),
                DirectiveSemantics::AclProxyAuth,
                SemanticBlockerKind::ProxyAuthenticationAcl,
            ),
            _ => {
                self.diagnostics.push(Self::diagnostic(
                    expanded,
                    E_UNSUPPORTED_FEATURE,
                    "Squid ACL type is not registered by the strict importer",
                ));
                self.record(
                    expanded,
                    DirectiveFamily::Acl,
                    DirectiveSemantics::AclUnsupported,
                    Activation::Blocked(SemanticBlockerKind::UnsupportedAclType),
                );
                return;
            }
        };
        let Some(matchers) = matchers.filter(|matchers| !matchers.is_empty()) else {
            self.invalid(
                expanded,
                DirectiveFamily::Acl,
                semantics,
                "Squid acl contains no valid matcher",
            );
            return;
        };
        self.effective.acl_definitions.push(AclDefinition {
            origin: origin(expanded),
            name: name.into(),
            acl_type,
            matchers,
        });
        self.record(
            expanded,
            DirectiveFamily::Acl,
            semantics,
            Activation::Blocked(blocker),
        );
    }

    pub(super) fn access(&mut self, expanded: &ExpandedDirective) {
        let kind = access_kind(&expanded.directive.name.value)
            .expect("classifier only calls access for registered access directives");
        self.parse_access(expanded, kind, None, &expanded.directive.arguments);
    }

    pub(super) fn header_access(&mut self, expanded: &ExpandedDirective) {
        let [selector, arguments @ ..] = expanded.directive.arguments.as_slice() else {
            self.invalid(
                expanded,
                DirectiveFamily::Access,
                DirectiveSemantics::HeaderAccess,
                "Squid header access requires a header selector and action",
            );
            return;
        };
        let kind = if expanded.directive.name.value == b"request_header_access" {
            AccessListKind::RequestHeader
        } else {
            AccessListKind::ReplyHeader
        };
        self.parse_access(expanded, kind, Some(selector.into()), arguments);
    }

    fn parse_access(
        &mut self,
        expanded: &ExpandedDirective,
        kind: AccessListKind,
        selector: Option<NativeValue>,
        arguments: &[Word],
    ) {
        let [action, terms @ ..] = arguments else {
            self.invalid(
                expanded,
                DirectiveFamily::Access,
                access_semantics(kind),
                "Squid access rule requires an allow or deny action",
            );
            return;
        };
        let action = match action.value.as_slice() {
            b"allow" => AccessAction::Allow,
            b"deny" => AccessAction::Deny,
            _ => {
                self.invalid(
                    expanded,
                    DirectiveFamily::Access,
                    access_semantics(kind),
                    "Squid access rule action must be allow or deny",
                );
                return;
            }
        };
        if terms.is_empty() {
            self.invalid(
                expanded,
                DirectiveFamily::Access,
                access_semantics(kind),
                "Squid access rule requires at least one ACL term",
            );
            return;
        }
        let terms = terms
            .iter()
            .map(|term| {
                let (negated, value) = term.value.strip_prefix(b"!").map_or_else(
                    || (false, term.value.clone()),
                    |value| (true, value.to_vec()),
                );
                AclTerm {
                    negated,
                    name: NativeValue {
                        value,
                        span: term.span,
                    },
                    resolution: AclReferenceResolution::Unresolved,
                }
            })
            .collect();
        self.effective.access_rules.push(AccessRule {
            origin: origin(expanded),
            kind,
            selector,
            action,
            terms,
            order: self.effective.access_rules.len(),
        });
        self.record(
            expanded,
            DirectiveFamily::Access,
            access_semantics(kind),
            Activation::Blocked(access_blocker(kind)),
        );
    }

    pub(super) fn merge_acls(&mut self) {
        let mut indexes = HashMap::<Vec<u8>, usize>::new();
        let mut conflicts = Vec::new();
        for definition in self.effective.acl_definitions.clone() {
            if let Some(index) = indexes.get(&definition.name.value).copied() {
                let effective = &mut self.effective.acls[index];
                if effective.acl_type == definition.acl_type {
                    effective.definitions.push(definition.origin.occurrence);
                    effective.matchers.extend(definition.matchers.clone());
                } else {
                    conflicts.push(definition.origin.occurrence);
                    self.diagnostics.push(
                        Diagnostic::new(
                            E_DUPLICATE_IDENTITY,
                            Severity::Error,
                            DiagnosticStage::Resolve,
                            "same-name Squid ACL declarations use conflicting types",
                        )
                        .with_primary_span(definition.origin.directive_span)
                        .with_include_stack(
                            definition
                                .origin
                                .provenance
                                .include_stack
                                .iter()
                                .map(|frame| frame.directive_span),
                        ),
                    );
                }
            } else {
                indexes.insert(definition.name.value.clone(), self.effective.acls.len());
                self.effective.acls.push(EffectiveAcl {
                    name: definition.name.value.clone(),
                    acl_type: definition.acl_type,
                    definitions: vec![definition.origin.occurrence],
                    matchers: definition.matchers.clone(),
                });
            }
        }
        for occurrence in conflicts {
            self.block_occurrence(occurrence, SemanticBlockerKind::ConflictingAclType);
        }
    }

    pub(super) fn resolve_access(&mut self) {
        let definitions = self
            .effective
            .acls
            .iter()
            .map(|acl| (acl.name.clone(), acl.definitions.clone()))
            .collect::<HashMap<_, _>>();
        let mut unresolved = Vec::new();
        for rule in &mut self.effective.access_rules {
            for term in &mut rule.terms {
                term.resolution = if let Some(builtin) = builtin_acl(&term.name.value) {
                    AclReferenceResolution::Builtin(builtin)
                } else if let Some(definitions) = definitions.get(&term.name.value) {
                    AclReferenceResolution::Defined(definitions.clone())
                } else {
                    unresolved.push(rule.origin.occurrence);
                    AclReferenceResolution::Unresolved
                };
            }
        }
        unresolved.sort_unstable();
        unresolved.dedup();
        for occurrence in unresolved {
            let expanded = &self.graph.expanded_directives[occurrence.get()];
            self.diagnostics.push(Self::diagnostic(
                expanded,
                E_UNRESOLVED_REFERENCE,
                "Squid access rule references an unresolved ACL",
            ));
            self.block_occurrence(occurrence, SemanticBlockerKind::UnresolvedAclReference);
        }

        let mut keys = Vec::new();
        for rule in &self.effective.access_rules {
            let key = (
                rule.kind,
                rule.selector
                    .as_ref()
                    .map(|selector| selector.value.clone()),
            );
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        for (kind, selector) in keys {
            let rules = self
                .effective
                .access_rules
                .iter()
                .filter(|rule| {
                    rule.kind == kind
                        && rule.selector.as_ref().map(|value| value.value.as_slice())
                            == selector.as_deref()
                })
                .cloned()
                .collect::<Vec<_>>();
            let default_action = rules
                .last()
                .map_or(AccessAction::Deny, |rule| rule.action.opposite());
            self.effective.access_policies.push(AccessPolicy {
                kind,
                selector,
                rules,
                default_action,
            });
        }
    }

    pub(super) fn resolve_direct_access(&mut self) {
        let updates = self
            .effective
            .access_policies
            .iter()
            .filter(|policy| {
                matches!(
                    policy.kind,
                    AccessListKind::AlwaysDirect | AccessListKind::NeverDirect
                ) && policy.selector.is_none()
            })
            .flat_map(|policy| {
                let supported = policy.rules.len() == 1
                    && policy.rules[0].terms.len() == 1
                    && !policy.rules[0].terms[0].negated
                    && matches!(
                        policy.rules[0].terms[0].resolution,
                        AclReferenceResolution::Builtin(BuiltinAcl::All)
                    );
                policy
                    .rules
                    .iter()
                    .map(move |rule| (rule.origin.occurrence, supported))
            })
            .collect::<Vec<_>>();
        for (occurrence, supported) in updates {
            if supported {
                self.activate_occurrence(occurrence);
            } else {
                self.block_occurrence(occurrence, SemanticBlockerKind::DirectRoutingPolicy);
            }
        }
    }
}
