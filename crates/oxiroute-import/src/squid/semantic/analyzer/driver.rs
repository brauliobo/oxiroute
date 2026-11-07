#[allow(clippy::wildcard_imports)]
use super::*;

impl<'a> Analyzer<'a> {
    pub(super) fn new(graph: &'a SourceGraph) -> Self {
        Self {
            graph,
            effective: EffectiveConfiguration::default(),
            decisions: vec![None; graph.expanded_directives.len()],
            diagnostics: Vec::new(),
        }
    }

    pub(super) fn run(mut self) -> Report<EffectiveConfiguration> {
        for expanded in &self.graph.expanded_directives {
            self.classify(expanded);
        }
        self.merge_acls();
        self.resolve_access();
        self.resolve_direct_access();
        self.resolve_authentication();
        self.terminal_accounting();
        Report::new(self.effective, self.diagnostics)
    }

    fn classify(&mut self, expanded: &ExpandedDirective) {
        let name = expanded.directive.name.value.as_slice();
        match name {
            b"include" => self.include(expanded),
            b"acl" => self.acl(expanded),
            b"request_header_access" | b"reply_header_access" => self.header_access(expanded),
            name if access_kind(name).is_some() => self.access(expanded),
            b"http_port" => self.port(expanded, PortKind::Http),
            b"https_port" => self.port(expanded, PortKind::Https),
            b"icp_port" => self.port(expanded, PortKind::Icp),
            b"htcp_port" => self.port(expanded, PortKind::Htcp),
            b"cache_peer" => self.cache_peer(expanded),
            b"refresh_pattern" => self.refresh(expanded),
            name if is_cache_policy(name) => self.cache(expanded),
            name if is_storage(name) => self.storage(expanded),
            b"auth_param" => self.auth_param(expanded),
            name if is_authentication(name) => self.authentication_control(expanded),
            b"access_log" => self.access_log(expanded),
            name if is_logging(name) => self.logging_control(expanded),
            b"dns_nameservers" => self.dns_nameservers(expanded),
            name if is_dns(name) => self.dns_control(expanded),
            b"forwarded_for" => self.forwarded_for(expanded),
            b"via" => self.via(expanded),
            b"request_header_replace" | b"reply_header_replace" => self.header_replace(expanded),
            name if is_process(name) => self.process(expanded),
            _ => self.unknown(expanded),
        }
    }

    fn include(&mut self, expanded: &ExpandedDirective) {
        let activation = self
            .graph
            .includes
            .iter()
            .find(|edge| edge.occurrence == expanded.occurrence)
            .filter(|edge| edge.failure.is_some())
            .map_or(Activation::Structural, |_| {
                Activation::Blocked(SemanticBlockerKind::IncludeExpansion)
            });
        self.record(
            expanded,
            DirectiveFamily::Include,
            DirectiveSemantics::Include,
            activation,
        );
    }

    fn terminal_accounting(&mut self) {
        for expanded in &self.graph.expanded_directives {
            if self.decisions[expanded.occurrence.get()].is_none() {
                self.diagnostics.push(Self::diagnostic(
                    expanded,
                    E_UNCONSUMED_DIRECTIVE,
                    "Squid directive escaped terminal semantic accounting",
                ));
                self.record(
                    expanded,
                    DirectiveFamily::Unknown,
                    DirectiveSemantics::Unknown,
                    Activation::Blocked(SemanticBlockerKind::UnknownDirective),
                );
            }
        }
        self.effective.ledger = DecisionLedger {
            decisions: self
                .decisions
                .drain(..)
                .map(|decision| decision.expect("terminal accounting fills every occurrence"))
                .collect(),
        };
    }

    fn unknown(&mut self, expanded: &ExpandedDirective) {
        self.diagnostics.push(Self::diagnostic(
            expanded,
            E_UNKNOWN_DIRECTIVE,
            "Squid directive is not registered by the strict importer",
        ));
        self.record(
            expanded,
            DirectiveFamily::Unknown,
            DirectiveSemantics::Unknown,
            Activation::Blocked(SemanticBlockerKind::UnknownDirective),
        );
    }

    pub(super) fn invalid(
        &mut self,
        expanded: &ExpandedDirective,
        family: DirectiveFamily,
        semantics: DirectiveSemantics,
        message: &'static str,
    ) {
        self.diagnostics
            .push(Self::diagnostic(expanded, E_UNSUPPORTED_FORM, message));
        self.record(
            expanded,
            family,
            semantics,
            Activation::Blocked(SemanticBlockerKind::InvalidForm),
        );
    }

    pub(super) fn block_occurrence(
        &mut self,
        occurrence: OccurrenceId,
        blocker: SemanticBlockerKind,
    ) {
        let Some(decision) = self
            .decisions
            .get_mut(occurrence.get())
            .and_then(Option::as_mut)
        else {
            return;
        };
        let DecisionOutcome::Classified {
            family,
            semantics,
            resolution,
            ..
        } = decision.outcome;
        decision.outcome = DecisionOutcome::Classified {
            family,
            semantics,
            resolution,
            activation: Activation::Blocked(blocker),
        };
    }

    pub(super) fn activate_occurrence(&mut self, occurrence: OccurrenceId) {
        let Some(decision) = self
            .decisions
            .get_mut(occurrence.get())
            .and_then(Option::as_mut)
        else {
            return;
        };
        let DecisionOutcome::Classified {
            family,
            semantics,
            resolution,
            ..
        } = decision.outcome;
        decision.outcome = DecisionOutcome::Classified {
            family,
            semantics,
            resolution,
            activation: Activation::Structural,
        };
    }

    pub(super) fn record(
        &mut self,
        expanded: &ExpandedDirective,
        family: DirectiveFamily,
        semantics: DirectiveSemantics,
        activation: Activation,
    ) {
        self.decisions[expanded.occurrence.get()] = Some(Decision {
            origin: origin(expanded),
            name: expanded.directive.name.value.clone(),
            outcome: DecisionOutcome::Classified {
                family,
                semantics,
                resolution: resolution_for(semantics, activation),
                activation,
            },
        });
    }

    pub(super) fn diagnostic(
        expanded: &ExpandedDirective,
        code: crate::DiagnosticCode,
        message: &'static str,
    ) -> Diagnostic {
        Diagnostic::new(code, Severity::Error, DiagnosticStage::Resolve, message)
            .with_primary_span(expanded.directive.span)
            .with_include_stack(
                expanded
                    .provenance
                    .include_stack
                    .iter()
                    .map(|frame| frame.directive_span),
            )
    }
}
