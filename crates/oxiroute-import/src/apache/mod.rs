//! Bounded Apache httpd parsing, resolution, and static reverse-proxy lowering.

use std::path::Path;

mod lexer;
mod loader;
mod lower;
mod parser;
mod semantic;

use crate::{DiagnosticCode, Report};

pub use lexer::{Line, Word, lex};
pub use loader::{
    ApacheLoadLimits, ExpandedDirective, ExpandedOccurrence, IncludeCandidate,
    IncludeCandidateStatus, IncludeEdge, IncludeFrame, OccurrenceId, ParsedSource, Provenance,
    SourceGraph, load, load_with_limits,
};
pub use lower::{ApacheImportReport, ApacheProvenance, BlockedVirtualHost, CanonicalCandidate};
pub use parser::{Directive, Document, parse};
pub use semantic::{
    ApacheResolution, DirectiveOrigin, EffectiveBalancer, EffectiveListen, EffectiveProxyPass,
    EffectiveTls, EffectiveVirtualHost, OccurrenceDecision, OccurrenceDisposition, ProxyScheme,
    ProxyTarget, resolve,
};

/// Syntax errors produced while reading Apache httpd source.
pub const E_SYNTAX: DiagnosticCode = DiagnosticCode::new("E_APACHE_SYNTAX");

/// A rewrite changes request routing outside the static importer subset.
pub const E_REWRITE_UNSUPPORTED: DiagnosticCode = DiagnosticCode::new("E_APACHE_REWRITE");
pub const E_APACHE_REWRITE_UNSUPPORTED: DiagnosticCode = E_REWRITE_UNSUPPORTED;

/// A `ProxyPass` form depends on a regular expression, interpolation, or runtime value.
pub const E_DYNAMIC_PROXY_PASS: DiagnosticCode = DiagnosticCode::new("E_APACHE_DYNAMIC_PROXY_PASS");
pub const E_APACHE_DYNAMIC_PROXY_PASS: DiagnosticCode = E_DYNAMIC_PROXY_PASS;

/// Apache directory or location merging cannot be represented by one flat route.
pub const E_DIRECTORY_MERGE: DiagnosticCode = DiagnosticCode::new("E_APACHE_DIRECTORY_MERGE");
pub const E_APACHE_DIRECTORY_MERGE: DiagnosticCode = E_DIRECTORY_MERGE;

/// A loaded Apache module is outside the audited importer capability profile.
pub const E_UNSUPPORTED_MODULE: DiagnosticCode = DiagnosticCode::new("E_APACHE_UNSUPPORTED_MODULE");
pub const E_APACHE_UNSUPPORTED_MODULE: DiagnosticCode = E_UNSUPPORTED_MODULE;

/// An Apache directive has no exact canonical lowering in this subset.
pub const E_UNSUPPORTED_DIRECTIVE: DiagnosticCode =
    DiagnosticCode::new("E_APACHE_UNSUPPORTED_DIRECTIVE");
pub const E_APACHE_UNSUPPORTED_DIRECTIVE: DiagnosticCode = E_UNSUPPORTED_DIRECTIVE;

/// Two Apache virtual hosts claim one effective listener/host identity.
pub const E_AMBIGUOUS_VHOST: DiagnosticCode = DiagnosticCode::new("E_APACHE_AMBIGUOUS_VHOST");
pub const E_APACHE_AMBIGUOUS_VHOST: DiagnosticCode = E_AMBIGUOUS_VHOST;

/// Runtime balancer-manager state is not part of a static source snapshot.
pub const E_DYNAMIC_BALANCER_MANAGER: DiagnosticCode =
    DiagnosticCode::new("E_APACHE_DYNAMIC_BALANCER_MANAGER");
pub const E_APACHE_DYNAMIC_BALANCER_MANAGER: DiagnosticCode = E_DYNAMIC_BALANCER_MANAGER;

/// Resolves an already loaded Apache source graph while retaining loader diagnostics.
#[must_use]
pub fn resolve_graph(loaded: Report<SourceGraph>) -> Report<ApacheResolution> {
    resolve(loaded)
}

/// Loads, parses, resolves, and lowers one Apache root.
#[must_use]
pub fn import_root(root: &Path) -> ApacheImportReport {
    import_root_with_limits(root, ApacheLoadLimits::default())
}

/// Imports one Apache root with explicit source and expansion bounds.
#[must_use]
pub fn import_root_with_limits(root: &Path, limits: ApacheLoadLimits) -> ApacheImportReport {
    lower::lower(load_with_limits(root, limits))
}

/// Lowers an already loaded and resolved Apache graph.
#[must_use]
pub fn import_loaded(loaded: Report<SourceGraph>) -> ApacheImportReport {
    lower::lower(loaded)
}
