//! Ordered `HAProxy` source loading and byte-level structural parsing.

use std::path::Path;

mod lexer;
mod lower;
mod parser;
mod resolver;
mod source_roots;

use crate::{DiagnosticCode, Report};

pub use crate::E_SOURCE_IO;
pub use lexer::{Line, LineEnding, Word, lex};
pub use lower::CanonicalCandidate;
pub use parser::{
    Configuration, Directive, Document, ParsedSource, Section, SectionKind, parse, parse_roots,
    parse_sources,
};
pub use resolver::{
    AclCriterion, AclDefinition, AclReference, BackendReference, BalanceAlgorithm, BindAddress,
    BindTls, BlockingReason, ConditionPolarity, Consumption, Decision, DecisionLedger,
    DecisionOutcome, DefaultsSelection, DefaultsSource, E_CONFLICTING_DIRECTIVE,
    E_DUPLICATE_IDENTITY, E_LOGGING_UNSUPPORTED, E_PROCESS_OWNED, E_STATS_UNSUPPORTED,
    E_UNCONSUMED_DIRECTIVE, E_UNKNOWN_DIRECTIVE, E_UNRESOLVED_REFERENCE, E_UNSUPPORTED_FORM,
    E_UNSUPPORTED_SECTION, EffectiveBackend, EffectiveBind, EffectiveConfiguration,
    EffectiveDefaults, EffectiveFrontend, EffectiveGlobal, EffectiveListen, EffectiveSection,
    EffectiveServer, EffectiveValue, Externalization, ForwardFor, HttpCheck, HttpHeaderValue,
    HttpRequestRule, HttpResponseRule, InheritanceStep, OccurrenceId, OptionState, Provenance,
    ProxyMode, ProxySettings, Redispatch, ReferenceProvenance, ReferenceTarget, SectionId,
    SemanticBlocker, SemanticBlockerKind, ServerAddress, ServerOption, StatusRange, Timeouts,
    TlsAlpn, TlsMinimumVersion, UseBackend,
};
pub use source_roots::{
    HaproxyLoadLimits, LoadedRoots, LoadedSource, RootLoadDecision, RootLoadFailure,
    RootLoadOutcome, load_roots, load_roots_with_limits,
};

/// Syntax errors produced while reading `HAProxy` source.
pub const E_SYNTAX: DiagnosticCode = DiagnosticCode::new("E_SYNTAX");

/// An environment-dependent word requires an explicit expansion stage.
pub const E_ENVIRONMENT_EXPANSION: DiagnosticCode = DiagnosticCode::new("E_ENVIRONMENT_EXPANSION");

/// Conditional directives require an explicit preprocessing stage.
pub const E_CONDITIONAL_PREPROCESSING: DiagnosticCode =
    DiagnosticCode::new("E_CONDITIONAL_PREPROCESSING");

/// Maximum words accepted on one `HAProxy` configuration line, including its keyword.
pub const MAX_WORDS_PER_LINE: usize = 64;

/// Resolves a parsed report while preserving every source, lexical, parse, and preprocessing
/// diagnostic in the semantic report.
#[must_use]
pub fn resolve_parsed(parsed: Report<Configuration>) -> Report<EffectiveConfiguration> {
    resolver::resolve_report(parsed)
}

/// Parses and resolves already snapshotted source occurrences without allowing diagnostics to be
/// detached from semantic resolution.
#[must_use]
pub fn analyze_sources(sources: &[LoadedSource]) -> Report<EffectiveConfiguration> {
    resolve_parsed(parser::parse_sources(sources))
}

/// Loads, parses, and resolves ordered `-f` roots as one diagnostic-carrying operation.
#[must_use]
pub fn analyze_roots<P: AsRef<Path>>(roots: &[P]) -> Report<EffectiveConfiguration> {
    resolve_parsed(parser::parse_roots(roots))
}

/// Lowers a parsed report through semantic resolution and canonical validation without exposing a
/// diagnostic-dropping intermediate path.
#[must_use]
pub fn import_parsed(parsed: Report<Configuration>) -> Report<CanonicalCandidate> {
    let resolved = resolve_parsed(parsed);
    lower::lower(&resolved)
}

/// Imports already snapshotted source occurrences through the complete native pipeline.
#[must_use]
pub fn import_sources(sources: &[LoadedSource]) -> Report<CanonicalCandidate> {
    import_parsed(parser::parse_sources(sources))
}

/// Imports ordered `-f` roots through loading, parsing, resolution, lowering, and validation.
#[must_use]
pub fn import_roots<P: AsRef<Path>>(roots: &[P]) -> Report<CanonicalCandidate> {
    import_parsed(parser::parse_roots(roots))
}
