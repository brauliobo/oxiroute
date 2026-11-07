#[allow(clippy::wildcard_imports)]
use super::*;

impl Resolver {
    pub(super) fn reject_unsupported_section(&mut self, index: usize) {
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

    pub(super) fn block_section_directives(&mut self, index: usize, reason: BlockingReason) {
        let id = self.sections[index].id;
        let count = self.sections[index].section.directives.len();
        for directive_ordinal in 0..count {
            self.block(section_directive_id(id, directive_ordinal), reason);
        }
    }

    pub(super) fn block_preprocessing(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
    ) -> bool {
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

    pub(super) fn block_environment(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
    ) -> bool {
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

    pub(super) fn reject_semantic_directive(
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

    pub(super) fn track_and_reject_semantics(
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

    pub(super) fn block_bind_semantics(
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

    pub(super) fn conflicting_directive(
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

    pub(super) fn conflicting_option(
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

    pub(super) fn finish_setting(
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

    pub(super) fn externalize_process_setting(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
    ) {
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

    pub(super) fn externalize_log_transport(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
    ) {
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

    pub(super) fn externalize_activation(
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

    pub(super) fn unknown_directive(
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

    pub(super) fn unsupported_directive_form(
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

    pub(super) fn unsupported_directive_form_for_occurrence(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
    ) {
        let kind = self
            .section_kind(occurrence)
            .expect("directive occurrence belongs to a section");
        self.unsupported_directive_form(occurrence, directive, kind);
    }

    pub(super) fn unsupported_form(
        &mut self,
        occurrence: OccurrenceId,
        span: Span,
        message: String,
    ) {
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

    pub(super) fn set_setting<T: PartialEq>(
        &mut self,
        slot: &mut Option<EffectiveValue<T>>,
        value: EffectiveValue<T>,
    ) -> Option<Span> {
        set_value(slot, value, &mut self.decisions, &self.decision_indices)
    }

    pub(super) fn consume(&mut self, occurrence: OccurrenceId, consumption: Consumption) {
        let decision = self.pending_decision_mut(occurrence);
        if decision.outcome.is_none() {
            decision.outcome = Some(DecisionOutcome::Consumed(consumption));
        }
    }

    pub(super) fn block(&mut self, occurrence: OccurrenceId, reason: BlockingReason) {
        let decision = self.pending_decision_mut(occurrence);
        if decision.outcome.is_none() {
            decision.outcome = Some(DecisionOutcome::Blocked(reason));
        }
    }

    pub(super) fn pending_decision_mut(
        &mut self,
        occurrence: OccurrenceId,
    ) -> &mut PendingDecision {
        let index = self.decision_indices[&occurrence];
        &mut self.decisions[index]
    }

    fn section(&self, id: SectionId) -> &SectionMeta {
        self.sections
            .iter()
            .find(|section| section.id == id)
            .expect("section occurrence was indexed")
    }

    pub(super) fn section_kind(&self, occurrence: OccurrenceId) -> Option<SectionKind> {
        match occurrence {
            OccurrenceId::Preamble { .. } => None,
            OccurrenceId::SectionHeader(id)
            | OccurrenceId::SectionDirective { section: id, .. } => {
                Some(self.section(id).section.kind)
            }
        }
    }
}

pub(super) fn push_pending_decision(
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

pub(super) fn semantic_blocker(
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

pub(super) fn set_value<T: PartialEq>(
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

pub(super) fn set_idempotent_value<T: PartialEq>(
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
