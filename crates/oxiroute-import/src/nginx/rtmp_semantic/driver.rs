#[allow(clippy::wildcard_imports)]
use super::*;

impl<'a> Resolver<'a> {
    pub(super) fn new(graph: &'a SourceGraph, complete_root: bool) -> Self {
        Self {
            graph,
            dispositions: vec![None; graph.expanded_occurrences.len()],
            diagnostics: Vec::new(),
            complete_root,
        }
    }

    pub(super) fn run(mut self) -> Report<RtmpResolution> {
        let mut rtmp_blocks = Vec::new();
        let mut first_rtmp = None;
        let root_auto_push = self.resolve_root_auto_push_policy();

        for directive in &self.graph.expanded_directives {
            if directive.directive.name.value == b"rtmp" {
                if let Some(first) = first_rtmp {
                    self.block_related(
                        directive.occurrence,
                        E_DUPLICATE_IDENTITY,
                        "nginx permits only one effective rtmp block",
                        first,
                    );
                } else {
                    first_rtmp = Some(directive.occurrence);
                }
                rtmp_blocks.push(self.resolve_rtmp_block(directive, &root_auto_push));
            } else if is_root_auto_push_policy(&directive.directive.name.value) {
                // The exact nginx directives are resolved once at Nginx-main scope above.
            } else if self.complete_root {
                self.structural_subtree(directive);
            } else if Self::is_registered_in_context(directive, DirectiveContext::NginxMain) {
                self.resolve_unsupported_subtree(
                    directive,
                    DirectiveContext::NginxMain,
                    unsupported_rtmp_reason(&directive.directive.name.value),
                );
            } else {
                self.structural_subtree(directive);
            }
        }

        self.reject_overlapping_listens(&rtmp_blocks);
        self.classify_remaining();
        let decisions = self
            .graph
            .expanded_occurrences
            .iter()
            .map(|occurrence| self.decision(occurrence))
            .collect();

        Report::new(
            RtmpResolution {
                rtmp_blocks,
                decisions,
            },
            self.diagnostics,
        )
    }

    fn resolve_root_auto_push_policy(&mut self) -> Policy {
        let directives: Vec<_> = self
            .graph
            .expanded_directives
            .iter()
            .filter(|directive| is_root_auto_push_policy(&directive.directive.name.value))
            .cloned()
            .collect();
        self.resolve_local_policy(&directives, DirectiveContext::NginxMain, Policy::default())
    }

