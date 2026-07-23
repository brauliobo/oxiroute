use std::{collections::BTreeMap, ffi::OsString, path::Path};

use oxiroute_config::Config;

use crate::{
    CanonicalDraft, CanonicalProvenance, Diagnostic, DiagnosticCode, DiagnosticStage,
    E_DUPLICATE_IDENTITY, E_SEMANTICS_NOT_REPRESENTABLE, E_UNRESOLVED_REFERENCE,
    E_UNSUPPORTED_FEATURE, Severity,
};

use super::{
    Activation, DecisionLedger, DecisionOutcome, EffectiveConfiguration, OccurrenceId, Provenance,
    RootSelection, SemanticBlockerKind, SourceGraph, analyze, load, load_selected,
};

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
    pub blocked_capabilities: Vec<BlockedCapability>,
    pub draft: CanonicalDraft,
    pub canonical_provenance: Vec<CanonicalProvenance<Provenance>>,
    pub config: Option<Config>,
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

fn import_graph(graph: SourceGraph, mut diagnostics: Vec<Diagnostic>) -> ImportReport {
    let (effective, semantic_diagnostics) = analyze(&graph).into_parts();
    diagnostics.extend(semantic_diagnostics);

    let mut grouped = BTreeMap::<SemanticBlockerKind, Vec<OccurrenceId>>::new();
    for decision in &effective.ledger.decisions {
        let DecisionOutcome::Classified {
            activation: Activation::Blocked(kind),
            ..
        } = decision.outcome
        else {
            continue;
        };
        grouped
            .entry(kind)
            .or_default()
            .push(decision.origin.occurrence);
    }

    let blocked_capabilities = grouped
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
    let ((), diagnostics) = crate::Report::new((), diagnostics).into_parts();
    let decision_ledger = effective.ledger.clone();

    ImportReport {
        source_graph: graph,
        effective,
        decision_ledger,
        blocked_capabilities,
        draft: CanonicalDraft::default(),
        canonical_provenance: Vec::new(),
        config: None,
        diagnostics,
    }
}

const fn blocker_code(kind: SemanticBlockerKind) -> DiagnosticCode {
    match kind {
        SemanticBlockerKind::InvalidForm => super::E_UNSUPPORTED_FORM,
        SemanticBlockerKind::UnknownDirective => super::E_UNKNOWN_DIRECTIVE,
        SemanticBlockerKind::ConflictingAclType => E_DUPLICATE_IDENTITY,
        SemanticBlockerKind::UnresolvedAclReference => E_UNRESOLVED_REFERENCE,
        SemanticBlockerKind::IncludeExpansion
        | SemanticBlockerKind::UnsupportedPortOption
        | SemanticBlockerKind::UnsupportedAclType => E_UNSUPPORTED_FEATURE,
        _ => E_SEMANTICS_NOT_REPRESENTABLE,
    }
}

const fn is_schema_blocker(kind: SemanticBlockerKind) -> bool {
    matches!(
        kind,
        SemanticBlockerKind::ForwardProxyListener
            | SemanticBlockerKind::SourceAddressAcl
            | SemanticBlockerKind::DestinationPortAcl
            | SemanticBlockerKind::ProxyAuthenticationAcl
            | SemanticBlockerKind::OrderedHttpAccess
            | SemanticBlockerKind::HeaderAccessPolicy
            | SemanticBlockerKind::DirectRoutingPolicy
            | SemanticBlockerKind::CacheAccessPolicy
            | SemanticBlockerKind::CachePeerHierarchy
            | SemanticBlockerKind::RefreshPolicy
            | SemanticBlockerKind::CachePolicy
            | SemanticBlockerKind::StoragePolicy
            | SemanticBlockerKind::ProxyAuthentication
            | SemanticBlockerKind::AccessLoggingPolicy
            | SemanticBlockerKind::LoggingPolicy
            | SemanticBlockerKind::ResolverPolicy
            | SemanticBlockerKind::ForwardedForPolicy
            | SemanticBlockerKind::ViaPolicy
            | SemanticBlockerKind::HeaderPrivacyPolicy
    )
}

const fn blocker_message(kind: SemanticBlockerKind) -> &'static str {
    match kind {
        SemanticBlockerKind::IncludeExpansion => {
            "Squid include expansion did not produce a complete source graph"
        }
        SemanticBlockerKind::ForwardProxyListener => {
            "Squid forward-proxy listener semantics lack a canonical capability"
        }
        SemanticBlockerKind::SourceAddressAcl => {
            "Squid source-address ACL semantics lack a canonical capability"
        }
        SemanticBlockerKind::DestinationPortAcl => {
            "Squid destination-port ACL semantics lack a canonical capability"
        }
        SemanticBlockerKind::ProxyAuthenticationAcl => {
            "Squid proxy-authentication ACL semantics lack a canonical capability"
        }
        SemanticBlockerKind::OrderedHttpAccess => {
            "Squid ordered first-match HTTP access semantics lack a canonical capability"
        }
        SemanticBlockerKind::HeaderAccessPolicy => {
            "Squid header access semantics lack a canonical capability"
        }
        SemanticBlockerKind::DirectRoutingPolicy => {
            "Squid direct-routing semantics lack a canonical capability"
        }
        SemanticBlockerKind::CacheAccessPolicy => {
            "Squid cache access semantics lack a canonical capability"
        }
        SemanticBlockerKind::CachePeerHierarchy => {
            "Squid cache-peer hierarchy semantics lack a canonical capability"
        }
        SemanticBlockerKind::RefreshPolicy => {
            "Squid ordered refresh semantics lack a canonical capability"
        }
        SemanticBlockerKind::CachePolicy => {
            "Squid cache policy semantics lack a canonical capability"
        }
        SemanticBlockerKind::StoragePolicy => "Squid storage semantics lack a canonical capability",
        SemanticBlockerKind::ProxyAuthentication => {
            "Squid proxy authentication semantics lack a canonical capability"
        }
        SemanticBlockerKind::AccessLoggingPolicy => {
            "Squid access logging semantics lack a canonical capability"
        }
        SemanticBlockerKind::LoggingPolicy => "Squid logging semantics lack a canonical capability",
        SemanticBlockerKind::ResolverPolicy => {
            "Squid resolver selection semantics lack a canonical capability"
        }
        SemanticBlockerKind::ForwardedForPolicy => {
            "Squid forwarded-for privacy semantics lack a canonical capability"
        }
        SemanticBlockerKind::ViaPolicy => "Squid Via header semantics lack a canonical capability",
        SemanticBlockerKind::HeaderPrivacyPolicy => {
            "Squid header privacy semantics lack a canonical capability"
        }
        SemanticBlockerKind::UnsupportedPortOption => "Squid port option is not represented",
        SemanticBlockerKind::UnsupportedAclType => "Squid ACL type is not represented",
        SemanticBlockerKind::ConflictingAclType => {
            "same-name Squid ACL declarations use conflicting types"
        }
        SemanticBlockerKind::UnresolvedAclReference => {
            "Squid access rule references an unresolved ACL"
        }
        SemanticBlockerKind::InvalidForm => "Squid directive form is invalid",
        SemanticBlockerKind::UnknownDirective => "Squid directive is unknown",
    }
}
