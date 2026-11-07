#[allow(clippy::wildcard_imports)]
use super::*;

impl<'a> Lowerer<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        graph: &'a SourceGraph,
        invocation: &'a InvocationFacts,
        backends: &'a [Backend],
        directors: &'a [Director],
        probes: &'a [Probe],
        modern_directors: &'a [ModernDirector],
        subroutines: &'a [Subroutine],
        statements: &'a [StatementDecision],
        imports: &'a [VmodImport],
        vmod_objects: &'a [VmodObject],
        diagnostics: Vec<Diagnostic>,
    ) -> Self {
        Self {
            graph,
            source_invocation: invocation,
            backends,
            directors,
            probes,
            modern_directors,
            subroutines,
            statements,
            imports,
            vmod_objects,
            diagnostics,
            blocker: None,
            draft: empty_config(),
            provenance: CanonicalProvenanceLedger::new(EmptyOriginPolicy::Discard),
            backend_infos: BTreeMap::new(),
            director_infos: BTreeMap::new(),
            invocation_policy: None,
            root_origin: graph.root.and_then(|root| {
                graph.source(root).map(|source| Provenance {
                    span: source.source.full_span(),
                    include_stack: Vec::new(),
                })
            }),
        }
    }

    pub(super) fn run(mut self) -> (VarnishCanonicalCandidate, LoweringStatus, Vec<Diagnostic>) {
        let has_canonical_graph = !self.subroutines.is_empty();
        self.scan_graph();
        if has_canonical_graph && !self.has_errors() {
            self.lower_invocation();
        }
        if has_canonical_graph && !self.has_errors() {
            self.lower_backends();
        }
        if has_canonical_graph && !self.has_errors() {
            self.lower_builtin_graph();
        }

        let draft = std::mem::replace(&mut self.draft, empty_config());
        let state = if has_canonical_graph {
            self.finalize(draft)
        } else {
            CanonicalCandidateState::Blocked(draft)
        };
        let status = if state.validated().is_some() {
            LoweringStatus::Lowered
        } else if !has_canonical_graph {
            LoweringStatus::Blocked(LoweringBlocker::NoCanonicalGraph)
        } else {
            LoweringStatus::Blocked(self.blocker.unwrap_or_else(|| {
                if self.has_source_errors() {
                    LoweringBlocker::InvalidSource
                } else {
                    LoweringBlocker::Validation
                }
            }))
        };
        let source_metadata = SourceImportMetadata {
            original_sources: self
                .graph
                .sources
                .iter()
                .map(|source| source.source.clone())
                .collect(),
            ..SourceImportMetadata::default()
        };
        let candidate = VarnishCanonicalCandidate::new(
            state,
            self.provenance.into_entries(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            source_metadata,
        );
        let ((), diagnostics) = crate::Report::new((), self.diagnostics).into_parts();
        (candidate, status, diagnostics)
    }

    #[allow(
        clippy::needless_borrow,
        clippy::too_many_lines,
        clippy::unnecessary_to_owned
    )]
    fn scan_graph(&mut self) {
        if !self.graph.snapshot_stable {
            self.block(
                LoweringBlocker::InvalidSource,
                E_SOURCE_CHANGED,
                DiagnosticStage::Source,
                "VCL source snapshot is not stable",
                self.root_span(),
            );
        }
        for import in self.imports.to_vec() {
            self.block(
                LoweringBlocker::UnsupportedBehavior,
                E_VCL_LOWERING_BLOCKED,
                DiagnosticStage::Lower,
                format!(
                    "VMOD import `{}` is outside the exact Varnish lowering subset",
                    display(&import.module)
                ),
                Some(import.provenance.span),
            );
        }
        for object in self.vmod_objects.to_vec() {
            self.block(
                LoweringBlocker::UnsupportedBehavior,
                E_VCL_LOWERING_BLOCKED,
                DiagnosticStage::Lower,
                format!(
                    "VMOD object `{}` is outside the exact Varnish lowering subset",
                    display(&object.name)
                ),
                Some(object.provenance.span),
            );
        }
        for director in self.modern_directors.to_vec() {
            self.block(
                LoweringBlocker::UnsupportedBehavior,
                E_VCL_LOWERING_BLOCKED,
                DiagnosticStage::Lower,
                format!(
                    "modern director `{}` is outside the exact Varnish lowering subset",
                    display(&director.name)
                ),
                Some(director.provenance.span),
            );
        }
        for (subroutine_index, subroutine) in self.subroutines.iter().enumerate() {
            let kind = subroutine.kind;
            let name = subroutine.name.clone();
            let span = subroutine.provenance.span;
            let has_statements = self
                .statements
                .iter()
                .any(|statement| statement.subroutine == subroutine_index);
            if kind == SubroutineKind::Custom {
                self.block(
                    LoweringBlocker::UnsupportedSubroutine,
                    E_VCL_UNSUPPORTED_SUBROUTINE,
                    DiagnosticStage::Lower,
                    format!("custom VCL subroutine `{}` is not lowered", display(&name)),
                    Some(span),
                );
            } else if !matches!(
                kind,
                SubroutineKind::Recv
                    | SubroutineKind::Hash
                    | SubroutineKind::BackendFetch
                    | SubroutineKind::BackendResponse
                    | SubroutineKind::Deliver
            ) && has_statements
            {
                self.block(
                    LoweringBlocker::UnsupportedSubroutine,
                    E_VCL_UNSUPPORTED_SUBROUTINE,
                    DiagnosticStage::Lower,
                    format!(
                        "VCL phase `{}` is outside the exact lowering graph",
                        display(&name)
                    ),
                    Some(span),
                );
            }
        }
        for statement in self.statements.to_vec() {
            match &statement.classification {
                StatementClassification::Conditional(_) => self.block_statement(
                    LoweringBlocker::SemanticMismatch,
                    E_VCL_SEMANTIC_MISMATCH,
                    &statement,
                    "conditional VCL control flow is not representable by the canonical route/cache graph",
                ),
                StatementClassification::SubroutineCall { .. } => self.block_statement(
                    LoweringBlocker::UnsupportedSubroutine,
                    E_VCL_UNSUPPORTED_SUBROUTINE,
                    &statement,
                    "subroutine calls are not lowered",
                ),
                StatementClassification::Dynamic(_) | StatementClassification::Unsupported(_) => {
                    self.block_statement(
                        LoweringBlocker::UnsupportedBehavior,
                        E_VCL_LOWERING_BLOCKED,
                        &statement,
                        "dynamic or unsupported VCL behavior is not lowered",
                    );
                }
                StatementClassification::Invalid => self.block_statement(
                    LoweringBlocker::InvalidSource,
                    E_VCL_LOWERING_BLOCKED,
                    &statement,
                    "invalid VCL statement cannot be lowered",
                ),
                StatementClassification::Invalidation(_) => self.block_statement(
                    LoweringBlocker::SemanticMismatch,
                    E_VCL_SEMANTIC_MISMATCH,
                    &statement,
                    "VCL invalidation is not equivalent to the canonical HTTP cache interface",
                ),
                StatementClassification::Feature(FeatureBehavior::Esi { .. }) => self.block_statement(
                    LoweringBlocker::UnsupportedBehavior,
                    E_UNSUPPORTED_FEATURE,
                    &statement,
                    "ESI is outside the exact Varnish lowering subset",
                ),
                StatementClassification::Response(_) => {
                    self.block_statement(
                        LoweringBlocker::UnsupportedBehavior,
                        E_UNSUPPORTED_FEATURE,
                        &statement,
                        "VCL response generation is outside the exact HTTP route subset",
                    );
                }
                StatementClassification::NewDirector { .. }
                | StatementClassification::DirectorMethod { .. } => self.block_statement(
                    LoweringBlocker::UnsupportedBehavior,
                    E_VCL_LOWERING_BLOCKED,
                    &statement,
                    "runtime director construction is outside the exact lowering subset",
                ),
                _ => {}
            }
        }
    }

    pub(super) fn backend_origin(&self, declaration: usize) -> Option<Provenance> {
        self.backends
            .get(declaration)
            .map(|backend| backend.provenance.clone())
    }

    pub(super) fn director_origin(&self, declaration: usize) -> Option<Provenance> {
        self.directors
            .get(declaration)
            .map(|director| director.provenance.clone())
    }

    pub(super) fn pool_origin(&self, pool: &UpstreamPool) -> Option<Provenance> {
        self.backends
            .iter()
            .find(|backend| {
                String::from_utf8(backend.name.clone()).ok().as_deref() == Some(pool.name.as_str())
            })
            .map(|backend| backend.provenance.clone())
            .or_else(|| {
                self.directors
                    .iter()
                    .find(|director| {
                        String::from_utf8(director.name.clone()).ok().as_deref()
                            == Some(pool.name.as_str())
                    })
                    .map(|director| director.provenance.clone())
            })
            .or_else(|| self.root_origin.clone())
    }

    pub(super) fn phase(&self, kind: SubroutineKind) -> Vec<StatementDecision> {
        let indices = self
            .subroutines
            .iter()
            .enumerate()
            .filter(|(_, subroutine)| subroutine.kind == kind)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        self.statements
            .iter()
            .filter(|statement| indices.contains(&statement.subroutine))
            .cloned()
            .collect()
    }

    pub(super) fn phase_origin(&self, kind: SubroutineKind) -> Vec<Provenance> {
        self.phase(kind)
            .iter()
            .map(|statement| statement.provenance.clone())
            .collect()
    }

    fn finalize(&mut self, draft: ConfigDraft) -> CanonicalCandidateState {
        match finalize_candidate(&draft, !self.has_errors()) {
            Ok(Some(config)) => CanonicalCandidateState::Validated(config),
            Ok(None) => CanonicalCandidateState::Blocked(draft),
            Err(error) => {
                self.block(
                    LoweringBlocker::Validation,
                    E_INVALID_VALUE,
                    DiagnosticStage::Validate,
                    format!("lowered Varnish canonical draft is invalid: {error}"),
                    self.root_span(),
                );
                CanonicalCandidateState::Blocked(draft)
            }
        }
    }

    pub(super) fn record(&mut self, path: String, origins: Vec<Provenance>) {
        self.provenance.record_in_order(path, origins, |origin| {
            (origin.span, origin.include_stack.clone())
        });
    }

    pub(super) fn block_backend(&mut self, backend: &Backend, message: &'static str) {
        self.block(
            LoweringBlocker::SemanticMismatch,
            E_VCL_SEMANTIC_MISMATCH,
            DiagnosticStage::Lower,
            message,
            Some(backend.provenance.span),
        );
    }

    pub(super) fn block_director(&mut self, director: &Director, message: &'static str) {
        self.block(
            LoweringBlocker::SemanticMismatch,
            E_VCL_SEMANTIC_MISMATCH,
            DiagnosticStage::Lower,
            message,
            Some(director.provenance.span),
        );
    }

    pub(super) fn block_statement(
        &mut self,
        blocker: LoweringBlocker,
        code: crate::DiagnosticCode,
        statement: &StatementDecision,
        message: &'static str,
    ) {
        self.block(
            blocker,
            code,
            DiagnosticStage::Lower,
            message,
            Some(statement.provenance.span),
        );
    }

    pub(super) fn block_with_provenance(
        &mut self,
        blocker: LoweringBlocker,
        code: crate::DiagnosticCode,
        message: &'static str,
        provenance: &Provenance,
    ) {
        self.blocker.get_or_insert(blocker);
        self.diagnostics.push(
            Diagnostic::new(code, Severity::Error, DiagnosticStage::Lower, message)
                .with_primary_span(provenance.span)
                .with_include_stack(provenance.include_stack.iter().copied()),
        );
    }

    pub(super) fn block(
        &mut self,
        blocker: LoweringBlocker,
        code: crate::DiagnosticCode,
        stage: DiagnosticStage,
        message: impl Into<String>,
        span: Option<crate::Span>,
    ) {
        self.blocker.get_or_insert(blocker);
        let diagnostic = Diagnostic::new(code, Severity::Error, stage, message).with_include_stack(
            span.and_then(|span| self.provenance_for_span(span))
                .map(|provenance| provenance.include_stack)
                .unwrap_or_default(),
        );
        self.diagnostics.push(if let Some(span) = span {
            diagnostic.with_primary_span(span)
        } else {
            diagnostic
        });
    }

    fn provenance_for_span(&self, span: crate::Span) -> Option<Provenance> {
        self.statements
            .iter()
            .find(|statement| statement.provenance.span == span)
            .map(|statement| statement.provenance.clone())
            .or_else(|| self.root_origin.clone())
    }

    fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity() == Severity::Error)
    }

    fn has_source_errors(&self) -> bool {
        self.diagnostics.iter().any(|diagnostic| {
            diagnostic.severity() == Severity::Error
                && matches!(diagnostic.code(), E_SOURCE_CHANGED | E_SOURCE_LIMIT)
        })
    }

    pub(super) fn root_span(&self) -> Option<crate::Span> {
        self.root_origin.as_ref().map(|origin| origin.span)
    }
}
