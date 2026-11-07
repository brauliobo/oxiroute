#[allow(clippy::wildcard_imports)]
use super::*;

impl Resolver {
    pub(super) fn resolve_stats(
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

    pub(super) fn resolve_mode(
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

    pub(super) fn resolve_bind(
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

    pub(super) fn resolve_default_backend(
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

    pub(super) fn resolve_balance(
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

    pub(super) fn resolve_server(
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

    pub(super) fn resolve_default_server(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        let synthetic_word = |value: &[u8]| super::super::super::Word {
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

    pub(super) fn resolve_retries(
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

    pub(super) fn resolve_retry_on(
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

    pub(super) fn resolve_timeout(
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

    pub(super) fn resolve_maxconn(
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
}
