#[allow(clippy::wildcard_imports)]
use super::*;

impl Resolver {
    pub(super) fn resolve_http_request_rule(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        let Some((rule, condition)) = parse_http_request_rule(&directive.arguments) else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        state
            .pending_http_request_rules
            .push(PendingHttpRequestRule {
                occurrence,
                span: directive.span,
                rule,
                condition,
            });
    }

    pub(super) fn resolve_http_response_rule(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        let Some(rule) = parse_http_response_rule(&directive.arguments) else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        state
            .settings
            .http_response_rules
            .push(EffectiveValue::direct(rule, occurrence, directive.span));
        self.consume(occurrence, Consumption::Entry);
    }

    pub(super) fn resolve_acl(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        header: &ParsedHeader,
        state: &mut SectionState,
    ) {
        if self.section_kind(occurrence) == Some(SectionKind::Defaults) && header.name.is_none() {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        }
        let Some(acl) = parse_acl(directive) else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        state
            .acls
            .push(EffectiveValue::direct(acl, occurrence, directive.span));
        self.consume(occurrence, Consumption::Entry);
    }

    pub(super) fn resolve_use_backend(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        let [backend, polarity, rest @ ..] = directive.arguments.as_slice() else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        let polarity = match polarity.value.as_slice() {
            b"if" => ConditionPolarity::If,
            b"unless" => ConditionPolarity::Unless,
            _ => {
                self.unsupported_directive_form_for_occurrence(occurrence, directive);
                return;
            }
        };
        if rest
            .iter()
            .any(|word| matches!(word.value.as_slice(), b"{" | b"}"))
        {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        }
        if rest.is_empty() {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        }
        let mut condition_negated = false;
        let mut acl_conditions = Vec::new();
        let mut index = 0;
        while index < rest.len() {
            if rest[index].value == b"!" {
                condition_negated = true;
                index += 1;
            }
            let Some(acl) = rest.get(index) else {
                self.unsupported_directive_form_for_occurrence(occurrence, directive);
                return;
            };
            acl_conditions.push(PendingAclCondition {
                name: acl.value.clone(),
                span: acl.span,
                polarity,
                negated: false,
            });
            index += 1;
        }
        state.pending_use_backends.push(PendingUseBackend {
            occurrence,
            span: directive.span,
            backend_name: backend.value.clone(),
            backend_span: backend.span,
            acl_conditions,
            polarity,
            condition_negated,
        });
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn resolve_option(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        kind: SectionKind,
        state: &mut SectionState,
    ) {
        let disabled = directive.name.value == b"no";
        let arguments = if disabled {
            let [option, rest @ ..] = directive.arguments.as_slice() else {
                self.unsupported_directive_form_for_occurrence(occurrence, directive);
                return;
            };
            if option.value != b"option" {
                self.unsupported_directive_form_for_occurrence(occurrence, directive);
                return;
            }
            rest
        } else {
            directive.arguments.as_slice()
        };
        let Some((name, arguments)) = arguments.split_first() else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };

        match name.value.as_slice() {
            b"redispatch" if supports_backend_policy(kind) => {
                let value = if disabled {
                    if !arguments.is_empty() {
                        self.unsupported_directive_form_for_occurrence(occurrence, directive);
                        return;
                    }
                    OptionState::Disabled
                } else {
                    let interval = match arguments {
                        [] => None,
                        [interval] => parse_i32(&interval.value),
                        _ => {
                            self.unsupported_directive_form_for_occurrence(occurrence, directive);
                            return;
                        }
                    };
                    if !arguments.is_empty() && interval.is_none() {
                        self.unsupported_directive_form_for_occurrence(occurrence, directive);
                        return;
                    }
                    OptionState::Enabled(Redispatch { interval })
                };
                let value = EffectiveValue::direct(value, occurrence, directive.span);
                let conflict = self.set_setting(&mut state.settings.redispatch, value);
                if !self.finish_setting(occurrence, directive, conflict, &mut state.settings) {
                    self.consume(occurrence, Consumption::Setting);
                }
            }
            b"forwardfor" => {
                let value = if disabled {
                    if !arguments.is_empty() {
                        self.unsupported_directive_form_for_occurrence(occurrence, directive);
                        return;
                    }
                    OptionState::Disabled
                } else {
                    let Some(forward_for) = parse_forward_for(arguments) else {
                        self.unsupported_directive_form_for_occurrence(occurrence, directive);
                        return;
                    };
                    OptionState::Enabled(forward_for)
                };
                let value = EffectiveValue::direct(value, occurrence, directive.span);
                let conflict = self.set_setting(&mut state.settings.forward_for, value);
                if !self.finish_setting(occurrence, directive, conflict, &mut state.settings) {
                    self.consume(occurrence, Consumption::Setting);
                }
            }
            b"httpchk" if supports_backend_policy(kind) => {
                let value = if disabled {
                    if !arguments.is_empty() {
                        self.unsupported_directive_form_for_occurrence(occurrence, directive);
                        return;
                    }
                    OptionState::Disabled
                } else {
                    let Some(check) = parse_http_check(arguments) else {
                        self.unsupported_directive_form_for_occurrence(occurrence, directive);
                        return;
                    };
                    OptionState::Enabled(check)
                };
                let value = EffectiveValue::direct(value, occurrence, directive.span);
                let conflict = self.set_setting(&mut state.settings.http_check, value);
                if !self.finish_setting(occurrence, directive, conflict, &mut state.settings) {
                    self.consume(occurrence, Consumption::Setting);
                }
            }
            b"http-server-close" if supports_backend_policy(kind) => {
                if !arguments.is_empty() {
                    self.unsupported_directive_form_for_occurrence(occurrence, directive);
                    return;
                }
                let value = EffectiveValue::direct(!disabled, occurrence, directive.span);
                let conflict = self.set_setting(&mut state.settings.http_server_close, value);
                if !self.finish_setting(occurrence, directive, conflict, &mut state.settings) {
                    self.consume(occurrence, Consumption::Setting);
                }
            }
            name if is_logging_option(name) => {
                self.externalize_log_transport(occurrence, directive);
            }
            _ => self.track_and_reject_semantics(
                occurrence,
                directive,
                SemanticBlockerKind::ProxyDefault,
                state,
                "HAProxy option changes proxy behavior that is not represented by the import IR",
            ),
        }
    }

