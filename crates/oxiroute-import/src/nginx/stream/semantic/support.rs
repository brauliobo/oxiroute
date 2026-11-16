#[allow(clippy::wildcard_imports)]
use super::*;

impl Resolver<'_> {
    pub(super) fn resolve_timeout(&mut self, directive: &ExpandedDirective) -> Option<u64> {
        let valid_shape = directive.children.is_none() && directive.directive.arguments.len() == 1;
        if !valid_shape {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "stream timeout requires one duration and a semicolon",
            );
            return None;
        }
        let value = &directive.directive.arguments[0].value;
        if has_variable(value) {
            self.block(
                directive.occurrence,
                E_UNSUPPORTED_FEATURE,
                "variables in stream timeouts are unsupported",
            );
            return None;
        }
        if let Some(value) = parse_duration_ms(value) {
            self.resolved(directive.occurrence);
            Some(value)
        } else {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "stream timeout must be a positive whole-millisecond duration",
            );
            None
        }
    }

    pub(super) fn resolve_off_only_flag(
        &mut self,
        directive: &ExpandedDirective,
        message: &'static str,
    ) {
        if directive.children.is_none()
            && directive.directive.arguments.len() == 1
            && directive.directive.arguments[0].value == b"off"
        {
            self.resolved(directive.occurrence);
        } else if directive.children.is_some() || directive.directive.arguments.len() != 1 {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "stream flag requires one literal value and a semicolon",
            );
        } else {
            self.block(directive.occurrence, E_UNSUPPORTED_FEATURE, message);
        }
    }

    pub(super) fn resolve_on_or_off_flag(
        &mut self,
        directive: &ExpandedDirective,
        context: &'static str,
    ) -> bool {
        if directive.children.is_none()
            && directive.directive.arguments.len() == 1
            && matches!(
                directive.directive.arguments[0].value.as_slice(),
                b"on" | b"off"
            )
        {
            self.resolved(directive.occurrence);
            true
        } else if directive.children.is_some() || directive.directive.arguments.len() != 1 {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "stream flag requires one literal value and a semicolon",
            );
            false
        } else {
            self.block(directive.occurrence, E_UNSUPPORTED_FEATURE, context);
            false
        }
    }

    pub(super) fn reject_duplicate_scalar(
        &mut self,
        directive: &ExpandedDirective,
        scalar_policies: &mut HashMap<Vec<u8>, OccurrenceId>,
    ) -> bool {
        if let Some(first) =
            scalar_policies.insert(directive.directive.name.value.clone(), directive.occurrence)
        {
            self.block_related(
                directive.occurrence,
                E_DUPLICATE_IDENTITY,
                "duplicate nginx stream scalar directive",
                first,
            );
            true
        } else {
            false
        }
    }

    pub(super) fn reject_overlapping_listens(&mut self, streams: &[EffectiveStream]) {
        let listens = streams
            .iter()
            .flat_map(|stream| &stream.servers)
            .flat_map(|server| &server.listens)
            .filter_map(|listen| {
                listen
                    .endpoint
                    .as_ref()
                    .map(|endpoint| (endpoint, listen.origin.occurrence))
            })
            .collect::<Vec<_>>();
        for (index, (first, first_origin)) in listens.iter().enumerate() {
            for (second, second_origin) in &listens[index + 1..] {
                if first == second || listen_endpoints_overlap(first, second) {
                    self.block_related(
                        *second_origin,
                        E_DUPLICATE_IDENTITY,
                        "overlapping nginx stream listen sockets cannot be represented as independent TCP listeners",
                        *first_origin,
                    );
                    self.block_related(
                        *first_origin,
                        E_DUPLICATE_IDENTITY,
                        "overlapping nginx stream listen sockets cannot be represented as independent TCP listeners",
                        *second_origin,
                    );
                }
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
        } else {
            self.resolved(occurrence);
        }
    }

    pub(super) fn block_subtree(&mut self, directive: &ExpandedDirective, message: &'static str) {
        self.block(directive.occurrence, E_UNSUPPORTED_FEATURE, message);
        for child in directive.children.as_deref().unwrap_or_default() {
            self.block_subtree(child, message);
        }
    }

    pub(super) fn structural_subtree(&mut self, directive: &ExpandedDirective) {
        self.dispositions[directive.occurrence.get()] = Some(OccurrenceDisposition::Structural);
        for child in directive.children.as_deref().unwrap_or_default() {
            self.structural_subtree(child);
        }
    }

    pub(super) fn classify_remaining(&mut self) {
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
                } else if has_variable(&occurrence.directive.arguments[0].value) {
                    self.block(
                        occurrence.id,
                        E_UNSUPPORTED_FEATURE,
                        "variables in include paths are unsupported",
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
                    "reachable nginx directive was not resolved",
                );
            }
        }
    }

    pub(super) fn resolved(&mut self, occurrence: OccurrenceId) {
        let disposition = &mut self.dispositions[occurrence.get()];
        if disposition.is_none() {
            *disposition = Some(OccurrenceDisposition::Resolved);
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
                .with_related_span(first.directive.span, "first identity declared here")
                .with_include_stack(
                    expanded
                        .provenance
                        .include_stack
                        .iter()
                        .map(|frame| frame.directive_span),
                ),
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

    pub(super) fn decision(&self, occurrence: &ExpandedOccurrence) -> OccurrenceDecision {
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
