#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockedCapability {
    pub kind: SemanticBlockerKind,
    pub occurrences: Vec<OccurrenceId>,
    pub diagnostic_code: DiagnosticCode,
}

/// Complete Squid source, semantic, blocker, and canonicalization evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportReport {
    pub source_graph: SourceGraph,
    pub effective: EffectiveConfiguration,
    pub decision_ledger: DecisionLedger,
    pub capabilities: SquidCapabilityReport,
    pub blocked_capabilities: Vec<BlockedCapability>,
    pub candidate: CanonicalCandidate<DirectiveOrigin>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedImportReport {
    pub selection: RootSelection,
    pub import: ImportReport,
}

/// Schema-independent input handed to a future canonical lowering implementation.
#[derive(Clone, Copy, Debug)]
pub struct LoweringView<'a> {
    pub effective: &'a EffectiveConfiguration,
    pub decision_ledger: &'a DecisionLedger,
    pub blocked_capabilities: &'a [BlockedCapability],
}

/// Adapter boundary for canonical forward-proxy/cache schema implementations.
pub trait SquidLoweringAdapter {
    type Output;
    type Error;

    /// # Errors
    ///
    /// Returns an adapter-defined error when the target schema cannot represent the typed view.
    fn lower(&self, source: LoweringView<'_>) -> Result<Self::Output, Self::Error>;
}

impl ImportReport {
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity() == Severity::Error)
    }

    /// # Errors
    ///
    /// Returns the error produced by `adapter`.
    pub fn lower_with<A: SquidLoweringAdapter>(&self, adapter: &A) -> Result<A::Output, A::Error> {
        adapter.lower(LoweringView {
            effective: &self.effective,
            decision_ledger: &self.decision_ledger,
            blocked_capabilities: &self.blocked_capabilities,
        })
    }
}

/// Loads, classifies, and audits one active Squid configuration graph.
#[must_use]
pub fn import(root: &Path) -> ImportReport {
    let (graph, diagnostics) = load(root).into_parts();
    import_graph(graph, diagnostics)
}

/// Discovers the active native CLI root, then runs the report-preserving import pipeline.
#[must_use]
pub fn import_selected(arguments: &[OsString], compiled_default: &Path) -> SelectedImportReport {
    let (selected, diagnostics) = load_selected(arguments, compiled_default).into_parts();
    let import = import_graph(selected.graph, diagnostics);
    SelectedImportReport {
        selection: selected.selection,
        import,
    }
}

#[allow(clippy::too_many_lines)]
fn import_graph(graph: SourceGraph, mut diagnostics: Vec<Diagnostic>) -> ImportReport {
    let (effective, semantic_diagnostics) = analyze(&graph).into_parts();
    diagnostics.extend(semantic_diagnostics);
    if let Some(pattern) = effective.refresh_policy.patterns.first() {
        diagnostics.push(
            Diagnostic::new(
                E_UNSUPPORTED_FEATURE,
                Severity::Warning,
                DiagnosticStage::Lower,
                "Squid cache freshness rules are externalized because OxiRoute forward proxying is direct and non-caching",
            )
            .with_primary_span(pattern.origin.directive_span),
        );
    }

    let lowered = lower(&effective);
    let lowered_occurrences = consumed_occurrences(&effective);
    let mut grouped = BTreeMap::<SemanticBlockerKind, Vec<OccurrenceId>>::new();
    for decision in &effective.ledger.decisions {
        let DecisionOutcome::Classified {
            activation: Activation::Blocked(kind),
            ..
        } = decision.outcome
        else {
            continue;
        };
        if !lowered_occurrences.contains(&decision.origin.occurrence) {
            grouped
                .entry(kind)
                .or_default()
                .push(decision.origin.occurrence);
        }
    }

    let mut blocked_capabilities: Vec<BlockedCapability> = grouped
        .into_iter()
        .map(|(kind, occurrences)| {
            let diagnostic_code = blocker_code(kind);
            if is_schema_blocker(kind) {
                let first = occurrences
                    .first()
                    .and_then(|occurrence| effective.ledger.decision(*occurrence));
                let mut diagnostic = Diagnostic::new(
                    diagnostic_code,
                    Severity::Error,
                    DiagnosticStage::Lower,
                    blocker_message(kind),
                );
                if let Some(first) = first {
                    diagnostic = diagnostic
                        .with_primary_span(first.origin.directive_span)
                        .with_include_stack(
                            first
                                .origin
                                .provenance
                                .include_stack
                                .iter()
                                .map(|frame| frame.directive_span),
                        );
                }
                diagnostics.push(diagnostic);
            }
            BlockedCapability {
                kind,
                occurrences,
                diagnostic_code,
            }
        })
        .collect();
    if let Err(kind) = &lowered
        && !blocked_capabilities
            .iter()
            .any(|capability| capability.kind == *kind)
    {
        let occurrences = effective
            .ledger
            .decisions
            .iter()
            .filter_map(|decision| match decision.outcome {
                DecisionOutcome::Classified {
                    activation: Activation::Blocked(blocked),
                    ..
                } if blocked == *kind => Some(decision.origin.occurrence),
                DecisionOutcome::Classified { .. } => None,
            })
            .collect::<Vec<_>>();
        diagnostics.push(lower_blocker_diagnostic(*kind, &effective, &graph));
        blocked_capabilities.push(BlockedCapability {
            kind: *kind,
            occurrences,
            diagnostic_code: blocker_code(*kind),
        });
        blocked_capabilities.sort_unstable_by_key(|capability| capability.kind);
    }
    let decision_ledger = effective.ledger.clone();
    let (draft, canonical_provenance) = lowered.map_or_else(
        |_| (empty_config(), Vec::new()),
        |lowered| (lowered.draft, lowered.provenance),
    );
    let eligible = blocked_capabilities.is_empty()
        && !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity() == Severity::Error);
    let state = match finalize_candidate(&draft, eligible) {
        Ok(Some(config)) => CanonicalCandidateState::Validated(config),
        Ok(None) => CanonicalCandidateState::Blocked(draft),
        Err(error) => {
            let mut diagnostic = Diagnostic::new(
                E_SEMANTICS_NOT_REPRESENTABLE,
                Severity::Error,
                DiagnosticStage::Validate,
                format!("lowered Squid candidate failed canonical validation: {error}"),
            );
            if let Some(origin) = canonical_provenance
                .iter()
                .flat_map(|entry| entry.origins.iter())
                .next()
            {
                diagnostic = diagnostic
                    .with_primary_span(origin.directive_span)
                    .with_include_stack(
                        origin
                            .provenance
                            .include_stack
                            .iter()
                            .map(|frame| frame.directive_span),
                    );
            }
            diagnostics.push(diagnostic);
            CanonicalCandidateState::Blocked(draft)
        }
    };
    let ((), diagnostics) = crate::Report::new((), diagnostics).into_parts();
    let source_metadata = SourceImportMetadata {
        original_sources: graph
            .sources
            .iter()
            .map(|source| source.source.clone())
            .collect(),
        ..SourceImportMetadata::default()
    };

    ImportReport {
        source_graph: graph,
        effective,
        decision_ledger,
        capabilities: squid_capability_report(),
        blocked_capabilities,
        candidate: CanonicalCandidate::new(
            state,
            canonical_provenance,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            source_metadata,
        ),
        diagnostics,
    }
}