    pub(super) fn resolve_block_header(
        &mut self,
        directive: &ExpandedDirective,
        context: DirectiveContext,
        name: &'static str,
    ) {
        if self.validate_registered(directive, context).is_err() {
            return;
        }
        if directive.children.is_none() {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "nginx-RTMP structural directive requires a block",
            );
        } else {
            debug_assert_eq!(directive.directive.name.value, name.as_bytes());
            self.resolved(directive.occurrence);
        }
    }

    pub(super) fn resolve_unsupported_subtree(
        &mut self,
        directive: &ExpandedDirective,
        context: DirectiveContext,
        message: &'static str,
    ) {
        if self.validate_registered(directive, context).is_ok() {
            self.block(directive.occurrence, E_UNSUPPORTED_FEATURE, message);
        }
        for child in directive.children.as_deref().unwrap_or_default() {
            self.block_subtree(child, message);
        }
    }

    pub(super) fn resolve_source_noop(
        &mut self,
        directive: &ExpandedDirective,
        context: DirectiveContext,
    ) {
        if self.validate_registered(directive, context).is_ok() {
            self.resolved(directive.occurrence);
        }
    }

    pub(super) fn validate_registered(
        &mut self,
        directive: &ExpandedDirective,
        context: DirectiveContext,
    ) -> Result<(), ()> {
        let Ok(name) = std::str::from_utf8(&directive.directive.name.value) else {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "nginx-RTMP directive name must be UTF-8",
            );
            return Err(());
        };
        let Some(arguments) = directive
            .directive
            .arguments
            .iter()
            .map(|argument| std::str::from_utf8(&argument.value).ok())
            .collect::<Option<Vec<_>>>()
        else {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "nginx-RTMP directive arguments must be UTF-8",
            );
            return Err(());
        };
        match validate_directive(name, context, &arguments) {
            Ok(_) => Ok(()),
            Err(DirectiveError::UnknownDirective(_) | DirectiveError::InvalidContext { .. }) => {
                self.block(
                    directive.occurrence,
                    E_UNSUPPORTED_FEATURE,
                    "directive is not registered for this nginx-RTMP context",
                );
                Err(())
            }
            Err(DirectiveError::InvalidArity { .. } | DirectiveError::InvalidValue { .. }) => {
                self.block(
                    directive.occurrence,
                    E_INVALID_VALUE,
                    "nginx-RTMP directive has invalid registered syntax",
                );
                Err(())
            }
        }
    }

    fn is_registered_in_context(directive: &ExpandedDirective, context: DirectiveContext) -> bool {
        let Ok(name) = std::str::from_utf8(&directive.directive.name.value) else {
            return false;
        };
        directive_specs()
            .iter()
            .any(|spec| spec.name == name && spec.contexts.contains(&context))
    }

    fn structural_subtree(&mut self, directive: &ExpandedDirective) {
        self.structural(directive.occurrence);
        for child in directive.children.as_deref().unwrap_or_default() {
            self.structural_subtree(child);
        }
    }

    fn block_subtree(&mut self, directive: &ExpandedDirective, message: &'static str) {
        self.block(directive.occurrence, E_UNSUPPORTED_FEATURE, message);
        for child in directive.children.as_deref().unwrap_or_default() {
            self.block_subtree(child, message);
        }
    }

    fn classify_remaining(&mut self) {
        for index in 0..self.graph.expanded_occurrences.len() {
            if self.dispositions[index].is_some() {
                continue;
            }
            let occurrence = &self.graph.expanded_occurrences[index];
            if occurrence.directive.name.value == b"include" {
                if occurrence.directive.arguments.len() != 1
                    || occurrence.directive.children.is_some()
                {
                    self.block(
                        occurrence.id,
                        E_INVALID_VALUE,
                        "include requires one path and a semicolon",
                    );
                } else {
                    let failure = self
                        .graph
                        .includes
                        .iter()
                        .find(|edge| edge.occurrence == occurrence.id)
                        .and_then(include_failure);
                    self.dispositions[index] = Some(failure.map_or(
                        OccurrenceDisposition::Structural,
                        OccurrenceDisposition::Blocking,
                    ));
                }
            } else {
                self.block(
                    occurrence.id,
                    E_UNSUPPORTED_FEATURE,
                    "reachable nginx-RTMP directive was not resolved",
                );
            }
        }
    }

    pub(super) fn finish_occurrence(
        &mut self,
        occurrence: OccurrenceId,
        outcome: Option<(DiagnosticCode, &'static str)>,
    ) {
        if let Some((code, message)) = outcome {
            self.block(occurrence, code, message);
        } else if self.dispositions[occurrence.get()].is_none() {
            self.resolved(occurrence);
        }
    }

    pub(super) fn resolved(&mut self, occurrence: OccurrenceId) {
        if self.dispositions[occurrence.get()].is_none() {
            self.dispositions[occurrence.get()] = Some(OccurrenceDisposition::Resolved);
        }
    }

    fn structural(&mut self, occurrence: OccurrenceId) {
        if self.dispositions[occurrence.get()].is_none() {
            self.dispositions[occurrence.get()] = Some(OccurrenceDisposition::Structural);
        }
    }

    pub(super) fn block(
        &mut self,
        occurrence: OccurrenceId,
        code: DiagnosticCode,
        message: &'static str,
    ) {
        if matches!(
            self.dispositions[occurrence.get()],
            Some(OccurrenceDisposition::Blocking(_))
        ) {
            return;
        }
        self.dispositions[occurrence.get()] = Some(OccurrenceDisposition::Blocking(code));
        let expanded = self.occurrence(occurrence);
        self.diagnostics.push(
            Diagnostic::new(code, Severity::Error, DiagnosticStage::Resolve, message)
                .with_primary_span(expanded.directive.span)
                .with_include_stack(
                    expanded
                        .provenance
                        .include_stack
                        .iter()
                        .map(|frame| frame.directive_span),
                ),
        );
    }

    pub(super) fn block_related(
        &mut self,
        occurrence: OccurrenceId,
        code: DiagnosticCode,
        message: &'static str,
        first: OccurrenceId,
    ) {
        if matches!(
            self.dispositions[occurrence.get()],
            Some(OccurrenceDisposition::Blocking(_))
        ) {
            return;
        }
        self.dispositions[occurrence.get()] = Some(OccurrenceDisposition::Blocking(code));
        let expanded = self.occurrence(occurrence);
        let first = self.occurrence(first);
        self.diagnostics.push(
            Diagnostic::new(code, Severity::Error, DiagnosticStage::Resolve, message)
                .with_primary_span(expanded.directive.span)
                .with_include_stack(
                    expanded
                        .provenance
                        .include_stack
                        .iter()
                        .map(|frame| frame.directive_span),
                )
                .with_related_span(first.directive.span, "first identity declared here"),
        );
    }

    fn occurrence(&self, id: OccurrenceId) -> &ExpandedOccurrence {
        let occurrence = &self.graph.expanded_occurrences[id.get()];
        debug_assert_eq!(occurrence.id, id);
        occurrence
    }

    pub(super) fn origin(directive: &ExpandedDirective) -> DirectiveOrigin {
        DirectiveOrigin {
            occurrence: directive.occurrence,
            span: directive.directive.span,
            provenance: directive.provenance.clone(),
        }
    }

    pub(super) fn value(&self, word: &Word) -> NginxValue {
        let source = self
            .graph
            .source(word.span.source())
            .expect("expanded word source is retained in the graph");
        NginxValue {
            value: word.value.clone(),
            raw: source
                .source
                .slice(word.span.range())
                .expect("expanded word span is within its source")
                .to_vec(),
            span: word.span,
        }
    }

    fn decision(&self, occurrence: &ExpandedOccurrence) -> OccurrenceDecision {
        OccurrenceDecision {
            occurrence: occurrence.id,
            parent: occurrence.parent,
            name: self.value(&occurrence.directive.name),
            arguments: occurrence
                .directive
                .arguments
                .iter()
                .map(|word| self.value(word))
                .collect(),
            span: occurrence.directive.span,
            provenance: occurrence.provenance.clone(),
            disposition: self.dispositions[occurrence.id.get()]
                .expect("every expanded occurrence has a terminal disposition"),
        }
    }
}
