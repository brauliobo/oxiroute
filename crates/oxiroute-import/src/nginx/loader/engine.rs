#[allow(clippy::wildcard_imports)]
use super::*;

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

pub(super) fn load_inner<F>(
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