    fn resolve_http_check_expect(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        let [expect, status, pattern] = directive.arguments.as_slice() else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        if expect.value != b"expect" || status.value != b"status" {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        }
        let Some(ranges) = parse_status_ranges(&pattern.value) else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        let value = EffectiveValue::direct(ranges, occurrence, pattern.span);
        let conflict = self.set_setting(&mut state.settings.http_check_expect, value);
        if !self.finish_setting(occurrence, directive, conflict, &mut state.settings) {
            self.consume(occurrence, Consumption::Setting);
        }
    }

    pub(super) fn resolve_http_check(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        match directive
            .arguments
            .first()
            .map(|argument| argument.value.as_slice())
        {
            Some(b"expect") => self.resolve_http_check_expect(occurrence, directive, state),
            Some(b"send") => {
                let Some(check) = parse_http_check_send(&directive.arguments[1..]) else {
                    self.unsupported_directive_form_for_occurrence(occurrence, directive);
                    return;
                };
                let value = EffectiveValue::direct(check, occurrence, directive.span);
                let conflict = self.set_setting(&mut state.settings.http_check_send, value);
                if !self.finish_setting(occurrence, directive, conflict, &mut state.settings) {
                    self.consume(occurrence, Consumption::Setting);
                }
            }
            _ => self.unsupported_directive_form_for_occurrence(occurrence, directive),
        }
    }

