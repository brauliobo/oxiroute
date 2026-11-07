use std::collections::HashMap;

use crate::canonical::dns_name;
use crate::{
    Diagnostic, DiagnosticStage, E_DUPLICATE_IDENTITY, E_UNRESOLVED_REFERENCE,
    E_UNSUPPORTED_FEATURE, Report, Severity,
};

use super::super::{
    E_UNCONSUMED_DIRECTIVE, E_UNKNOWN_DIRECTIVE, E_UNSUPPORTED_FORM, ExpandedDirective,
    OccurrenceId, SourceGraph, Word, bytes,
};
use super::model::{
    AccessAction, AccessListKind, AccessPolicy, AccessRule, AclDefinition, AclMatcher,
    AclReferenceResolution, AclTerm, AclType, Activation, AuthenticationHelper,
    AuthenticationParameter, AuthenticationRealm, AuthenticationScheme, AuthenticationSetting,
    AuthenticationValue, BuiltinAcl, CacheDirective, CachePeer, CachePeerType, Decision,
    DecisionLedger, DecisionOutcome, DirectiveFamily, DirectiveOrigin, DirectiveResolution,
    DirectiveSemantics, DnsNameservers, EffectiveAcl, EffectiveConfiguration, ForwardedForMode,
    LogDestination, LoggingDirective, NativeValue, OpaqueDirective, PeerOption, PortDirective,
    PortKind, PortOption, PrivacyDirective, ProcessDirective, ProxyAuthMatcher, RefreshOption,
    RefreshPattern, SecretFact, SecretKind, SemanticBlockerKind, StorageDirective,
};

#[must_use]
pub fn analyze(graph: &SourceGraph) -> Report<EffectiveConfiguration> {
    Analyzer::new(graph).run()
}

/// Resolves a loaded graph while preserving every source, lexical, and parse diagnostic.
#[must_use]
pub fn analyze_loaded(loaded: Report<SourceGraph>) -> Report<EffectiveConfiguration> {
    let (graph, mut diagnostics) = loaded.into_parts();
    let (effective, semantic_diagnostics) = analyze(&graph).into_parts();
    diagnostics.extend(semantic_diagnostics);
    Report::new(effective, diagnostics)
}

struct Analyzer<'a> {
    graph: &'a SourceGraph,
    effective: EffectiveConfiguration,
    decisions: Vec<Option<Decision>>,
    diagnostics: Vec<Diagnostic>,
}

mod acl;
mod auth;
mod driver;
mod helpers;
mod peers;
mod settings;

use helpers::{
    access_blocker, access_kind, access_semantics, builtin_acl, is_authentication, is_cache_policy,
    is_dns, is_logging, is_process, is_static_parent_peer, is_storage, origin,
    parse_log_destination, parse_peer_option, parse_port_option, parse_refresh_option,
    port_semantics, proxy_auth_matcher, resolution_for,
};
