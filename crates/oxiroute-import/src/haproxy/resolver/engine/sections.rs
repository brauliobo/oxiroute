#[allow(clippy::wildcard_imports)]
use super::*;

impl Resolver {
    pub(super) fn resolve_global(&mut self, index: usize) {
        let meta = self.sections[index].clone();
        let Some(header) = meta.header.clone() else {
            self.block_section_directives(index, BlockingReason::UnsupportedForm);
            return;
        };
        debug_assert!(header.name.is_none());
        self.consume(OccurrenceId::SectionHeader(meta.id), Consumption::Section);
        self.effective
            .global
            .sections
            .push(effective_section(&meta, &header));

        for (directive_ordinal, directive) in meta.section.directives.iter().enumerate() {
            let occurrence = section_directive_id(meta.id, directive_ordinal);
            if self.block_preprocessing(occurrence, directive) {
                continue;
            }
            match directive.name.value.as_slice() {
                b"maxconn" => {
                    let Some(value) = parse_one_u64(directive) else {
                        self.unsupported_directive_form(occurrence, directive, SectionKind::Global);
                        continue;
                    };
                    let value =
                        EffectiveValue::direct(value, occurrence, directive.arguments[0].span);
                    let conflict = set_value(
                        &mut self.effective.global.maxconn,
                        value,
                        &mut self.decisions,
                        &self.decision_indices,
                    );
                    if let Some(first_span) = conflict {
                        self.conflicting_directive(occurrence, directive, first_span);
                        self.effective
                            .global
                            .semantic_blockers
                            .push(semantic_blocker(
                                SemanticBlockerKind::ConflictingDirective,
                                occurrence,
                                directive,
                            ));
                    } else {
                        self.consume(occurrence, Consumption::Setting);
                    }
                }
                b"stats" => self.externalize_activation(
                    occurrence,
                    directive,
                    ActivationRequirementKind::StatisticsEndpoint,
                    None,
                    false,
                ),
                name if is_logging_directive_name(name) => {
                    self.externalize_log_transport(occurrence, directive);
                }
                name if is_global_security_directive(name) => {
                    self.effective
                        .global
                        .semantic_blockers
                        .push(semantic_blocker(
                            SemanticBlockerKind::GlobalSecurity,
                            occurrence,
                            directive,
                        ));
                    self.reject_semantic_directive(
                        occurrence,
                        directive,
                        "HAProxy global TLS or security policy is not represented by the canonical configuration",
                    );
                }
                name if is_process_owned(name) => {
                    self.externalize_process_setting(occurrence, directive);
                }
                _ => self.unknown_directive(occurrence, directive, "in a global section"),
            }
        }
    }

    pub(super) fn resolve_defaults(&mut self, index: usize) -> Option<EffectiveDefaults> {
        match self
            .defaults_state
            .get(index)
            .copied()
            .expect("section was indexed")
        {
            DefaultsResolutionState::Resolved => return self.resolved_defaults[index].clone(),
            DefaultsResolutionState::Visiting => return None,
            DefaultsResolutionState::Unvisited => {}
        }
        self.defaults_state[index] = DefaultsResolutionState::Visiting;

        let meta = self.sections[index].clone();
        let Some(header) = meta.header.clone() else {
            self.block_section_directives(index, BlockingReason::UnsupportedForm);
            self.defaults_state[index] = DefaultsResolutionState::Resolved;
            return None;
        };

        let (settings, server_defaults, defaults) = self.explicit_defaults_base(&meta, &header);
        let mut state = SectionState {
            settings,
            server_defaults,
            ..SectionState::default()
        };
        self.resolve_section_directives(index, &header, &mut state);
        self.finish_http_request_rules(&mut state);
        self.finish_use_backends(&mut state);

        let resolved = EffectiveDefaults {
            section: effective_section(&meta, &header),
            defaults,
            settings: state.settings,
            server_defaults: state.server_defaults,
            acls: state.acls,
        };
        self.consume(
            OccurrenceId::SectionHeader(meta.id),
            if resolved.defaults.is_some() {
                Consumption::Inheritance
            } else {
                Consumption::Section
            },
        );
        self.defaults_state[index] = DefaultsResolutionState::Resolved;
        self.resolved_defaults[index] = Some(resolved.clone());
        Some(resolved)
    }

