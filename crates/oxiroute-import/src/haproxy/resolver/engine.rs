/// Resolves parsed `HAProxy` sources in their existing occurrence order.
#[must_use]
pub(super) fn resolve(configuration: &Configuration) -> Report<EffectiveConfiguration> {
    Resolver::new(configuration).run()
}
/// Resolves a parsed report without discarding source, lexing, or parsing diagnostics.
#[must_use]
pub(super) fn resolve_report(parsed: Report<Configuration>) -> Report<EffectiveConfiguration> {
    let (configuration, mut diagnostics) = parsed.into_parts();
    let (effective, resolve_diagnostics) = resolve(&configuration).into_parts();
    for diagnostic in resolve_diagnostics {
        if !diagnostics.contains(&diagnostic) {
            diagnostics.push(diagnostic);
        }
    }
    Report::new(effective, diagnostics)
}

#[derive(Clone)]
struct ParsedHeader {
    name: Option<(Vec<u8>, Span)>,
    from: Option<(Vec<u8>, Span)>,
}

#[derive(Clone)]
struct SectionMeta {
    id: SectionId,
    section: Section,
    header: Option<ParsedHeader>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum DefaultsResolutionState {
    Unvisited,
    Visiting,
    Resolved,
}

struct PendingDecision {
    occurrence: OccurrenceId,
    section: Option<SectionId>,
    keyword: Vec<u8>,
    span: Span,
    outcome: Option<DecisionOutcome>,
}

#[derive(Default)]
struct SectionState {
    settings: ProxySettings,
    binds: Vec<EffectiveBind>,
    servers: Vec<EffectiveServer>,
    server_defaults: Option<EffectiveServer>,
    acls: Vec<EffectiveValue<AclDefinition>>,
    pending_http_request_rules: Vec<PendingHttpRequestRule>,
    pending_use_backends: Vec<PendingUseBackend>,
    use_backends: Vec<EffectiveValue<UseBackend>>,
    stats: StatsSettings,
}

struct PendingHttpRequestRule {
    occurrence: OccurrenceId,
    span: Span,
    rule: HttpRequestRule,
    condition: Option<PendingAclCondition>,
}

struct PendingAclCondition {
    name: Vec<u8>,
    span: Span,
    polarity: ConditionPolarity,
    negated: bool,
}

struct PendingUseBackend {
    occurrence: OccurrenceId,
    span: Span,
    backend_name: Vec<u8>,
    backend_span: Span,
    acl_conditions: Vec<PendingAclCondition>,
    polarity: ConditionPolarity,
    condition_negated: bool,
}

struct Resolver {
    preamble: Vec<(OccurrenceId, Directive)>,
    sections: Vec<SectionMeta>,
    decisions: Vec<PendingDecision>,
    decision_indices: HashMap<OccurrenceId, usize>,
    diagnostics: Vec<Diagnostic>,
    defaults_by_name: HashMap<Vec<u8>, Vec<usize>>,
    backends_by_name: HashMap<Vec<u8>, Vec<usize>>,
    defaults_state: Vec<DefaultsResolutionState>,
    resolved_defaults: Vec<Option<EffectiveDefaults>>,
    effective: EffectiveConfiguration,
}

impl Resolver {
    fn new(configuration: &Configuration) -> Self {
        let mut preamble = Vec::new();
        let mut sections = Vec::new();
        let mut decisions = Vec::new();
        let mut decision_indices = HashMap::new();

        for file in &configuration.files {
            let source = file.source.id();
            for (directive_ordinal, directive) in file.document.preamble.iter().enumerate() {
                let occurrence = OccurrenceId::Preamble {
                    source,
                    directive_ordinal,
                };
                push_pending_decision(
                    &mut decisions,
                    &mut decision_indices,
                    occurrence,
                    None,
                    directive,
                );
                preamble.push((occurrence, directive.clone()));
            }
            for (section_ordinal, section) in file.document.sections.iter().enumerate() {
                let id = SectionId {
                    source,
                    section_ordinal,
                };
                push_pending_decision(
                    &mut decisions,
                    &mut decision_indices,
                    OccurrenceId::SectionHeader(id),
                    Some(id),
                    &section.header,
                );
                for (directive_ordinal, directive) in section.directives.iter().enumerate() {
                    push_pending_decision(
                        &mut decisions,
                        &mut decision_indices,
                        OccurrenceId::SectionDirective {
                            section: id,
                            directive_ordinal,
                        },
                        Some(id),
                        directive,
                    );
                }
                sections.push(SectionMeta {
                    id,
                    section: section.clone(),
                    header: None,
                });
            }
        }

        let section_count = sections.len();
        Self {
            preamble,
            sections,
            decisions,
            decision_indices,
            diagnostics: Vec::new(),
            defaults_by_name: HashMap::new(),
            backends_by_name: HashMap::new(),
            defaults_state: vec![DefaultsResolutionState::Unvisited; section_count],
            resolved_defaults: vec![None; section_count],
            effective: EffectiveConfiguration {
                root_decisions: configuration.root_decisions.clone(),
                ..EffectiveConfiguration::default()
            },
        }
    }

