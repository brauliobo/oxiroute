//! Ordered `HAProxy` source loading and byte-level structural parsing.

use std::path::Path;

mod lexer;
mod lower;
mod parser;
mod preprocess;
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
pub use preprocess::{PreprocessedSources, PreprocessingEnvironment, preprocess_sources};
pub use resolver::{
    AclCriterion, AclDefinition, AclReference, BackendReference, BalanceAlgorithm, BindAddress,
    BindTls, BlockingReason, ConditionPolarity, Consumption, Decision, DecisionLedger,
    DecisionOutcome, DefaultsSelection, DefaultsSource, E_CONFLICTING_DIRECTIVE,
    E_DUPLICATE_IDENTITY, E_LOGGING_UNSUPPORTED, E_PROCESS_OWNED, E_STATS_UNSUPPORTED,
    E_UNCONSUMED_DIRECTIVE, E_UNKNOWN_DIRECTIVE, E_UNRESOLVED_REFERENCE, E_UNSUPPORTED_FORM,
    E_UNSUPPORTED_SECTION, EffectiveBackend, EffectiveBind, EffectiveConfiguration,
    EffectiveDefaults, EffectiveFrontend, EffectiveGlobal, EffectiveListen, EffectiveSection,
    EffectiveServer, EffectiveValue, Externalization, ForwardFor, HttpCheck, HttpHeaderValue,
    HttpRequestCondition, HttpRequestRule, HttpResponseRule, InheritanceStep, OccurrenceId,
    OptionState, Provenance, ProxyMode, ProxySettings, Redispatch, ReferenceProvenance,
    ReferenceTarget, SectionId, SemanticBlocker, SemanticBlockerKind, ServerAddress, ServerOption,
    StatsAdminPolicy, StatsSettings, StatusRange, Timeouts, TlsAlpn, TlsMinimumVersion, UseBackend,
};
pub use source_roots::{
    HaproxyLoadLimits, LoadedRoots, LoadedSource, RootLoadDecision, RootLoadFailure,
    RootLoadOutcome, load_roots, load_roots_with_limits,
};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HaproxyImportOptions {
    pub one_request_per_connection: Vec<HaproxyOneRequestPerConnectionOverlay>,
    pub prometheus_migrations: Vec<HaproxyPrometheusMigrationOverlay>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HaproxyOneRequestPerConnectionOverlay {
    pub backend: String,
}

/// Explicitly accepts migration from one exact `HAProxy` Prometheus service to `OxiRoute`'s different
/// metric families and broader statistics route set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HaproxyPrometheusMigrationOverlay {
    pub section: String,
}

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
    import_parsed_with_options(parsed, &HaproxyImportOptions::default())
}

/// Lowers a parsed report with explicit audited operational overlays.
#[must_use]
pub fn import_parsed_with_options(
    parsed: Report<Configuration>,
    options: &HaproxyImportOptions,
) -> Report<CanonicalCandidate> {
    let resolved = resolve_parsed(parsed);
    lower::lower(&resolved, options)
}

/// Imports already snapshotted source occurrences through the complete native pipeline.
#[must_use]
pub fn import_sources(sources: &[LoadedSource]) -> Report<CanonicalCandidate> {
    import_parsed(parser::parse_sources(sources))
}

/// Imports ordered `-f` roots through loading, parsing, resolution, lowering, and validation.
#[must_use]
pub fn import_roots<P: AsRef<Path>>(roots: &[P]) -> Report<CanonicalCandidate> {
    let (configuration, diagnostics) = parser::parse_roots(roots).into_parts();
    let source_metadata = crate::SourceImportMetadata {
        original_sources: configuration
            .files
            .iter()
            .map(|file| file.source.clone())
            .collect(),
        ..crate::SourceImportMetadata::default()
    };
    let (mut candidate, diagnostics) =
        import_parsed(Report::new(configuration, diagnostics)).into_parts();
    candidate.source_metadata = source_metadata;
    Report::new(candidate, diagnostics)
}

/// Imports ordered roots using only the explicitly supplied, fingerprinted preprocessing inputs.
#[must_use]
pub fn import_roots_with_environment<P: AsRef<Path>>(
    roots: &[P],
    environment: PreprocessingEnvironment,
) -> Report<CanonicalCandidate> {
    import_roots_with_options(roots, environment, &HaproxyImportOptions::default())
}

/// Imports ordered roots with explicit preprocessing inputs and audited operational overlays.
#[must_use]
pub fn import_roots_with_options<P: AsRef<Path>>(
    roots: &[P],
    environment: PreprocessingEnvironment,
    options: &HaproxyImportOptions,
) -> Report<CanonicalCandidate> {
    let (loaded, mut diagnostics) = load_roots(roots).into_parts();
    let (preprocessed, preprocessing_diagnostics) =
        preprocess_sources(&loaded, environment).into_parts();
    diagnostics.extend(preprocessing_diagnostics);

    let mut parsed = if loaded.complete() {
        parser::parse_sources(&preprocessed.sources).into_parts().0
    } else {
        Configuration {
            files: Vec::new(),
            root_decisions: Vec::new(),
        }
    };
    parsed.root_decisions = preprocessed.root_decisions;
    let resolved = resolve_parsed(Report::new(parsed, diagnostics));
    let (mut candidate, import_diagnostics) = lower::lower(&resolved, options).into_parts();
    diagnostics = import_diagnostics;
    candidate.source_metadata = preprocessed.metadata;
    Report::new(candidate, diagnostics)
}