    fn explicit_defaults_base(
        &mut self,
        meta: &SectionMeta,
        header: &ParsedHeader,
    ) -> (
        ProxySettings,
        Option<EffectiveServer>,
        Option<DefaultsSource>,
    ) {
        let Some((name, reference_span)) = &header.from else {
            return (ProxySettings::default(), None, None);
        };
        let Some(target_index) = self.resolve_defaults_reference(
            OccurrenceId::SectionHeader(meta.id),
            *reference_span,
            name,
        ) else {
            return (ProxySettings::default(), None, None);
        };
        if self.defaults_state[target_index] == DefaultsResolutionState::Visiting {
            self.unresolved_reference(
                OccurrenceId::SectionHeader(meta.id),
                *reference_span,
                "defaults",
                name,
                &[],
                "forms an inheritance cycle",
            );
            return (ProxySettings::default(), None, None);
        }
        let Some(target) = self.resolve_defaults(target_index) else {
            self.unresolved_reference(
                OccurrenceId::SectionHeader(meta.id),
                *reference_span,
                "defaults",
                name,
                &[],
                "does not resolve to a representable defaults section",
            );
            return (ProxySettings::default(), None, None);
        };
        let step = InheritanceStep {
            source_defaults: target.section.id,
            destination: meta.id,
            selection: DefaultsSelection::Explicit,
            reference_span: Some(*reference_span),
        };
        let source = defaults_source(
            meta,
            target.section.id,
            target.section.declaration,
            target.section.span,
            DefaultsSelection::Explicit,
            *reference_span,
        );
        (
            target.settings.inherited(&step),
            inherit_server_defaults(target.server_defaults, &step),
            Some(source),
        )
    }

    pub(super) fn resolve_proxy(&mut self, index: usize) {
        let meta = self.sections[index].clone();
        let Some(header) = meta.header.clone() else {
            self.block_section_directives(index, BlockingReason::UnsupportedForm);
            return;
        };
        let (settings, server_defaults, defaults) = self.proxy_defaults_base(index, &meta, &header);
        let mut state = SectionState {
            settings,
            server_defaults,
            ..SectionState::default()
        };
        self.resolve_section_directives(index, &header, &mut state);
        self.finish_http_request_rules(&mut state);
        self.finish_use_backends(&mut state);
        self.consume(
            OccurrenceId::SectionHeader(meta.id),
            if defaults.is_some() {
                Consumption::Inheritance
            } else {
                Consumption::Section
            },
        );

        let section = effective_section(&meta, &header);
        match meta.section.kind {
            SectionKind::Frontend => self.effective.frontends.push(EffectiveFrontend {
                section,
                defaults,
                settings: state.settings,
                binds: state.binds,
                acls: state.acls,
                use_backends: state.use_backends,
                stats: state.stats,
            }),
            SectionKind::Backend => self.effective.backends.push(EffectiveBackend {
                section,
                defaults,
                settings: state.settings,
                servers: state.servers,
                acls: state.acls,
            }),
            SectionKind::Listen => self.effective.listens.push(EffectiveListen {
                section,
                defaults,
                settings: state.settings,
                binds: state.binds,
                servers: state.servers,
                acls: state.acls,
                use_backends: state.use_backends,
                stats: state.stats,
            }),
            _ => unreachable!("caller selected a proxy section"),
        }
    }

    fn proxy_defaults_base(
        &mut self,
        index: usize,
        meta: &SectionMeta,
        header: &ParsedHeader,
    ) -> (
        ProxySettings,
        Option<EffectiveServer>,
        Option<DefaultsSource>,
    ) {
        let (target_index, selection, reference_span) = if let Some((name, span)) = &header.from {
            let Some(target) =
                self.resolve_defaults_reference(OccurrenceId::SectionHeader(meta.id), *span, name)
            else {
                return (ProxySettings::default(), None, None);
            };
            (target, DefaultsSelection::Explicit, *span)
        } else {
            let Some(target) = self.sections[..index]
                .iter()
                .rposition(|section| section.section.kind == SectionKind::Defaults)
            else {
                return (ProxySettings::default(), None, None);
            };
            (
                target,
                DefaultsSelection::ImplicitLatest,
                meta.section.header.span,
            )
        };
        let Some(target) = self.resolve_defaults(target_index) else {
            self.unresolved_reference(
                OccurrenceId::SectionHeader(meta.id),
                reference_span,
                "defaults",
                header
                    .from
                    .as_ref()
                    .map_or(b"<latest>".as_slice(), |(name, _)| name.as_slice()),
                &[],
                "does not resolve to a representable defaults section",
            );
            return (ProxySettings::default(), None, None);
        };
        let step = InheritanceStep {
            source_defaults: target.section.id,
            destination: meta.id,
            selection,
            reference_span: (selection == DefaultsSelection::Explicit).then_some(reference_span),
        };
        let source = defaults_source(
            meta,
            target.section.id,
            target.section.declaration,
            target.section.span,
            selection,
            reference_span,
        );
        (
            target.settings.inherited(&step),
            inherit_server_defaults(target.server_defaults, &step),
            Some(source),
        )
    }