    fn run(mut self) -> Report<EffectiveConfiguration> {
        self.prepare_headers_and_identities();
        self.resolve_preamble();

        for index in 0..self.sections.len() {
            match self.sections[index].section.kind {
                SectionKind::Global => self.resolve_global(index),
                SectionKind::Defaults => {
                    self.resolve_defaults(index);
                }
                SectionKind::Frontend | SectionKind::Backend | SectionKind::Listen => {
                    self.resolve_proxy(index);
                }
                _ => self.reject_unsupported_section(index),
            }
        }

        self.effective.defaults = self
            .sections
            .iter()
            .enumerate()
            .filter(|(_, section)| section.section.kind == SectionKind::Defaults)
            .filter_map(|(index, _)| self.resolved_defaults[index].clone())
            .collect();
        self.finish_ledger();

        Report::new(self.effective, self.diagnostics)
    }

    fn prepare_headers_and_identities(&mut self) {
        for index in 0..self.sections.len() {
            let meta = self.sections[index].clone();
            let occurrence = OccurrenceId::SectionHeader(meta.id);
            if self.block_environment(occurrence, &meta.section.header) {
                continue;
            }
            if !is_supported_section(meta.section.kind) {
                continue;
            }
            match parse_header(meta.section.kind, &meta.section.header) {
                Ok(header) => self.sections[index].header = Some(header),
                Err(message) => self.unsupported_form(
                    occurrence,
                    meta.section.header.span,
                    format!(
                        "unsupported HAProxy {} section header: {message}",
                        section_name(meta.section.kind)
                    ),
                ),
            }
        }

        let mut defaults_seen: HashMap<Vec<u8>, usize> = HashMap::new();
        let mut frontend_seen: HashMap<Vec<u8>, usize> = HashMap::new();
        let mut backend_seen: HashMap<Vec<u8>, usize> = HashMap::new();

        for index in 0..self.sections.len() {
            let Some(header) = self.sections[index].header.clone() else {
                continue;
            };
            let Some((name, _)) = &header.name else {
                continue;
            };
            let kind = self.sections[index].section.kind;
            if kind == SectionKind::Defaults {
                self.defaults_by_name
                    .entry(name.clone())
                    .or_default()
                    .push(index);
                self.register_identity(&mut defaults_seen, index, name, "defaults");
            }
            if matches!(kind, SectionKind::Frontend | SectionKind::Listen) {
                self.register_identity(&mut frontend_seen, index, name, "frontend");
            }
            if matches!(kind, SectionKind::Backend | SectionKind::Listen) {
                self.backends_by_name
                    .entry(name.clone())
                    .or_default()
                    .push(index);
                self.register_identity(&mut backend_seen, index, name, "backend");
            }
        }
    }

    fn register_identity(
        &mut self,
        seen: &mut HashMap<Vec<u8>, usize>,
        index: usize,
        name: &[u8],
        namespace: &str,
    ) {
        let Some(previous) = seen.insert(name.to_vec(), index) else {
            return;
        };
        let current_id = self.sections[index].id;
        let current_span = self.sections[index].section.header.span;
        let first_span = self.sections[previous].section.header.span;
        let occurrence = OccurrenceId::SectionHeader(current_id);
        self.block(occurrence, BlockingReason::DuplicateIdentity);
        self.diagnostics.push(
            Diagnostic::new(
                E_DUPLICATE_IDENTITY,
                Severity::Error,
                DiagnosticStage::Resolve,
                format!(
                    "duplicate HAProxy {namespace} identity `{}` cannot be represented uniquely",
                    display_bytes(name)
                ),
            )
            .with_primary_span(current_span)
            .with_related_span(first_span, "first declaration is here"),
        );
    }

    fn resolve_preamble(&mut self) {
        for (occurrence, directive) in self.preamble.clone() {
            if self.block_preprocessing(occurrence, &directive) {
                continue;
            }
            self.unknown_directive(occurrence, &directive, "before any section");
        }
    }

