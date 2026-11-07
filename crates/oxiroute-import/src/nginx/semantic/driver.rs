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

    pub(super) fn run(mut self) -> Report<HttpResolution> {
        let mut http_blocks = Vec::new();
        let mut first_http = None;

        for directive in &self.graph.expanded_directives {
            if directive.directive.name.value == b"http" {
                if let Some(first) = first_http {
                    self.block_related(
                        directive.occurrence,
                        E_DUPLICATE_IDENTITY,
                        "nginx permits only one effective http block",
                        first,
                    );
                } else {
                    first_http = Some(directive.occurrence);
                }
                http_blocks.push(self.resolve_http_block(directive));
            } else if self.complete_root {
                self.structural_subtree(directive);
            } else {
                let message = if directive.directive.name.value == b"events" {
                    "complete nginx configuration is not an HTTP fragment; expected only an http block"
                } else {
                    "directive is outside the nginx HTTP fragment root"
                };
                self.block_subtree(directive, message);
            }
        }

        self.classify_remaining();
        let decisions = self
            .graph
            .expanded_occurrences
            .iter()
            .map(|occurrence| self.decision(occurrence))
            .collect();

        Report::new(
            HttpResolution {
                http_blocks,
                decisions,
            },
            self.diagnostics,
        )
    }

    fn resolve_http_block(&mut self, directive: &ExpandedDirective) -> EffectiveHttp {
        let shape_valid = directive.directive.arguments.is_empty() && directive.children.is_some();
        if shape_valid {
            self.resolved(directive.occurrence);
        } else {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "http must be an argument-free block",
            );
        }

        let children = directive.children.as_deref().unwrap_or_default();
        let mut upstreams = Vec::new();
        let mut upstream_by_name = HashMap::new();

        for child in children {
            if child.directive.name.value != b"upstream" {
                continue;
            }
            let upstream = self.resolve_upstream(child);
            if child.directive.arguments.len() == 1
                && child.directive.children.is_some()
                && let Some(name) = &upstream.name
            {
                let normalized = ascii_lowercase(&name.value);
                if has_variable(&name.value) {
                    self.block(
                        child.occurrence,
                        E_UNSUPPORTED_FEATURE,
                        "variables in upstream names are unsupported",
                    );
                } else if let Some(first) = upstream_by_name.get(&normalized).copied() {
                    self.block_related(
                        child.occurrence,
                        E_DUPLICATE_IDENTITY,
                        "duplicate nginx upstream identity",
                        first,
                    );
                } else {
                    upstream_by_name.insert(normalized, child.occurrence);
                }
            }
            upstreams.push(upstream);
        }

        let mut servers = Vec::new();
        let mut declaration_order = Vec::new();
        let mut scalar_policies = HashMap::new();
        for child in children {
            match child.directive.name.value.as_slice() {
                b"upstream" => declaration_order.push(HttpDeclaration::Upstream(child.occurrence)),
                b"server" => {
                    declaration_order.push(HttpDeclaration::Server(child.occurrence));
                    servers.push(self.resolve_server(child, &upstream_by_name));
                }
                b"types" => self.resolve_types(child),
                name if is_http_policy(name) => {
                    self.resolve_policy(child);
                    self.reject_duplicate_scalar(child, &mut scalar_policies);
                }
                _ => {
                    self.block_subtree(child, "directive is unsupported in the nginx http context");
                }
            }
        }

        let binds = self.resolve_binds(&servers);
        EffectiveHttp {
            origin: Self::origin(directive),
            declaration_order,
            upstreams,
            servers,
            binds,
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

    pub(super) fn warn_related(
        &mut self,
        occurrence: OccurrenceId,
        code: DiagnosticCode,
        message: &'static str,
        related: OccurrenceId,
    ) {
        let expanded = self.occurrence(occurrence);
        let first = self.occurrence(related);
        self.diagnostics.push(
            Diagnostic::new(code, Severity::Warning, DiagnosticStage::Resolve, message)
                .with_primary_span(expanded.directive.span)
                .with_related_span(first.directive.span, "first-loaded declaration is here")
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
