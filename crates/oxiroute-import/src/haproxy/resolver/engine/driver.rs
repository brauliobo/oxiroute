#[allow(clippy::wildcard_imports)]
use super::*;

impl Resolver {
    pub(super) fn new(configuration: &Configuration) -> Self {
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

    pub(super) fn run(mut self) -> Report<EffectiveConfiguration> {
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

    pub(super) fn resolve_defaults_reference(
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

    pub(super) fn resolve_backend_reference(
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

    pub(super) fn unresolved_reference(
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