    #[allow(clippy::too_many_lines)]
    fn resolve_section_directives(
        &mut self,
        index: usize,
        header: &ParsedHeader,
        state: &mut SectionState,
    ) {
        let meta = self.sections[index].clone();
        for (directive_ordinal, directive) in meta.section.directives.iter().enumerate() {
            let occurrence = section_directive_id(meta.id, directive_ordinal);
            if self.block_preprocessing(occurrence, directive) {
                continue;
            }
            if directive.name.value == b"http-request"
                && directive
                    .arguments
                    .first()
                    .is_some_and(|argument| argument.value == b"use-service")
                && directive
                    .arguments
                    .get(1)
                    .is_some_and(|argument| argument.value == b"prometheus-exporter")
            {
                let supported = exact_prometheus_exporter(directive);
                self.externalize_activation(
                    occurrence,
                    directive,
                    ActivationRequirementKind::PrometheusExporter,
                    Some(meta.id),
                    supported,
                );
                continue;
            }
            if directive.name.value == b"stats" {
                if supports_stats_page(meta.section.kind)
                    && self.resolve_stats(occurrence, directive, state)
                {
                    self.effective.activation_only_sections.insert(meta.id);
                } else {
                    self.effective.blocked_stats_page_sections.insert(meta.id);
                    self.externalize_activation(
                        occurrence,
                        directive,
                        ActivationRequirementKind::StatisticsEndpoint,
                        Some(meta.id),
                        false,
                    );
                }
                continue;
            }
            if is_logging_directive(directive) {
                self.externalize_log_transport(occurrence, directive);
                continue;
            }
            if is_process_owned(&directive.name.value) {
                self.externalize_process_setting(occurrence, directive);
                continue;
            }

            match directive.name.value.as_slice() {
                b"mode" => self.resolve_mode(occurrence, directive, state),
                b"bind" if supports_bind(meta.section.kind) => {
                    self.resolve_bind(occurrence, directive, state);
                }
                b"default_backend" if supports_default_backend(meta.section.kind) => {
                    self.resolve_default_backend(occurrence, directive, state);
                }
                b"balance" if supports_balance(meta.section.kind) => {
                    self.resolve_balance(occurrence, directive, state);
                }
                b"server" if supports_server(meta.section.kind) => {
                    self.resolve_server(occurrence, directive, state);
                }
                b"default-server" if supports_backend_policy(meta.section.kind) => {
                    self.resolve_default_server(occurrence, directive, state);
                }
                b"retries" if supports_backend_policy(meta.section.kind) => {
                    self.resolve_retries(occurrence, directive, state);
                }
                b"timeout" => self.resolve_timeout(occurrence, directive, state),
                b"maxconn" if supports_maxconn(meta.section.kind) => {
                    self.resolve_maxconn(occurrence, directive, state);
                }
                b"acl" => self.resolve_acl(occurrence, directive, header, state),
                b"use_backend" if supports_use_backend(meta.section.kind) => {
                    self.resolve_use_backend(occurrence, directive, state);
                }
                b"http-request" if supports_http_rules(meta.section.kind) => {
                    self.resolve_http_request_rule(occurrence, directive, state);
                }
                b"http-response" if supports_http_rules(meta.section.kind) => {
                    self.resolve_http_response_rule(occurrence, directive, state);
                }
                b"option" | b"no" => {
                    self.resolve_option(occurrence, directive, meta.section.kind, state);
                }
                b"http-check" if supports_backend_policy(meta.section.kind) => {
                    self.resolve_http_check(occurrence, directive, state);
                }
                b"retry-on" if supports_backend_policy(meta.section.kind) => {
                    self.resolve_retry_on(occurrence, directive, state);
                }
                name if is_proxy_default_directive(name) => {
                    self.track_and_reject_semantics(
                        occurrence,
                        directive,
                        SemanticBlockerKind::ProxyDefault,
                        state,
                        "HAProxy proxy defaults or dispatch policy are not represented by the import IR",
                    );
                }
                name if is_known_resolver_directive(name) => {
                    self.unsupported_directive_form(occurrence, directive, meta.section.kind);
                }
                _ => self.unknown_directive(
                    occurrence,
                    directive,
                    &format!("in a {} section", section_name(meta.section.kind)),
                ),
            }
        }
    }
}