    pub(super) fn finish_http_request_rules(&mut self, state: &mut SectionState) {
        let mut definitions: HashMap<Vec<u8>, Vec<&EffectiveValue<AclDefinition>>> = HashMap::new();
        for acl in &state.acls {
            definitions
                .entry(acl.value.name.clone())
                .or_default()
                .push(acl);
        }

        for pending in state.pending_http_request_rules.drain(..) {
            let mut references = Vec::new();
            let condition = if let Some(condition) = pending.condition {
                let Some(acls) = definitions.get(&condition.name) else {
                    self.unresolved_reference(
                        pending.occurrence,
                        condition.span,
                        "ACL",
                        &condition.name,
                        &[],
                        "is not defined in this section",
                    );
                    continue;
                };
                let targets = acls
                    .iter()
                    .map(|acl| ReferenceTarget {
                        occurrence: acl.provenance.origin,
                        span: acl.provenance.origin_span,
                    })
                    .collect::<Vec<_>>();
                references.push(ReferenceProvenance {
                    use_span: condition.span,
                    targets: targets.clone(),
                });
                Some(HttpRequestCondition {
                    condition: AclReference {
                        name: condition.name,
                        definitions: targets.iter().map(|target| target.occurrence).collect(),
                    },
                    polarity: condition.polarity,
                    condition_negated: condition.negated,
                })
            } else {
                None
            };
            let mut rule = pending.rule;
            if let HttpRequestRule::FixedResponse {
                condition: target, ..
            } = &mut rule
            {
                *target = condition;
            }
            state
                .settings
                .http_request_rules
                .push(EffectiveValue::direct_references(
                    rule,
                    pending.occurrence,
                    pending.span,
                    references,
                ));
            self.consume(pending.occurrence, Consumption::Entry);
        }
    }

    pub(super) fn finish_use_backends(&mut self, state: &mut SectionState) {
        let mut definitions: HashMap<Vec<u8>, Vec<&EffectiveValue<AclDefinition>>> = HashMap::new();
        for acl in &state.acls {
            definitions
                .entry(acl.value.name.clone())
                .or_default()
                .push(acl);
        }

        for pending in state.pending_use_backends.drain(..) {
            let Some((target, backend_target)) = self.resolve_backend_reference(
                pending.occurrence,
                pending.backend_span,
                &pending.backend_name,
            ) else {
                continue;
            };
            let mut conditions = Vec::with_capacity(pending.acl_conditions.len());
            let mut references = vec![ReferenceProvenance {
                use_span: pending.backend_span,
                targets: vec![backend_target],
            }];
            let mut unresolved = false;
            for condition in &pending.acl_conditions {
                let Some(acls) = definitions.get(&condition.name) else {
                    self.unresolved_reference(
                        pending.occurrence,
                        condition.span,
                        "ACL",
                        &condition.name,
                        &[],
                        "is not defined in this section",
                    );
                    unresolved = true;
                    continue;
                };
                let acl_targets = acls
                    .iter()
                    .map(|acl| ReferenceTarget {
                        occurrence: acl.provenance.origin,
                        span: acl.provenance.origin_span,
                    })
                    .collect::<Vec<_>>();
                conditions.push(AclReference {
                    name: condition.name.clone(),
                    definitions: acl_targets.iter().map(|target| target.occurrence).collect(),
                });
                references.push(ReferenceProvenance {
                    use_span: condition.span,
                    targets: acl_targets,
                });
            }
            if unresolved {
                continue;
            }
            state.use_backends.push(EffectiveValue::direct_references(
                UseBackend {
                    backend: BackendReference {
                        name: pending.backend_name,
                        target,
                    },
                    conditions,
                    polarity: pending.polarity,
                    condition_negated: pending.condition_negated,
                },
                pending.occurrence,
                pending.span,
                references,
            ));
            self.consume(pending.occurrence, Consumption::Reference);
        }
    }
}
