use std::{
    ffi::OsString,
    fs, io,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Component, Path, PathBuf},
};

use crate::{
    Diagnostic, DiagnosticCode, DiagnosticStage, E_INCLUDE_CYCLE, E_INCLUDE_NOT_FOUND,
    E_SOURCE_CHANGED, E_SOURCE_IO, E_SOURCE_LIMIT, E_UNSUPPORTED_FEATURE,
    MAX_AGGREGATE_SOURCE_BYTES, MAX_DIRECTIVES_PER_SOURCE, MAX_EXPANDED_DIRECTIVES,
    MAX_GLOB_MATCHES, MAX_INCLUDE_DEPTH, MAX_SOURCE_BYTES, MAX_SOURCE_FILES, MAX_STRUCTURAL_DEPTH,
    MAX_TOKENS_PER_SOURCE, Report, Severity, SourceFile, SourceId, Span,
    source::{SourceBudget, SourceCatalog, SourceCatalogFailure, SourceIdentity, SourceNaming},
};

use super::{Directive, Document, E_SYNTAX, parser::parse_with_limits};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NginxLoadLimits {
    pub max_source_bytes: usize,
    pub max_tokens_per_source: usize,
    pub max_directives_per_source: usize,
    pub max_structural_depth: usize,
    pub max_include_depth: usize,
    pub max_source_files: usize,
    pub max_aggregate_source_bytes: usize,
    pub max_glob_matches: usize,
    pub max_glob_work: usize,
    pub max_expanded_directives: usize,
}

