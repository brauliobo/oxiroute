#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn lower_blocker_diagnostic(
    kind: SemanticBlockerKind,
    effective: &EffectiveConfiguration,
    graph: &SourceGraph,
) -> Diagnostic {
    let mut diagnostic = Diagnostic::new(
        blocker_code(kind),
        Severity::Error,
        DiagnosticStage::Lower,
        blocker_message(kind),
    );
    if let Some(origin) = effective.ledger.decisions.iter().find_map(|decision| {
        matches!(
            decision.outcome,
            DecisionOutcome::Classified {
                activation: Activation::Blocked(blocked),
                ..
            } if blocked == kind
        )
        .then_some(&decision.origin)
    }) {
        diagnostic = diagnostic
            .with_primary_span(origin.directive_span)
            .with_include_stack(
                origin
                    .provenance
                    .include_stack
                    .iter()
                    .map(|frame| frame.directive_span),
            );
    } else if let Some(root) = graph.root.and_then(|source| graph.source(source)) {
        diagnostic = diagnostic.with_primary_span(root.source.full_span());
    }
    diagnostic
}

pub(super) const fn blocker_code(kind: SemanticBlockerKind) -> DiagnosticCode {
    match kind {
        SemanticBlockerKind::InvalidForm => super::super::E_UNSUPPORTED_FORM,
        SemanticBlockerKind::UnknownDirective => super::super::E_UNKNOWN_DIRECTIVE,
        SemanticBlockerKind::ConflictingAclType => E_DUPLICATE_IDENTITY,
        SemanticBlockerKind::UnresolvedAclReference => E_UNRESOLVED_REFERENCE,
        SemanticBlockerKind::IncludeExpansion
        | SemanticBlockerKind::UnsupportedPortOption
        | SemanticBlockerKind::UnsupportedAclType => E_UNSUPPORTED_FEATURE,
        _ => E_SEMANTICS_NOT_REPRESENTABLE,
    }
}

pub(super) const fn is_schema_blocker(kind: SemanticBlockerKind) -> bool {
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

pub(super) const fn blocker_message(kind: SemanticBlockerKind) -> &'static str {
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
            "Squid direct-routing form is outside the exact global direct-fallback subset"
        }
        SemanticBlockerKind::CacheAccessPolicy => {
            "Squid cache access semantics lack a canonical capability"
        }
        SemanticBlockerKind::CachePeerHierarchy => {
            "Squid cache_peer must be an ordered static parent with HTTP port, ICP port 0, and no options"
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