    fn resolve_global(&mut self, index: usize) {
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

    fn resolve_defaults(&mut self, index: usize) -> Option<EffectiveDefaults> {
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

    fn resolve_proxy(&mut self, index: usize) {
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

    fn resolve_stats(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) -> bool {
        match directive.arguments.as_slice() {
            [enable] if enable.value == b"enable" => {
                let value = EffectiveValue::direct(true, occurrence, enable.span);
                let conflict = set_idempotent_value(
                    &mut state.stats.enable,
                    value,
                    &mut self.decisions,
                    &self.decision_indices,
                );
                if let Err(first_span) = conflict {
                    self.conflicting_directive(occurrence, directive, first_span);
                } else {
                    self.consume(occurrence, Consumption::Setting);
                }
                true
            }
            [uri, prefix] if uri.value == b"uri" && !prefix.value.is_empty() => {
                let value = EffectiveValue::direct(prefix.value.clone(), occurrence, prefix.span);
                let conflict = set_idempotent_value(
                    &mut state.stats.uri_prefix,
                    value,
                    &mut self.decisions,
                    &self.decision_indices,
                );
                if let Err(first_span) = conflict {
                    self.conflicting_directive(occurrence, directive, first_span);
                } else {
                    state.stats.enable.get_or_insert_with(|| {
                        EffectiveValue::direct(true, occurrence, prefix.span)
                    });
                    self.consume(occurrence, Consumption::Setting);
                }
                true
            }
            [refresh, raw] if refresh.value == b"refresh" => {
                let Some(duration) = parse_duration(&raw.value) else {
                    return false;
                };
                let value = EffectiveValue::direct(duration, occurrence, raw.span);
                let conflict = set_idempotent_value(
                    &mut state.stats.refresh,
                    value,
                    &mut self.decisions,
                    &self.decision_indices,
                );
                if let Err(first_span) = conflict {
                    self.conflicting_directive(occurrence, directive, first_span);
                } else {
                    self.consume(occurrence, Consumption::Setting);
                }
                true
            }
            [admin, condition, localhost]
                if admin.value == b"admin"
                    && condition.value == b"if"
                    && localhost.value == b"LOCALHOST" =>
            {
                let value =
                    EffectiveValue::direct(StatsAdminPolicy::Localhost, occurrence, localhost.span);
                let conflict = set_idempotent_value(
                    &mut state.stats.admin,
                    value,
                    &mut self.decisions,
                    &self.decision_indices,
                );
                if let Err(first_span) = conflict {
                    self.conflicting_directive(occurrence, directive, first_span);
                } else {
                    self.consume(occurrence, Consumption::Setting);
                }
                true
            }
            _ => false,
        }
    }

    fn resolve_mode(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        let Some(argument) = exactly_one_argument(directive) else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        let mode = match argument.value.as_slice() {
            b"http" => ProxyMode::Http,
            b"tcp" => ProxyMode::Tcp,
            _ => {
                let value = EffectiveValue::direct(
                    ProxyMode::Unsupported(argument.value.clone()),
                    occurrence,
                    argument.span,
                );
                let conflict = self.set_setting(&mut state.settings.mode, value);
                if let Some(first_span) = conflict {
                    self.conflicting_directive(occurrence, directive, first_span);
                } else {
                    self.block(occurrence, BlockingReason::SemanticBlocker);
                    self.diagnostics.push(
                        Diagnostic::new(
                            E_UNSUPPORTED_FORM,
                            Severity::Error,
                            DiagnosticStage::Resolve,
                            format!(
                                "unsupported HAProxy mode `{}` cannot inherit or lower as HTTP",
                                display_bytes(&argument.value)
                            ),
                        )
                        .with_primary_span(argument.span),
                    );
                }
                state.settings.semantic_blockers.push(semantic_blocker(
                    SemanticBlockerKind::Mode,
                    occurrence,
                    directive,
                ));
                return;
            }
        };
        let value = EffectiveValue::direct(mode, occurrence, argument.span);
        if let Some(first_span) = self.set_setting(&mut state.settings.mode, value) {
            self.conflicting_directive(occurrence, directive, first_span);
            state.settings.semantic_blockers.push(semantic_blocker(
                SemanticBlockerKind::ConflictingDirective,
                occurrence,
                directive,
            ));
        } else {
            self.consume(occurrence, Consumption::Setting);
        }
    }

    fn resolve_bind(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        match parse_bind(directive, occurrence) {
            Ok(binds) => {
                state.binds.extend(binds);
                self.consume(occurrence, Consumption::Entry);
            }
            Err(BindParseError::Malformed) => {
                self.unsupported_directive_form_for_occurrence(occurrence, directive);
            }
            Err(BindParseError::Semantic(message)) => {
                self.block_bind_semantics(occurrence, directive, state, &message);
            }
            Err(BindParseError::Conflict {
                name,
                current_span,
                previous_span,
            }) => {
                self.conflicting_option(occurrence, current_span, previous_span, &name);
                state.settings.semantic_blockers.push(semantic_blocker(
                    SemanticBlockerKind::ConflictingDirective,
                    occurrence,
                    directive,
                ));
            }
        }
    }

    fn resolve_default_backend(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        let Some(argument) = exactly_one_argument(directive) else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        let Some((target, reference_target)) =
            self.resolve_backend_reference(occurrence, argument.span, &argument.value)
        else {
            return;
        };
        let value = EffectiveValue::direct_reference(
            BackendReference {
                name: argument.value.clone(),
                target,
            },
            occurrence,
            argument.span,
            vec![reference_target],
        );
        let conflict = self.set_setting(&mut state.settings.default_backend, value);
        if !self.finish_setting(occurrence, directive, conflict, &mut state.settings) {
            self.consume(occurrence, Consumption::Reference);
        }
    }

    fn resolve_balance(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        let Some(argument) = exactly_one_argument(directive) else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        let algorithm = match argument.value.as_slice() {
            b"roundrobin" => BalanceAlgorithm::RoundRobin,
            b"leastconn" => BalanceAlgorithm::LeastConnections,
            b"first" => BalanceAlgorithm::First,
            _ => {
                self.unsupported_directive_form_for_occurrence(occurrence, directive);
                return;
            }
        };
        let value = EffectiveValue::direct(algorithm, occurrence, argument.span);
        let conflict = self.set_setting(&mut state.settings.balance, value);
        if !self.finish_setting(occurrence, directive, conflict, &mut state.settings) {
            self.consume(occurrence, Consumption::Setting);
        }
    }

    fn resolve_server(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        let Some(parsed) = parse_server(directive, occurrence) else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        let mut server = parsed.server;
        if let Some(defaults) = &state.server_defaults {
            if server.check.is_none() {
                server.check.clone_from(&defaults.check);
            }
            if server.interval.is_none() {
                server.interval.clone_from(&defaults.interval);
            }
            if server.fast_interval.is_none() {
                server.fast_interval.clone_from(&defaults.fast_interval);
            }
            if server.down_interval.is_none() {
                server.down_interval.clone_from(&defaults.down_interval);
            }
            if server.rise.is_none() {
                server.rise.clone_from(&defaults.rise);
            }
            if server.fall.is_none() {
                server.fall.clone_from(&defaults.fall);
            }
            if server.max_connections.is_none() {
                server.max_connections.clone_from(&defaults.max_connections);
            }
            if server.observe.is_none() {
                server.observe.clone_from(&defaults.observe);
            }
            if server.error_limit.is_none() {
                server.error_limit.clone_from(&defaults.error_limit);
            }
            if server.on_error.is_none() {
                server.on_error.clone_from(&defaults.on_error);
            }
            if server.weight.is_none() {
                server.weight.clone_from(&defaults.weight);
            }
            server
                .unsupported_options
                .extend(defaults.unsupported_options.iter().cloned());
        }
        for conflict in parsed.conflicts {
            self.conflicting_option(
                occurrence,
                conflict.current_span,
                conflict.previous_span,
                &conflict.name,
            );
        }
        if !server.unsupported_options.is_empty() {
            self.block(occurrence, BlockingReason::SemanticBlocker);
            self.diagnostics.push(
                Diagnostic::new(
                    E_UNSUPPORTED_FORM,
                    Severity::Error,
                    DiagnosticStage::Resolve,
                    format!(
                        "HAProxy server options `{}` affect selection, capacity, TLS, or health-check behavior that is not represented canonically",
                        server
                            .unsupported_options
                            .iter()
                            .map(|option| display_bytes(&option.value.name))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )
                .with_primary_span(directive.span),
            );
        }
        if let Some(previous) = state
            .servers
            .iter()
            .find(|candidate| candidate.name.value == server.name.value)
        {
            self.block(occurrence, BlockingReason::DuplicateIdentity);
            self.diagnostics.push(
                Diagnostic::new(
                    E_DUPLICATE_IDENTITY,
                    Severity::Error,
                    DiagnosticStage::Resolve,
                    format!(
                        "duplicate HAProxy server identity `{}` cannot be represented uniquely",
                        display_bytes(&server.name.value)
                    ),
                )
                .with_primary_span(server.name.provenance.origin_span)
                .with_related_span(
                    previous.name.provenance.origin_span,
                    "first declaration is here",
                ),
            );
        } else if self.pending_decision_mut(occurrence).outcome.is_none() {
            self.consume(occurrence, Consumption::Entry);
        }
        state.servers.push(server);
    }

    fn resolve_default_server(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        let synthetic_word = |value: &[u8]| super::Word {
            value: value.to_vec(),
            span: directive.span,
            environment_references: Vec::new(),
        };
        let mut synthetic = directive.clone();
        synthetic.arguments = vec![
            synthetic_word("__defaults".as_bytes()),
            synthetic_word("127.0.0.1:1".as_bytes()),
        ];
        synthetic.arguments.extend(directive.arguments.clone());
        let Some(parsed) = parse_server(&synthetic, occurrence) else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        for conflict in parsed.conflicts {
            self.conflicting_option(
                occurrence,
                conflict.current_span,
                conflict.previous_span,
                &conflict.name,
            );
        }
        if parsed.server.unsupported_options.is_empty() {
            match &mut state.server_defaults {
                Some(defaults) => merge_server_defaults(defaults, parsed.server),
                None => state.server_defaults = Some(parsed.server),
            }
            self.consume(occurrence, Consumption::Setting);
        } else {
            self.track_and_reject_semantics(
                occurrence,
                directive,
                SemanticBlockerKind::ProxyDefault,
                state,
                "HAProxy default-server contains options without canonical server equivalents",
            );
        }
    }

    fn resolve_retries(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        let Some(value) = parse_one_u32(directive) else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        let value = EffectiveValue::direct(value, occurrence, directive.arguments[0].span);
        let conflict = self.set_setting(&mut state.settings.retries, value);
        if !self.finish_setting(occurrence, directive, conflict, &mut state.settings) {
            self.consume(occurrence, Consumption::Setting);
        }
    }

    fn resolve_retry_on(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        let retry_on = match parse_retry_on(&directive.arguments) {
            Ok(retry_on) => retry_on,
            Err(reason) => {
                let message = format!("unsupported HAProxy retry-on form: {reason}");
                self.track_and_reject_semantics(
                    occurrence,
                    directive,
                    SemanticBlockerKind::Retry,
                    state,
                    &message,
                );
                return;
            }
        };
        let value = EffectiveValue::direct(retry_on, occurrence, directive.span);
        let conflict = self.set_setting(&mut state.settings.retry_on, value);
        if !self.finish_setting(occurrence, directive, conflict, &mut state.settings) {
            self.consume(occurrence, Consumption::Setting);
        }
    }

    fn resolve_timeout(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        let [class, raw] = directive.arguments.as_slice() else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        let Some(duration) = parse_duration(&raw.value) else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        let value = EffectiveValue::direct(duration, occurrence, raw.span);
        let slot = match class.value.as_slice() {
            b"client" => &mut state.settings.timeouts.client,
            b"connect" => &mut state.settings.timeouts.connect,
            b"queue" => &mut state.settings.timeouts.queue,
            b"server" => &mut state.settings.timeouts.server,
            b"http-request" => &mut state.settings.timeouts.http_request,
            b"http-keep-alive" => &mut state.settings.timeouts.http_keep_alive,
            _ => {
                self.track_and_reject_semantics(
                    occurrence,
                    directive,
                    SemanticBlockerKind::Timeout,
                    state,
                    "HAProxy timeout class has no canonical equivalent",
                );
                return;
            }
        };
        let conflict = set_value(slot, value, &mut self.decisions, &self.decision_indices);
        if !self.finish_setting(occurrence, directive, conflict, &mut state.settings) {
            self.consume(occurrence, Consumption::Setting);
        }
    }

    fn resolve_maxconn(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        let Some(value) = parse_one_u64(directive) else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        let value = EffectiveValue::direct(value, occurrence, directive.arguments[0].span);
        let conflict = self.set_setting(&mut state.settings.maxconn, value);
        if !self.finish_setting(occurrence, directive, conflict, &mut state.settings) {
            self.consume(occurrence, Consumption::Setting);
        }
    }

    fn resolve_http_request_rule(
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

    fn resolve_http_response_rule(
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

    fn resolve_acl(
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

    fn resolve_use_backend(
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
    fn resolve_option(
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

    fn resolve_http_check(
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

    fn finish_http_request_rules(&mut self, state: &mut SectionState) {
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

    fn finish_use_backends(&mut self, state: &mut SectionState) {
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

    fn resolve_defaults_reference(
        &mut self,
        occurrence: OccurrenceId,
        span: Span,
        name: &[u8],
    ) -> Option<usize> {
        let candidates = self.defaults_by_name.get(name).cloned().unwrap_or_default();
        if candidates.len() == 1 {
            return candidates.first().copied();
        }
        let related = candidates
            .iter()
            .map(|index| self.sections[*index].section.header.span)
            .collect::<Vec<_>>();
        let reason = if candidates.is_empty() {
            "is not declared"
        } else {
            "is ambiguous"
        };
        self.unresolved_reference(occurrence, span, "defaults", name, &related, reason);
        None
    }

    fn resolve_backend_reference(
        &mut self,
        occurrence: OccurrenceId,
        span: Span,
        name: &[u8],
    ) -> Option<(SectionId, ReferenceTarget)> {
        let candidates = self.backends_by_name.get(name).cloned().unwrap_or_default();
        if let [index] = candidates.as_slice() {
            let target = &self.sections[*index];
            return Some((
                target.id,
                ReferenceTarget {
                    occurrence: OccurrenceId::SectionHeader(target.id),
                    span: target.section.header.span,
                },
            ));
        }
        let related = candidates
            .iter()
            .map(|index| self.sections[*index].section.header.span)
            .collect::<Vec<_>>();
        let reason = if candidates.is_empty() {
            "is not declared"
        } else {
            "is ambiguous"
        };
        self.unresolved_reference(occurrence, span, "backend", name, &related, reason);
        None
    }

    fn unresolved_reference(
        &mut self,
        occurrence: OccurrenceId,
        span: Span,
        reference_kind: &str,
        name: &[u8],
        related: &[Span],
        reason: &str,
    ) {
        self.block(occurrence, BlockingReason::UnresolvedReference);
        let mut diagnostic = Diagnostic::new(
            E_UNRESOLVED_REFERENCE,
            Severity::Error,
            DiagnosticStage::Resolve,
            format!(
                "HAProxy {reference_kind} reference `{}` {reason}",
                display_bytes(name)
            ),
        )
        .with_primary_span(span);
        for target in related {
            diagnostic = diagnostic.with_related_span(*target, "candidate declaration is here");
        }
        self.diagnostics.push(diagnostic);
    }

    fn reject_unsupported_section(&mut self, index: usize) {
        let meta = self.sections[index].clone();
        let occurrence = OccurrenceId::SectionHeader(meta.id);
        self.block(occurrence, BlockingReason::UnsupportedSection);
        self.diagnostics.push(
            Diagnostic::new(
                E_UNSUPPORTED_SECTION,
                Severity::Error,
                DiagnosticStage::Resolve,
                format!(
                    "HAProxy {} sections are not represented by the import IR",
                    section_name(meta.section.kind)
                ),
            )
            .with_primary_span(meta.section.header.span),
        );
        self.block_section_directives(index, BlockingReason::UnsupportedSection);
    }

    fn block_section_directives(&mut self, index: usize, reason: BlockingReason) {
        let id = self.sections[index].id;
        let count = self.sections[index].section.directives.len();
        for directive_ordinal in 0..count {
            self.block(section_directive_id(id, directive_ordinal), reason);
        }
    }

    fn block_preprocessing(&mut self, occurrence: OccurrenceId, directive: &Directive) -> bool {
        if is_conditional(directive) {
            self.block(occurrence, BlockingReason::ConditionalPreprocessing);
            self.diagnostics.push(
                Diagnostic::new(
                    E_CONDITIONAL_PREPROCESSING,
                    Severity::Error,
                    DiagnosticStage::Resolve,
                    "HAProxy conditional requires explicit preprocessing before activation",
                )
                .with_primary_span(directive.name.span),
            );
            return true;
        }
        self.block_environment(occurrence, directive)
    }

    fn block_environment(&mut self, occurrence: OccurrenceId, directive: &Directive) -> bool {
        let references = directive
            .arguments
            .iter()
            .chain(std::iter::once(&directive.name))
            .flat_map(|word| word.environment_references.iter().copied())
            .collect::<Vec<_>>();
        if references.is_empty() {
            return false;
        }
        self.block(occurrence, BlockingReason::EnvironmentPreprocessing);
        for reference in references {
            self.diagnostics.push(
                Diagnostic::new(
                    E_ENVIRONMENT_EXPANSION,
                    Severity::Error,
                    DiagnosticStage::Resolve,
                    "HAProxy environment reference requires explicit preprocessing before activation",
                )
                .with_primary_span(reference),
            );
        }
        true
    }

    fn reject_semantic_directive(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        message: &str,
    ) {
        self.block(occurrence, BlockingReason::SemanticBlocker);
        self.diagnostics.push(
            Diagnostic::new(
                E_UNSUPPORTED_FORM,
                Severity::Error,
                DiagnosticStage::Resolve,
                message,
            )
            .with_primary_span(directive.span),
        );
    }

    fn track_and_reject_semantics(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        kind: SemanticBlockerKind,
        state: &mut SectionState,
        message: &str,
    ) {
        state
            .settings
            .semantic_blockers
            .push(semantic_blocker(kind, occurrence, directive));
        self.reject_semantic_directive(occurrence, directive, message);
    }

    fn block_bind_semantics(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
        message: &str,
    ) {
        self.track_and_reject_semantics(
            occurrence,
            directive,
            SemanticBlockerKind::Tls,
            state,
            message,
        );
    }

    fn conflicting_directive(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        first_span: Span,
    ) {
        self.block(occurrence, BlockingReason::ConflictingDirective);
        self.diagnostics.push(
            Diagnostic::new(
                E_CONFLICTING_DIRECTIVE,
                Severity::Error,
                DiagnosticStage::Resolve,
                format!(
                    "conflicting HAProxy `{}` directives cannot select one effective value",
                    display_bytes(&directive.name.value)
                ),
            )
            .with_primary_span(directive.span)
            .with_related_span(first_span, "first direct value is here"),
        );
    }

    fn conflicting_option(
        &mut self,
        occurrence: OccurrenceId,
        current_span: Span,
        previous_span: Span,
        name: &[u8],
    ) {
        self.block(occurrence, BlockingReason::ConflictingDirective);
        self.diagnostics.push(
            Diagnostic::new(
                E_CONFLICTING_DIRECTIVE,
                Severity::Error,
                DiagnosticStage::Resolve,
                format!(
                    "conflicting HAProxy `{}` options cannot select one effective value",
                    display_bytes(name)
                ),
            )
            .with_primary_span(current_span)
            .with_related_span(previous_span, "first option is here"),
        );
    }

    fn finish_setting(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        conflict: Option<Span>,
        settings: &mut ProxySettings,
    ) -> bool {
        let Some(first_span) = conflict else {
            return false;
        };
        self.conflicting_directive(occurrence, directive, first_span);
        settings.semantic_blockers.push(semantic_blocker(
            SemanticBlockerKind::ConflictingDirective,
            occurrence,
            directive,
        ));
        true
    }

    fn externalize_process_setting(&mut self, occurrence: OccurrenceId, directive: &Directive) {
        let decision = self.pending_decision_mut(occurrence);
        if decision.outcome.is_none() {
            decision.outcome = Some(DecisionOutcome::Externalized(Externalization::ProcessOwned));
        }
        self.diagnostics.push(
            Diagnostic::new(
                E_PROCESS_OWNED,
                Severity::Warning,
                DiagnosticStage::Resolve,
                "HAProxy process-owned behavior is externalized to the deployment",
            )
            .with_primary_span(directive.span),
        );
        self.effective
            .deployment_requirements
            .push(DeploymentRequirement {
                kind: process_requirement_kind(&directive.name.value),
                directive: display_bytes(&directive.name.value),
                value: directive
                    .arguments
                    .iter()
                    .map(|argument| display_bytes(&argument.value))
                    .collect(),
                origin: ProvenanceSpan {
                    role: ProvenanceRole::Value,
                    span: directive.span,
                },
            });
    }

    fn externalize_log_transport(&mut self, occurrence: OccurrenceId, directive: &Directive) {
        let decision = self.pending_decision_mut(occurrence);
        if decision.outcome.is_none() {
            decision.outcome = Some(DecisionOutcome::Externalized(Externalization::LogTransport));
        }
        self.diagnostics.push(
            Diagnostic::new(
                E_LOGGING_UNSUPPORTED,
                Severity::Warning,
                DiagnosticStage::Resolve,
                "HAProxy log transport is externalized to the deployment; no format equivalence is claimed",
            )
            .with_primary_span(directive.span),
        );
        self.effective
            .deployment_requirements
            .push(DeploymentRequirement {
                kind: DeploymentRequirementKind::LogTransport,
                directive: display_bytes(&directive.name.value),
                value: directive
                    .arguments
                    .iter()
                    .map(|argument| display_bytes(&argument.value))
                    .collect(),
                origin: ProvenanceSpan {
                    role: ProvenanceRole::Value,
                    span: directive.span,
                },
            });
    }

    fn externalize_activation(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        kind: ActivationRequirementKind,
        section: Option<SectionId>,
        supported: bool,
    ) {
        let decision = self.pending_decision_mut(occurrence);
        if decision.outcome.is_none() {
            decision.outcome = Some(DecisionOutcome::Externalized(Externalization::Activation));
        }
        self.diagnostics.push(
            Diagnostic::new(
                E_STATS_UNSUPPORTED,
                Severity::Warning,
                DiagnosticStage::Resolve,
                "HAProxy statistics endpoint requires explicit activation; no runtime equivalence is claimed",
            )
            .with_primary_span(directive.span),
        );
        self.effective
            .activation_requirements
            .push(ActivationRequirement {
                kind,
                directive: display_directive(directive),
                origin: ProvenanceSpan {
                    role: ProvenanceRole::Value,
                    span: directive.span,
                },
                equivalent_runtime_endpoint: false,
            });
        if let Some(section) = section {
            self.effective.activation_only_sections.insert(section);
            if supported {
                self.effective.supported_stats_sections.insert(section);
            }
        }
    }

    fn unknown_directive(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        location: &str,
    ) {
        self.block(occurrence, BlockingReason::UnknownDirective);
        self.diagnostics.push(
            Diagnostic::new(
                E_UNKNOWN_DIRECTIVE,
                Severity::Error,
                DiagnosticStage::Resolve,
                format!(
                    "unknown HAProxy directive `{}` {location}",
                    display_bytes(&directive.name.value)
                ),
            )
            .with_primary_span(directive.name.span),
        );
    }

    fn unsupported_directive_form(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        kind: SectionKind,
    ) {
        self.unsupported_form(
            occurrence,
            directive.span,
            format!(
                "unsupported HAProxy `{}` form in a {} section",
                display_bytes(&directive.name.value),
                section_name(kind)
            ),
        );
    }

    fn unsupported_directive_form_for_occurrence(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
    ) {
        let kind = self
            .section_kind(occurrence)
            .expect("directive occurrence belongs to a section");
        self.unsupported_directive_form(occurrence, directive, kind);
    }

    fn unsupported_form(&mut self, occurrence: OccurrenceId, span: Span, message: String) {
        self.block(occurrence, BlockingReason::UnsupportedForm);
        self.diagnostics.push(
            Diagnostic::new(
                E_UNSUPPORTED_FORM,
                Severity::Error,
                DiagnosticStage::Resolve,
                message,
            )
            .with_primary_span(span),
        );
    }

    fn set_setting<T: PartialEq>(
        &mut self,
        slot: &mut Option<EffectiveValue<T>>,
        value: EffectiveValue<T>,
    ) -> Option<Span> {
        set_value(slot, value, &mut self.decisions, &self.decision_indices)
    }

    fn consume(&mut self, occurrence: OccurrenceId, consumption: Consumption) {
        let decision = self.pending_decision_mut(occurrence);
        if decision.outcome.is_none() {
            decision.outcome = Some(DecisionOutcome::Consumed(consumption));
        }
    }

    fn block(&mut self, occurrence: OccurrenceId, reason: BlockingReason) {
        let decision = self.pending_decision_mut(occurrence);
        if decision.outcome.is_none() {
            decision.outcome = Some(DecisionOutcome::Blocked(reason));
        }
    }

    fn pending_decision_mut(&mut self, occurrence: OccurrenceId) -> &mut PendingDecision {
        let index = self.decision_indices[&occurrence];
        &mut self.decisions[index]
    }

    fn section(&self, id: SectionId) -> &SectionMeta {
        self.sections
            .iter()
            .find(|section| section.id == id)
            .expect("section occurrence was indexed")
    }

    fn section_kind(&self, occurrence: OccurrenceId) -> Option<SectionKind> {
        match occurrence {
            OccurrenceId::Preamble { .. } => None,
            OccurrenceId::SectionHeader(id)
            | OccurrenceId::SectionDirective { section: id, .. } => {
                Some(self.section(id).section.kind)
            }
        }
    }

    fn finish_ledger(&mut self) {
        for decision in &mut self.decisions {
            if decision.outcome.is_some() {
                continue;
            }
            decision.outcome = Some(DecisionOutcome::Blocked(
                BlockingReason::UnconsumedDirective,
            ));
            self.diagnostics.push(
                Diagnostic::new(
                    E_UNCONSUMED_DIRECTIVE,
                    Severity::Error,
                    DiagnosticStage::Resolve,
                    format!(
                        "HAProxy occurrence `{}` was not consumed by semantic resolution",
                        display_bytes(&decision.keyword)
                    ),
                )
                .with_primary_span(decision.span),
            );
        }
        self.effective.ledger = DecisionLedger {
            entries: self
                .decisions
                .drain(..)
                .map(|decision| Decision {
                    occurrence: decision.occurrence,
                    section: decision.section,
                    keyword: decision.keyword,
                    span: decision.span,
                    outcome: decision.outcome.expect("terminal decision was assigned"),
                })
                .collect(),
        };
    }
}

fn push_pending_decision(
    decisions: &mut Vec<PendingDecision>,
    indices: &mut HashMap<OccurrenceId, usize>,
    occurrence: OccurrenceId,
    section: Option<SectionId>,
    directive: &Directive,
) {
    let index = decisions.len();
    decisions.push(PendingDecision {
        occurrence,
        section,
        keyword: directive.name.value.clone(),
        span: directive.span,
        outcome: None,
    });
    indices.insert(occurrence, index);
}

fn semantic_blocker(
    kind: SemanticBlockerKind,
    occurrence: OccurrenceId,
    directive: &Directive,
) -> EffectiveValue<SemanticBlocker> {
    EffectiveValue::direct(
        SemanticBlocker {
            kind,
            keyword: directive.name.value.clone(),
            arguments: directive
                .arguments
                .iter()
                .map(|argument| argument.value.clone())
                .collect(),
        },
        occurrence,
        directive.span,
    )
}

fn set_value<T: PartialEq>(
    slot: &mut Option<EffectiveValue<T>>,
    value: EffectiveValue<T>,
    decisions: &mut [PendingDecision],
    indices: &HashMap<OccurrenceId, usize>,
) -> Option<Span> {
    let conflict = slot
        .as_ref()
        .filter(|previous| previous.provenance.is_direct() && previous.value != value.value)
        .map(|previous| previous.provenance.origin_span);
    if let Some(previous) = slot
        .as_ref()
        .filter(|previous| previous.provenance.is_direct())
    {
        let index = indices[&previous.provenance.origin];
        if matches!(decisions[index].outcome, Some(DecisionOutcome::Consumed(_))) {
            decisions[index].outcome = Some(DecisionOutcome::Superseded {
                by: value.provenance.origin,
            });
        }
    }
    *slot = Some(value);
    conflict
}

fn set_idempotent_value<T: PartialEq>(
    slot: &mut Option<EffectiveValue<T>>,
    value: EffectiveValue<T>,
    decisions: &mut [PendingDecision],
    indices: &HashMap<OccurrenceId, usize>,
) -> Result<(), Span> {
    if slot
        .as_ref()
        .is_some_and(|current| current.provenance.is_direct() && current.value == value.value)
    {
        return Ok(());
    }
    set_value(slot, value, decisions, indices).map_or(Ok(()), Err)
}