impl Default for NginxLoadLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: MAX_SOURCE_BYTES,
            max_tokens_per_source: MAX_TOKENS_PER_SOURCE,
            max_directives_per_source: MAX_DIRECTIVES_PER_SOURCE,
            max_structural_depth: MAX_STRUCTURAL_DEPTH,
            max_include_depth: MAX_INCLUDE_DEPTH,
            max_source_files: MAX_SOURCE_FILES,
            max_aggregate_source_bytes: MAX_AGGREGATE_SOURCE_BYTES,
            max_glob_matches: MAX_GLOB_MATCHES,
            max_glob_work: 1_000_000,
            max_expanded_directives: MAX_EXPANDED_DIRECTIVES,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedSource {
    pub source: SourceFile,
    pub canonical_path: PathBuf,
    pub document: Document,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IncludeEdge {
    pub occurrence: OccurrenceId,
    pub source: SourceId,
    pub span: Span,
    pub pattern: Vec<u8>,
    pub targets: Vec<SourceId>,
    pub candidates: Vec<IncludeCandidate>,
    pub truncated: bool,
    /// Terminal loader failure for this reachable include occurrence.
    pub failure: Option<DiagnosticCode>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IncludeCandidateStatus {
    Expanded(SourceId),
    Cycle(SourceId),
    ExpansionLimit(SourceId),
    CanonicalizeFailed,
    SourceIo,
    SourceChanged,
    SourceSizeLimit,
    SourceFileLimit,
    AggregateSourceLimit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IncludeCandidate {
    pub path: PathBuf,
    pub canonical_path: Option<PathBuf>,
    pub status: IncludeCandidateStatus,
    pub provenance: Provenance,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IncludeFrame {
    pub source: SourceId,
    pub directive_span: Span,
    pub target: SourceId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Provenance {
    pub source: SourceId,
    pub include_stack: Vec<IncludeFrame>,
}

/// Stable pre-order identity for one directive visited during include expansion.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OccurrenceId(usize);

impl OccurrenceId {
    #[must_use]
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// One directive occurrence in the bounded, pre-order expanded source graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpandedOccurrence {
    pub id: OccurrenceId,
    /// The containing block. Includes do not introduce a semantic scope.
    pub parent: Option<OccurrenceId>,
    pub directive: Directive,
    pub provenance: Provenance,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpandedDirective {
    pub occurrence: OccurrenceId,
    pub directive: Directive,
    pub children: Option<Vec<Self>>,
    pub provenance: Provenance,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SourceGraph {
    pub root: Option<SourceId>,
    pub sources: Vec<ParsedSource>,
    pub includes: Vec<IncludeEdge>,
    pub expanded_directives: Vec<ExpandedDirective>,
    pub expanded_occurrences: Vec<ExpandedOccurrence>,
    pub snapshot_stable: bool,
}

impl SourceGraph {
    #[must_use]
    pub fn source(&self, id: SourceId) -> Option<&ParsedSource> {
        let Ok(index) = usize::try_from(id.get()) else {
            return None;
        };
        self.sources
            .get(index)
            .filter(|source| source.source.id() == id)
    }
}

#[must_use]
pub fn load(root: &Path, root_prefix: &Path) -> Report<SourceGraph> {
    load_with_limits(root, root_prefix, NginxLoadLimits::default())
}

#[must_use]
pub fn load_with_limits(
    root: &Path,
    root_prefix: &Path,
    limits: NginxLoadLimits,
) -> Report<SourceGraph> {
    load_inner(root, root_prefix, limits, || {})
}

fn load_inner<F>(
    root: &Path,
    root_prefix: &Path,
    limits: NginxLoadLimits,
    before_recheck: F,
) -> Report<SourceGraph>
where
    F: FnOnce(),
{
    let prefix = match fs::canonicalize(root_prefix) {
        Ok(prefix) => prefix,
        Err(error) => {
            return Report::new(
                SourceGraph::default(),
                vec![Diagnostic::new(
                    E_SOURCE_IO,
                    Severity::Error,
                    DiagnosticStage::Source,
                    format!("failed to resolve the nginx root prefix: {error}"),
                )],
            );
        }
    };
    let root_path = if root.is_absolute() {
        root.to_path_buf()
    } else {
        prefix.join(root)
    };
    let root_canonical = match fs::canonicalize(&root_path) {
        Ok(path) => path,
        Err(error) => {
            return Report::new(
                SourceGraph::default(),
                vec![Diagnostic::new(
                    E_SOURCE_IO,
                    Severity::Error,
                    DiagnosticStage::Source,
                    format!("failed to resolve the nginx root source: {error}"),
                )],
            );
        }
    };

    let include_base = root_canonical
        .parent()
        .expect("resolved main configuration has a parent")
        .to_path_buf();
    let mut loader = Loader::new(
        include_base,
        limits,
        RootObservation {
            requested_prefix: root_prefix.to_path_buf(),
            canonical_prefix: prefix.clone(),
            requested_path: root_path,
            canonical_path: root_canonical.clone(),
        },
    );
    if let Ok(root_id) = loader.ensure_source(&root_canonical, None, &[]) {
        loader.root = Some(root_id);
        let mut active = vec![root_id];
        loader.expanded_directives = loader.expand_source(root_id, &[], &mut active, None);
    }

    before_recheck();
    loader.final_recheck();
    loader.finish()
}

struct Loader {
    include_base: PathBuf,
    limits: NginxLoadLimits,
    root_observation: RootObservation,
    root: Option<SourceId>,
    sources: Vec<SourceRecord>,
    source_catalog: SourceCatalog,
    includes: Vec<IncludeEdge>,
    observations: Vec<IncludeObservation>,
    expanded_directives: Vec<ExpandedDirective>,
    expanded_occurrences: Vec<ExpandedOccurrence>,
    diagnostics: Vec<Diagnostic>,
    expanded_directive_count: usize,
    expansion_limit_reached: bool,
}

impl Loader {
    fn new(
        include_base: PathBuf,
        limits: NginxLoadLimits,
        root_observation: RootObservation,
    ) -> Self {
        Self {
            include_base,
            limits,
            root_observation,
            root: None,
            sources: Vec::new(),
            source_catalog: SourceCatalog::new(SourceBudget {
                files: limits.max_source_files,
                source_bytes: limits.max_source_bytes,
                aggregate_bytes: limits.max_aggregate_source_bytes,
            }),
            includes: Vec::new(),
            observations: Vec::new(),
            expanded_directives: Vec::new(),
            expanded_occurrences: Vec::new(),
            diagnostics: Vec::new(),
            expanded_directive_count: 0,
            expansion_limit_reached: false,
        }
    }

    fn ensure_source(
        &mut self,
        canonical_path: &Path,
        primary_span: Option<Span>,
        include_stack: &[IncludeFrame],
    ) -> Result<SourceId, SourceLoadFailure> {
        if let Some(id) = self.source_catalog.source_id(canonical_path) {
            return Ok(id);
        }
        let stack_spans = include_stack
            .iter()
            .map(|frame| frame.directive_span)
            .collect::<Vec<_>>();
        let source = match self.source_catalog.load(
            canonical_path,
            stack_spans.clone(),
            SourceIdentity::Catalog,
            SourceNaming::Anonymous,
        ) {
            Ok(source) => source,
            Err(SourceCatalogFailure::SourceFileLimit) => {
                self.push_diagnostic(
                    E_SOURCE_LIMIT,
                    DiagnosticStage::Source,
                    format!(
                        "source file count exceeds the maximum of {}",
                        self.limits.max_source_files
                    ),
                    primary_span,
                    include_stack,
                );
                return Err(SourceLoadFailure::SourceFileLimit);
            }
            Err(SourceCatalogFailure::SourceSizeLimit) => {
                self.push_diagnostic(
                    E_SOURCE_LIMIT,
                    DiagnosticStage::Source,
                    format!(
                        "source exceeds the maximum size of {} bytes",
                        self.limits.max_source_bytes
                    ),
                    primary_span,
                    include_stack,
                );
                return Err(SourceLoadFailure::SourceSizeLimit);
            }
            Err(SourceCatalogFailure::Changed) => {
                self.push_diagnostic(
                    E_SOURCE_CHANGED,
                    DiagnosticStage::Source,
                    "source changed while it was being read".to_owned(),
                    primary_span,
                    include_stack,
                );
                return Err(SourceLoadFailure::SourceChanged);
            }
            Err(SourceCatalogFailure::Io(error)) => {
                self.push_diagnostic(
                    E_SOURCE_IO,
                    DiagnosticStage::Source,
                    format!("failed to read nginx source: {error}"),
                    primary_span,
                    include_stack,
                );
                return Err(SourceLoadFailure::SourceIo);
            }
            Err(SourceCatalogFailure::AggregateSourceLimit) => {
                self.push_aggregate_limit(primary_span, include_stack);
                return Err(SourceLoadFailure::AggregateSourceLimit);
            }
        };
        let id = source.id();
        let (document, parse_diagnostics) = parse_with_limits(
            &source,
            self.limits.max_source_bytes,
            self.limits.max_tokens_per_source,
            self.limits.max_directives_per_source,
            self.limits.max_structural_depth,
        )
        .into_parts();
        self.diagnostics.extend(
            parse_diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.with_include_stack(stack_spans.iter().copied())),
        );
        self.sources.push(SourceRecord {
            parsed: ParsedSource {
                source,
                canonical_path: canonical_path.to_path_buf(),
                document,
            },
        });
        Ok(id)
    }

    fn expand_source(
        &mut self,
        source: SourceId,
        include_stack: &[IncludeFrame],
        active: &mut Vec<SourceId>,
        parent: Option<OccurrenceId>,
    ) -> Vec<ExpandedDirective> {
        let directives = self.record(source).parsed.document.directives.clone();
        self.expand_directives(source, &directives, include_stack, active, parent)
    }

    fn expand_directives(
        &mut self,
        source: SourceId,
        directives: &[Directive],
        include_stack: &[IncludeFrame],
        active: &mut Vec<SourceId>,
        parent: Option<OccurrenceId>,
    ) -> Vec<ExpandedDirective> {
        let mut expanded = Vec::new();
        for directive in directives {
            if self.expansion_limit_reached {
                break;
            }
            if self.expanded_directive_count >= self.limits.max_expanded_directives {
                self.expansion_limit_reached = true;
                self.push_diagnostic(
                    E_SOURCE_LIMIT,
                    DiagnosticStage::Resolve,
                    format!(
                        "expanded directive occurrence count exceeds the maximum of {}",
                        self.limits.max_expanded_directives
                    ),
                    Some(directive.span),
                    include_stack,
                );
                break;
            }
            let occurrence = OccurrenceId::new(self.expanded_directive_count);
            self.expanded_directive_count += 1;
            let provenance = Provenance {
                source,
                include_stack: include_stack.to_vec(),
            };
            self.expanded_occurrences.push(ExpandedOccurrence {
                id: occurrence,
                parent,
                directive: directive.clone(),
                provenance: provenance.clone(),
            });
            if directive.name.value == b"include" {
                expanded.extend(self.expand_include(
                    occurrence,
                    source,
                    directive,
                    include_stack,
                    active,
                    parent,
                ));
                continue;
            }

            let children = directive.children.as_ref().map(|children| {
                self.expand_directives(source, children, include_stack, active, Some(occurrence))
            });
            expanded.push(ExpandedDirective {
                occurrence,
                directive: directive.clone(),
                children,
                provenance,
            });
        }
        expanded
    }

    fn expand_include(
        &mut self,
        occurrence: OccurrenceId,
        source: SourceId,
        directive: &Directive,
        include_stack: &[IncludeFrame],
        active: &mut Vec<SourceId>,
        parent: Option<OccurrenceId>,
    ) -> Vec<ExpandedDirective> {
        let Some((span, edge_index, candidates)) =
            self.include_targets(occurrence, source, directive, include_stack)
        else {
            return Vec::new();
        };

        let mut expanded = Vec::new();
        for candidate in candidates {
            let target = candidate.target;
            if self.expansion_limit_reached {
                self.includes[edge_index].candidates[candidate.index].status =
                    IncludeCandidateStatus::ExpansionLimit(target);
                self.includes[edge_index].failure = Some(E_SOURCE_LIMIT);
                continue;
            }
            if active.contains(&target) {
                self.includes[edge_index].candidates[candidate.index].status =
                    IncludeCandidateStatus::Cycle(target);
                self.includes[edge_index].failure = Some(E_INCLUDE_CYCLE);
                self.push_diagnostic(
                    E_INCLUDE_CYCLE,
                    DiagnosticStage::Resolve,
                    "include cycle detected on the active expansion stack".to_owned(),
                    Some(span),
                    include_stack,
                );
                continue;
            }
            let mut target_stack = include_stack.to_vec();
            target_stack.push(IncludeFrame {
                source,
                directive_span: directive.span,
                target,
            });
            active.push(target);
            expanded.extend(self.expand_source(target, &target_stack, active, parent));
            active.pop();
            self.includes[edge_index].candidates[candidate.index].status =
                if self.expansion_limit_reached {
                    self.includes[edge_index].failure = Some(E_SOURCE_LIMIT);
                    IncludeCandidateStatus::ExpansionLimit(target)
                } else {
                    IncludeCandidateStatus::Expanded(target)
                };
        }
        expanded
    }

    fn include_targets(
        &mut self,
        occurrence: OccurrenceId,
        source: SourceId,
        directive: &Directive,
        include_stack: &[IncludeFrame],
    ) -> Option<(Span, usize, Vec<CandidateWork>)> {
        let Some(argument) = directive.arguments.first() else {
            self.push_diagnostic(
                E_SYNTAX,
                DiagnosticStage::Parse,
                "include directive requires exactly one path".to_owned(),
                Some(directive.span),
                include_stack,
            );
            return None;
        };
        if directive.arguments.len() != 1 || directive.children.is_some() {
            self.push_diagnostic(
                E_SYNTAX,
                DiagnosticStage::Parse,
                "include directive requires exactly one path and a semicolon".to_owned(),
                Some(directive.span),
                include_stack,
            );
            return None;
        }

        let pattern = argument.value.clone();
        let span = argument.span;
        if include_stack.len() >= self.limits.max_include_depth {
            self.includes.push(empty_include_edge(
                occurrence,
                source,
                span,
                pattern,
                E_SOURCE_LIMIT,
            ));
            self.push_diagnostic(
                E_SOURCE_LIMIT,
                DiagnosticStage::Resolve,
                format!(
                    "include depth exceeds the maximum of {}",
                    self.limits.max_include_depth
                ),
                Some(span),
                include_stack,
            );
            return None;
        }

        let (pattern, resolution) =
            self.resolve_include_pattern(occurrence, source, span, pattern, include_stack)?;

        if resolution.truncated {
            self.push_diagnostic(
                E_SOURCE_LIMIT,
                DiagnosticStage::Resolve,
                format!(
                    "glob match count exceeds the maximum of {}",
                    self.limits.max_glob_matches
                ),
                Some(span),
                include_stack,
            );
        }

        let (edge_index, targets) =
            self.load_include_matches(occurrence, source, span, pattern, resolution, include_stack);
        Some((span, edge_index, targets))
    }

    fn resolve_include_pattern(
        &mut self,
        occurrence: OccurrenceId,
        source: SourceId,
        span: Span,
        pattern: Vec<u8>,
        include_stack: &[IncludeFrame],
    ) -> Option<(Vec<u8>, Resolution)> {
        let failure = match resolve_pattern(
            &self.include_base,
            &pattern,
            self.limits.max_glob_matches,
            self.limits.max_glob_work,
        ) {
            Ok(resolution) => return Some((pattern, resolution)),
            Err(failure) => failure,
        };
        let (code, stage, message) = match failure {
            ResolveFailure::Missing => (
                E_INCLUDE_NOT_FOUND,
                DiagnosticStage::Resolve,
                "exact include path was not found".to_owned(),
            ),
            ResolveFailure::Io(error) => (
                E_SOURCE_IO,
                DiagnosticStage::Source,
                format!("failed to expand nginx include: {error}"),
            ),
            ResolveFailure::UnsupportedGlob => (
                E_UNSUPPORTED_FEATURE,
                DiagnosticStage::Resolve,
                "include uses unsupported glob grammar".to_owned(),
            ),
            ResolveFailure::GlobWorkLimit => (
                E_SOURCE_LIMIT,
                DiagnosticStage::Resolve,
                format!(
                    "glob directory work exceeds the maximum of {} entries",
                    self.limits.max_glob_work
                ),
            ),
        };
        self.includes
            .push(empty_include_edge(occurrence, source, span, pattern, code));
        self.push_diagnostic(code, stage, message, Some(span), include_stack);
        None
    }

    fn load_include_matches(
        &mut self,
        occurrence: OccurrenceId,
        source: SourceId,
        span: Span,
        pattern: Vec<u8>,
        resolution: Resolution,
        include_stack: &[IncludeFrame],
    ) -> (usize, Vec<CandidateWork>) {
        let provenance = Provenance {
            source,
            include_stack: include_stack.to_vec(),
        };
        let observed_paths = resolution.paths.clone();
        let mut observed_canonical_paths = Vec::with_capacity(resolution.paths.len());
        let mut candidates = Vec::with_capacity(resolution.paths.len());
        let mut targets = Vec::new();
        let mut work = Vec::new();
        let mut failure = resolution.truncated.then_some(E_SOURCE_LIMIT);
        for path in resolution.paths {
            let canonical_path = match fs::canonicalize(&path) {
                Ok(canonical) => canonical,
                Err(error) => {
                    self.push_diagnostic(
                        E_SOURCE_CHANGED,
                        DiagnosticStage::Source,
                        format!("include match changed before it could be read: {error}"),
                        Some(span),
                        include_stack,
                    );
                    candidates.push(IncludeCandidate {
                        path,
                        canonical_path: None,
                        status: IncludeCandidateStatus::CanonicalizeFailed,
                        provenance: provenance.clone(),
                    });
                    failure = Some(E_SOURCE_CHANGED);
                    continue;
                }
            };
            observed_canonical_paths.push(canonical_path.clone());
            let index = candidates.len();
            match self.ensure_source(&canonical_path, Some(span), include_stack) {
                Ok(target) => {
                    targets.push(target);
                    work.push(CandidateWork { index, target });
                    candidates.push(IncludeCandidate {
                        path,
                        canonical_path: Some(canonical_path),
                        status: IncludeCandidateStatus::Expanded(target),
                        provenance: provenance.clone(),
                    });
                }
                Err(source_failure) => {
                    failure = Some(source_failure.diagnostic_code());
                    candidates.push(IncludeCandidate {
                        path,
                        canonical_path: Some(canonical_path),
                        status: source_failure.candidate_status(),
                        provenance: provenance.clone(),
                    });
                }
            }
        }
        self.observations.push(IncludeObservation {
            occurrence,
            pattern: pattern.clone(),
            span,
            include_stack: include_stack
                .iter()
                .map(|frame| frame.directive_span)
                .collect(),
            paths: observed_paths,
            canonical_paths: observed_canonical_paths,
            truncated: resolution.truncated,
        });

        let edge_index = self.includes.len();
        self.includes.push(IncludeEdge {
            occurrence,
            source,
            span,
            pattern,
            targets,
            candidates,
            truncated: resolution.truncated,
            failure,
        });
        (edge_index, work)
    }

    fn final_recheck(&mut self) {
        if canonical_path_changed(
            &self.root_observation.requested_prefix,
            &self.root_observation.canonical_prefix,
        ) || canonical_path_changed(
            &self.root_observation.requested_path,
            &self.root_observation.canonical_path,
        ) {
            self.diagnostics.push(Diagnostic::new(
                E_SOURCE_CHANGED,
                Severity::Error,
                DiagnosticStage::Source,
                "root source path changed while loading the graph",
            ));
        }

        for observation in self.observations.clone() {
            let changed = match resolve_pattern(
                &self.include_base,
                &observation.pattern,
                self.limits.max_glob_matches,
                self.limits.max_glob_work,
            ) {
                Ok(resolution) => {
                    let canonical_paths = resolution
                        .paths
                        .iter()
                        .map(fs::canonicalize)
                        .collect::<Result<Vec<_>, _>>();
                    resolution.paths != observation.paths
                        || resolution.truncated != observation.truncated
                        || canonical_paths.is_err()
                        || canonical_paths.is_ok_and(|paths| paths != observation.canonical_paths)
                }
                Err(_) => true,
            };
            if changed {
                if let Some(edge) = self
                    .includes
                    .iter_mut()
                    .find(|edge| edge.occurrence == observation.occurrence)
                {
                    edge.failure = Some(E_SOURCE_CHANGED);
                }
                self.diagnostics.push(
                    Diagnostic::new(
                        E_SOURCE_CHANGED,
                        Severity::Error,
                        DiagnosticStage::Source,
                        "include matches changed while loading the graph",
                    )
                    .with_primary_span(observation.span)
                    .with_include_stack(observation.include_stack),
                );
            }
        }

        for (changed_source, first_include_stack) in self.source_catalog.changed_snapshots() {
            let source = changed_source.id();
            for edge in &mut self.includes {
                let mut reaches_changed_source = false;
                for candidate in &mut edge.candidates {
                    if matches!(candidate.status, IncludeCandidateStatus::Expanded(id) if id == source)
                    {
                        candidate.status = IncludeCandidateStatus::SourceChanged;
                        reaches_changed_source = true;
                    }
                }
                if reaches_changed_source {
                    edge.failure = Some(E_SOURCE_CHANGED);
                }
            }
            self.diagnostics.push(
                Diagnostic::new(
                    E_SOURCE_CHANGED,
                    Severity::Error,
                    DiagnosticStage::Source,
                    "source changed while loading the graph",
                )
                .with_primary_span(changed_source.full_span())
                .with_include_stack(first_include_stack),
            );
        }
    }

    fn push_aggregate_limit(&mut self, primary_span: Option<Span>, include_stack: &[IncludeFrame]) {
        self.push_diagnostic(
            E_SOURCE_LIMIT,
            DiagnosticStage::Source,
            format!(
                "aggregate source size exceeds the maximum of {} bytes",
                self.limits.max_aggregate_source_bytes
            ),
            primary_span,
            include_stack,
        );
    }

    fn push_diagnostic(
        &mut self,
        code: DiagnosticCode,
        stage: DiagnosticStage,
        message: String,
        primary_span: Option<Span>,
        include_stack: &[IncludeFrame],
    ) {
        let mut diagnostic = Diagnostic::new(code, Severity::Error, stage, message)
            .with_include_stack(include_stack.iter().map(|frame| frame.directive_span));
        if let Some(span) = primary_span {
            diagnostic = diagnostic.with_primary_span(span);
        }
        self.diagnostics.push(diagnostic);
    }

    fn record(&self, id: SourceId) -> &SourceRecord {
        let index = usize::try_from(id.get()).expect("source identifiers fit usize");
        &self.sources[index]
    }

    fn finish(self) -> Report<SourceGraph> {
        let snapshot_stable = !self
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == E_SOURCE_CHANGED);
        Report::new(
            SourceGraph {
                root: self.root,
                sources: self
                    .sources
                    .into_iter()
                    .map(|record| record.parsed)
                    .collect(),
                includes: self.includes,
                expanded_directives: self.expanded_directives,
                expanded_occurrences: self.expanded_occurrences,
                snapshot_stable,
            },
            self.diagnostics,
        )
    }
}

fn empty_include_edge(
    occurrence: OccurrenceId,
    source: SourceId,
    span: Span,
    pattern: Vec<u8>,
    failure: DiagnosticCode,
) -> IncludeEdge {
    IncludeEdge {
        occurrence,
        source,
        span,
        pattern,
        targets: Vec::new(),
        candidates: Vec::new(),
        truncated: false,
        failure: Some(failure),
    }
}

struct SourceRecord {
    parsed: ParsedSource,
}

struct CandidateWork {
    index: usize,
    target: SourceId,
}

enum SourceLoadFailure {
    SourceIo,
    SourceChanged,
    SourceSizeLimit,
    SourceFileLimit,
    AggregateSourceLimit,
}

impl SourceLoadFailure {
    const fn candidate_status(self) -> IncludeCandidateStatus {
        match self {
            Self::SourceIo => IncludeCandidateStatus::SourceIo,
            Self::SourceChanged => IncludeCandidateStatus::SourceChanged,
            Self::SourceSizeLimit => IncludeCandidateStatus::SourceSizeLimit,
            Self::SourceFileLimit => IncludeCandidateStatus::SourceFileLimit,
            Self::AggregateSourceLimit => IncludeCandidateStatus::AggregateSourceLimit,
        }
    }

    const fn diagnostic_code(&self) -> DiagnosticCode {
        match self {
            Self::SourceIo => E_SOURCE_IO,
            Self::SourceChanged => E_SOURCE_CHANGED,
            Self::SourceSizeLimit | Self::SourceFileLimit | Self::AggregateSourceLimit => {
                E_SOURCE_LIMIT
            }
        }
    }
}

struct RootObservation {
    requested_prefix: PathBuf,
    canonical_prefix: PathBuf,
    requested_path: PathBuf,
    canonical_path: PathBuf,
}

#[derive(Clone)]
struct IncludeObservation {
    occurrence: OccurrenceId,
    pattern: Vec<u8>,
    span: Span,
    include_stack: Vec<Span>,
    paths: Vec<PathBuf>,
    canonical_paths: Vec<PathBuf>,
    truncated: bool,
}

struct Resolution {
    paths: Vec<PathBuf>,
    truncated: bool,
}

enum ResolveFailure {
    Missing,
    Io(io::Error),
    UnsupportedGlob,
    GlobWorkLimit,
}

fn resolve_pattern(
    prefix: &Path,
    pattern: &[u8],
    max_matches: usize,
    max_work: usize,
) -> Result<Resolution, ResolveFailure> {
    validate_glob_pattern(pattern).map_err(|()| ResolveFailure::UnsupportedGlob)?;
    let pattern_path = PathBuf::from(OsString::from_vec(pattern.to_vec()));
    if !has_glob_meta(pattern) {
        let literal_path = PathBuf::from(OsString::from_vec(unescape_glob_bytes(pattern)));
        let path = if pattern_path.is_absolute() {
            literal_path
        } else {
            prefix.join(literal_path)
        };
        return match fs::symlink_metadata(&path) {
            Ok(_) => Ok(Resolution {
                paths: vec![path],
                truncated: false,
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Err(ResolveFailure::Missing),
            Err(error) => Err(ResolveFailure::Io(error)),
        };
    }

    expand_glob(prefix, &pattern_path, max_matches, max_work)
}

fn canonical_path_changed(requested: &Path, expected: &Path) -> bool {
    match fs::canonicalize(requested) {
        Ok(path) => path != expected,
        Err(_) => true,
    }
}

fn expand_glob(
    prefix: &Path,
    pattern: &Path,
    max_matches: usize,
    max_work: usize,
) -> Result<Resolution, ResolveFailure> {
    let mut candidates = if pattern.is_absolute() {
        vec![PathBuf::from("/")]
    } else {
        vec![prefix.to_path_buf()]
    };
    let mut truncated = false;
    let mut work = 0;

    for component in pattern.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                for candidate in &mut candidates {
                    candidate.push("..");
                }
            }
            Component::Normal(component) if has_glob_meta(component.as_bytes()) => {
                let mut next = Vec::new();
                for candidate in candidates {
                    let entries = match fs::read_dir(&candidate) {
                        Ok(entries) => entries,
                        Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                        Err(error) => return Err(ResolveFailure::Io(error)),
                    };
                    for entry in entries {
                        if work == max_work {
                            return Err(ResolveFailure::GlobWorkLimit);
                        }
                        work += 1;
                        let entry = entry.map_err(ResolveFailure::Io)?;
                        if glob_matches(component.as_bytes(), entry.file_name().as_bytes()) {
                            push_bounded_path(&mut next, entry.path(), max_matches, &mut truncated);
                        }
                    }
                }
                candidates = next;
            }
            Component::Normal(component) => {
                for candidate in &mut candidates {
                    candidate.push(OsString::from_vec(unescape_glob_bytes(
                        component.as_bytes(),
                    )));
                }
            }
            Component::Prefix(_) => unreachable!("Unix paths do not have prefixes"),
        }
    }

    let mut paths = Vec::new();
    for candidate in candidates {
        match fs::symlink_metadata(&candidate) {
            Ok(_) => paths.push(candidate),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(ResolveFailure::Io(error)),
        }
    }
    paths.sort_by(|left, right| {
        left.as_os_str()
            .as_bytes()
            .cmp(right.as_os_str().as_bytes())
    });
    Ok(Resolution { paths, truncated })
}

fn validate_glob_pattern(pattern: &[u8]) -> Result<(), ()> {
    let mut index = 0;
    while let Some(byte) = pattern.get(index).copied() {
        match byte {
            b'\\' => {
                if index + 1 == pattern.len() {
                    return Err(());
                }
                index += 2;
            }
            b'[' => {
                index = validate_glob_class(pattern, index)?;
            }
            b']' => return Err(()),
            _ => index += 1,
        }
    }
    Ok(())
}

fn validate_glob_class(pattern: &[u8], opening: usize) -> Result<usize, ()> {
    let mut index = opening + 1;
    if pattern.get(index) == Some(&b'^') {
        return Err(());
    }
    if pattern.get(index) == Some(&b'!') {
        index += 1;
    }
    let content_start = index;
    while let Some(byte) = pattern.get(index).copied() {
        match byte {
            b'[' | b'\\' => return Err(()),
            b']' => {
                if index == content_start {
                    return Err(());
                }
                let content = &pattern[content_start..index];
                let mut content_index = 0;
                while content_index < content.len() {
                    if content_index + 2 < content.len() && content[content_index + 1] == b'-' {
                        if content[content_index] > content[content_index + 2] {
                            return Err(());
                        }
                        content_index += 3;
                    } else {
                        content_index += 1;
                    }
                }
                return Ok(index + 1);
            }
            _ => index += 1,
        }
    }
    Err(())
}

fn push_bounded_path(
    paths: &mut Vec<PathBuf>,
    path: PathBuf,
    max_paths: usize,
    truncated: &mut bool,
) {
    if paths.len() < max_paths {
        paths.push(path);
        return;
    }
    *truncated = true;
    let Some((largest_index, largest)) =
        paths.iter().enumerate().max_by(|(_, left), (_, right)| {
            left.as_os_str()
                .as_bytes()
                .cmp(right.as_os_str().as_bytes())
        })
    else {
        return;
    };
    if path.as_os_str().as_bytes() < largest.as_os_str().as_bytes() {
        paths[largest_index] = path;
    }
}

fn unescape_glob_bytes(pattern: &[u8]) -> Vec<u8> {
    let mut literal = Vec::with_capacity(pattern.len());
    let mut index = 0;
    while let Some(byte) = pattern.get(index).copied() {
        if byte == b'\\' && index + 1 < pattern.len() {
            literal.push(pattern[index + 1]);
            index += 2;
        } else {
            literal.push(byte);
            index += 1;
        }
    }
    literal
}

fn has_glob_meta(pattern: &[u8]) -> bool {
    let mut escaped = false;
    for byte in pattern {
        if escaped {
            escaped = false;
        } else if *byte == b'\\' {
            escaped = true;
        } else if matches!(*byte, b'*' | b'?' | b'[') {
            return true;
        }
    }
    false
}

fn glob_matches(pattern: &[u8], name: &[u8]) -> bool {
    if name.first() == Some(&b'.') && pattern_first_literal(pattern) != Some(b'.') {
        return false;
    }
    let mut memo = vec![vec![None; name.len() + 1]; pattern.len() + 1];
    glob_matches_from(pattern, name, 0, 0, &mut memo)
}

fn glob_matches_from(
    pattern: &[u8],
    name: &[u8],
    pattern_index: usize,
    name_index: usize,
    memo: &mut [Vec<Option<bool>>],
) -> bool {
    if let Some(result) = memo[pattern_index][name_index] {
        return result;
    }
    let result = match pattern.get(pattern_index).copied() {
        None => name_index == name.len(),
        Some(b'*') => {
            glob_matches_from(pattern, name, pattern_index + 1, name_index, memo)
                || (name_index < name.len()
                    && glob_matches_from(pattern, name, pattern_index, name_index + 1, memo))
        }
        Some(b'?') => {
            name_index < name.len()
                && glob_matches_from(pattern, name, pattern_index + 1, name_index + 1, memo)
        }
        Some(b'[') => class_match(&pattern[pattern_index..], name.get(name_index).copied())
            .is_some_and(|(matches, consumed)| {
                matches
                    && glob_matches_from(
                        pattern,
                        name,
                        pattern_index + consumed,
                        name_index + 1,
                        memo,
                    )
            }),
        Some(b'\\') if pattern_index + 1 < pattern.len() => {
            name.get(name_index) == pattern.get(pattern_index + 1)
                && glob_matches_from(pattern, name, pattern_index + 2, name_index + 1, memo)
        }
        Some(byte) => {
            name.get(name_index) == Some(&byte)
                && glob_matches_from(pattern, name, pattern_index + 1, name_index + 1, memo)
        }
    };
    memo[pattern_index][name_index] = Some(result);
    result
}

fn class_match(pattern: &[u8], byte: Option<u8>) -> Option<(bool, usize)> {
    let byte = byte?;
    let closing = pattern.iter().position(|candidate| *candidate == b']')?;
    if closing < 2 {
        return None;
    }
    let mut index = 1;
    let negated = matches!(pattern.get(index), Some(b'!' | b'^'));
    if negated {
        index += 1;
    }
    let mut matched = false;
    while index < closing {
        if index + 2 < closing && pattern[index + 1] == b'-' {
            matched |= pattern[index] <= byte && byte <= pattern[index + 2];
            index += 3;
        } else {
            matched |= pattern[index] == byte;
            index += 1;
        }
    }
    Some((matched != negated, closing + 1))
}

fn pattern_first_literal(pattern: &[u8]) -> Option<u8> {
    match pattern {
        [b'\\', literal, ..] => Some(*literal),
        [literal, ..] if !matches!(*literal, b'*' | b'?' | b'[') => Some(*literal),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use crate::E_SOURCE_CHANGED;

    use super::{NginxLoadLimits, load_inner};

    #[test]
    fn final_recheck_detects_source_changes() {
        let directory = tempdir().expect("tempdir");
        let root = directory.path().join("nginx.conf");
        fs::write(&root, b"before;").expect("write root");

        let report = load_inner(&root, directory.path(), NginxLoadLimits::default(), || {
            fs::write(&root, b"after;").expect("change root");
        });

        assert!(!report.value().snapshot_stable);
        assert!(
            report
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code() == E_SOURCE_CHANGED)
        );
    }

    #[test]
    fn final_recheck_detects_glob_changes() {
        let directory = tempdir().expect("tempdir");
        let root = directory.path().join("nginx.conf");
        fs::write(&root, b"include *.inc;").expect("write root");
        fs::write(directory.path().join("a.inc"), b"a;").expect("write include");

        let report = load_inner(&root, directory.path(), NginxLoadLimits::default(), || {
            fs::write(directory.path().join("b.inc"), b"b;").expect("add include");
        });

        assert!(!report.value().snapshot_stable);
        assert!(
            report
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code() == E_SOURCE_CHANGED)
        );
    }
}
