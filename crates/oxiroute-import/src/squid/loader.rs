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
    MAX_GLOB_MATCHES, MAX_SOURCE_BYTES, MAX_SOURCE_FILES, MAX_TOKENS_PER_SOURCE, Report, Severity,
    SourceFile, SourceId, Span,
    source::{SourceBudget, SourceCatalog, SourceCatalogFailure, SourceIdentity, SourceNaming},
};

use super::{Directive, Document, E_UNSUPPORTED_FORM, Word, parser::parse_with_limits};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SquidLoadLimits {
    pub max_source_bytes: usize,
    pub max_tokens_per_source: usize,
    pub max_directives_per_source: usize,
    pub max_include_depth: usize,
    pub max_source_files: usize,
    pub max_aggregate_source_bytes: usize,
    pub max_glob_matches: usize,
    pub max_glob_work: usize,
    pub max_expanded_directives: usize,
}

impl Default for SquidLoadLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: MAX_SOURCE_BYTES,
            max_tokens_per_source: MAX_TOKENS_PER_SOURCE,
            max_directives_per_source: MAX_DIRECTIVES_PER_SOURCE,
            max_include_depth: 16,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpandedDirective {
    pub occurrence: OccurrenceId,
    pub directive: Directive,
    pub provenance: Provenance,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IncludeTargetStatus {
    Expanded(SourceId),
    Cycle(SourceId),
    SourceIo,
    SourceChanged,
    SourceSizeLimit,
    SourceFileLimit,
    AggregateSourceLimit,
    ExpansionLimit,
    UnsupportedPipe,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IncludeTarget {
    pub requested_path: PathBuf,
    pub canonical_path: Option<PathBuf>,
    pub status: IncludeTargetStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IncludeEdge {
    pub occurrence: OccurrenceId,
    pub source: SourceId,
    pub span: Span,
    pub targets: Vec<IncludeTarget>,
    pub truncated: bool,
    pub failure: Option<DiagnosticCode>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SourceGraph {
    pub root: Option<SourceId>,
    pub sources: Vec<ParsedSource>,
    pub includes: Vec<IncludeEdge>,
    pub expanded_directives: Vec<ExpandedDirective>,
    pub snapshot_stable: bool,
}

impl SourceGraph {
    #[must_use]
    pub fn source(&self, id: SourceId) -> Option<&ParsedSource> {
        usize::try_from(id.get())
            .ok()
            .and_then(|index| self.sources.get(index))
            .filter(|source| source.source.id() == id)
    }
}

#[must_use]
pub fn load(root: &Path) -> Report<SourceGraph> {
    load_with_limits(root, SquidLoadLimits::default())
}

#[must_use]
pub fn load_with_limits(root: &Path, limits: SquidLoadLimits) -> Report<SourceGraph> {
    load_inner(root, limits, || {})
}

fn load_inner<F>(root: &Path, limits: SquidLoadLimits, before_recheck: F) -> Report<SourceGraph>
where
    F: FnOnce(),
{
    let canonical = match fs::canonicalize(root) {
        Ok(path) => path,
        Err(error) => {
            return Report::new(
                SourceGraph::default(),
                vec![Diagnostic::new(
                    E_SOURCE_IO,
                    Severity::Error,
                    DiagnosticStage::Source,
                    format!("failed to resolve Squid root source: {error}"),
                )],
            );
        }
    };
    let mut loader = Loader::new(limits, root.to_path_buf(), canonical.clone());
    if let Ok(root_id) = loader.ensure_source(&canonical, None, &[]) {
        loader.root = Some(root_id);
        let mut active = vec![root_id];
        loader.expand_source(root_id, &[], &mut active);
    }
    before_recheck();
    loader.final_recheck();
    loader.finish()
}

struct Loader {
    limits: SquidLoadLimits,
    root: Option<SourceId>,
    sources: Vec<SourceRecord>,
    source_catalog: SourceCatalog,
    includes: Vec<IncludeEdge>,
    expanded_directives: Vec<ExpandedDirective>,
    diagnostics: Vec<Diagnostic>,
    expanded_count: usize,
    glob_work: usize,
    glob_observations: Vec<GlobObservation>,
    path_observations: Vec<PathObservation>,
    root_observation: PathObservation,
    snapshot_stable: bool,
}

impl Loader {
    fn new(limits: SquidLoadLimits, requested_root: PathBuf, canonical_root: PathBuf) -> Self {
        Self {
            limits,
            root: None,
            sources: Vec::new(),
            source_catalog: SourceCatalog::new(SourceBudget {
                files: limits.max_source_files,
                source_bytes: limits.max_source_bytes,
                aggregate_bytes: limits.max_aggregate_source_bytes,
            }),
            includes: Vec::new(),
            expanded_directives: Vec::new(),
            diagnostics: Vec::new(),
            expanded_count: 0,
            glob_work: 0,
            glob_observations: Vec::new(),
            path_observations: Vec::new(),
            root_observation: PathObservation {
                requested: requested_root,
                canonical: canonical_root,
            },
            snapshot_stable: true,
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
        let stack = include_stack
            .iter()
            .map(|frame| frame.directive_span)
            .collect::<Vec<_>>();
        let source = match self.source_catalog.load(
            canonical_path,
            stack.clone(),
            SourceIdentity::Catalog,
            SourceNaming::Path,
        ) {
            Ok(source) => source,
            Err(SourceCatalogFailure::SourceFileLimit) => {
                self.source_limit(
                    format!(
                        "Squid source file count exceeds the maximum of {}",
                        self.limits.max_source_files
                    ),
                    primary_span,
                    include_stack,
                );
                return Err(SourceLoadFailure::SourceFileLimit);
            }
            Err(SourceCatalogFailure::SourceSizeLimit) => {
                self.source_limit(
                    format!(
                        "Squid source exceeds the maximum size of {} bytes",
                        self.limits.max_source_bytes
                    ),
                    primary_span,
                    include_stack,
                );
                return Err(SourceLoadFailure::SourceSizeLimit);
            }
            Err(SourceCatalogFailure::Changed) => {
                self.source_error(
                    E_SOURCE_CHANGED,
                    "Squid source changed while it was being read",
                    primary_span,
                    include_stack,
                );
                return Err(SourceLoadFailure::SourceChanged);
            }
            Err(SourceCatalogFailure::Io(error)) => {
                self.source_error(
                    E_SOURCE_IO,
                    format!("failed to read Squid source: {error}"),
                    primary_span,
                    include_stack,
                );
                return Err(SourceLoadFailure::SourceIo);
            }
            Err(SourceCatalogFailure::AggregateSourceLimit) => {
                self.aggregate_limit(primary_span, include_stack);
                return Err(SourceLoadFailure::AggregateSourceLimit);
            }
        };
        let id = source.id();
        let (document, parse_diagnostics) = parse_with_limits(
            &source,
            self.limits.max_source_bytes,
            self.limits.max_tokens_per_source,
            self.limits.max_directives_per_source,
        )
        .into_parts();
        self.diagnostics.extend(
            parse_diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.with_include_stack(stack.iter().copied())),
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
    ) {
        let directives = self.record(source).parsed.document.directives.clone();
        for directive in directives {
            if self.expanded_count == self.limits.max_expanded_directives {
                self.source_error(
                    E_SOURCE_LIMIT,
                    format!(
                        "Squid expanded directive count exceeds the maximum of {}",
                        self.limits.max_expanded_directives
                    ),
                    Some(directive.span),
                    include_stack,
                );
                return;
            }
            let occurrence = OccurrenceId::new(self.expanded_count);
            self.expanded_count += 1;
            self.expanded_directives.push(ExpandedDirective {
                occurrence,
                directive: directive.clone(),
                provenance: Provenance {
                    source,
                    include_stack: include_stack.to_vec(),
                },
            });
            if directive.name.value == b"include" {
                self.expand_include(occurrence, source, &directive, include_stack, active);
            }
        }
    }

    fn expand_include(
        &mut self,
        occurrence: OccurrenceId,
        source: SourceId,
        directive: &Directive,
        include_stack: &[IncludeFrame],
        active: &mut Vec<SourceId>,
    ) {
        let edge_index = self.includes.len();
        self.includes.push(IncludeEdge {
            occurrence,
            source,
            span: directive.span,
            targets: Vec::new(),
            truncated: false,
            failure: None,
        });
        let Some(paths) = self.prepare_include(source, directive, include_stack, edge_index) else {
            return;
        };
        for (argument_span, path) in paths {
            let context = IncludeTargetContext {
                source,
                directive,
                include_stack,
                edge_index,
                argument_span,
            };
            self.expand_include_target(&context, active, path);
        }
    }

    fn prepare_include(
        &mut self,
        source: SourceId,
        directive: &Directive,
        include_stack: &[IncludeFrame],
        edge_index: usize,
    ) -> Option<Vec<(Span, PathBuf)>> {
        if directive.arguments.is_empty() {
            self.includes[edge_index].failure = Some(E_UNSUPPORTED_FORM);
            self.source_error(
                E_UNSUPPORTED_FORM,
                "Squid include requires at least one path argument",
                Some(directive.span),
                include_stack,
            );
            return None;
        }
        if include_stack.len() == self.limits.max_include_depth {
            self.includes[edge_index].failure = Some(E_SOURCE_LIMIT);
            self.source_limit(
                format!(
                    "Squid include depth exceeds the maximum of {}",
                    self.limits.max_include_depth
                ),
                Some(directive.span),
                include_stack,
            );
            return None;
        }
        let mut expanded = Vec::new();
        for argument in &directive.arguments {
            let paths =
                self.prepare_include_argument(source, argument, include_stack, edge_index)?;
            if expanded.len().saturating_add(paths.len()) > self.limits.max_glob_matches {
                self.includes[edge_index].truncated = true;
                self.includes[edge_index].failure = Some(E_SOURCE_LIMIT);
                self.source_limit(
                    format!(
                        "Squid include expansion exceeds the match limit of {}",
                        self.limits.max_glob_matches
                    ),
                    Some(argument.span),
                    include_stack,
                );
                return None;
            }
            expanded.extend(paths.into_iter().map(|path| (argument.span, path)));
        }
        Some(expanded)
    }

    fn prepare_include_argument(
        &mut self,
        source: SourceId,
        argument: &Word,
        include_stack: &[IncludeFrame],
        edge_index: usize,
    ) -> Option<Vec<PathBuf>> {
        if argument.value.contains(&0) {
            self.includes[edge_index].failure = Some(E_UNSUPPORTED_FORM);
            self.source_error(
                E_UNSUPPORTED_FORM,
                "Squid include path contains a NUL byte",
                Some(argument.span),
                include_stack,
            );
            return None;
        }
        if argument
            .value
            .first()
            .is_some_and(|byte| matches!(byte, b'!' | b'|'))
        {
            self.includes[edge_index].failure = Some(E_UNSUPPORTED_FEATURE);
            self.includes[edge_index].targets.push(IncludeTarget {
                requested_path: PathBuf::from("<redacted-pipe-include>"),
                canonical_path: None,
                status: IncludeTargetStatus::UnsupportedPipe,
            });
            self.source_error(
                E_UNSUPPORTED_FEATURE,
                "pipe-backed Squid includes are not executed by the importer",
                Some(argument.span),
                include_stack,
            );
            return None;
        }

        let requested = PathBuf::from(OsString::from_vec(argument.value.clone()));
        let pattern = if requested.is_absolute() {
            requested
        } else {
            self.record(source)
                .parsed
                .canonical_path
                .parent()
                .expect("canonical source has a parent")
                .join(requested)
        };
        match self.expand_paths(&pattern) {
            Ok(paths) if !paths.is_empty() => Some(paths),
            Ok(_) => {
                self.includes[edge_index].failure = Some(E_INCLUDE_NOT_FOUND);
                self.source_error(
                    E_INCLUDE_NOT_FOUND,
                    "Squid include did not match a readable regular file",
                    Some(argument.span),
                    include_stack,
                );
                None
            }
            Err(GlobFailure::WorkLimit | GlobFailure::MatchLimit) => {
                self.includes[edge_index].truncated = true;
                self.includes[edge_index].failure = Some(E_SOURCE_LIMIT);
                self.source_limit(
                    format!(
                        "Squid include expansion exceeds the match/work limits of {}/{}",
                        self.limits.max_glob_matches, self.limits.max_glob_work
                    ),
                    Some(argument.span),
                    include_stack,
                );
                None
            }
            Err(GlobFailure::Io(error)) => {
                self.includes[edge_index].failure = Some(E_SOURCE_IO);
                self.source_error(
                    E_SOURCE_IO,
                    format!("failed to expand Squid include: {error}"),
                    Some(argument.span),
                    include_stack,
                );
                None
            }
        }
    }

    fn expand_include_target(
        &mut self,
        context: &IncludeTargetContext<'_>,
        active: &mut Vec<SourceId>,
        path: PathBuf,
    ) {
        let canonical = match fs::canonicalize(&path) {
            Ok(path) => path,
            Err(error) => {
                self.includes[context.edge_index]
                    .targets
                    .push(IncludeTarget {
                        requested_path: path,
                        canonical_path: None,
                        status: IncludeTargetStatus::SourceIo,
                    });
                self.includes[context.edge_index].failure = Some(E_SOURCE_IO);
                self.source_error(
                    E_SOURCE_IO,
                    format!("failed to resolve Squid include target: {error}"),
                    Some(context.argument_span),
                    context.include_stack,
                );
                return;
            }
        };
        self.path_observations.push(PathObservation {
            requested: path.clone(),
            canonical: canonical.clone(),
        });
        let target = match self.ensure_source(
            &canonical,
            Some(context.argument_span),
            context.include_stack,
        ) {
            Ok(target) => target,
            Err(failure) => {
                self.includes[context.edge_index]
                    .targets
                    .push(IncludeTarget {
                        requested_path: path,
                        canonical_path: Some(canonical),
                        status: failure.status(),
                    });
                self.includes[context.edge_index].failure = Some(failure.code());
                return;
            }
        };
        if active.contains(&target) {
            self.includes[context.edge_index]
                .targets
                .push(IncludeTarget {
                    requested_path: path,
                    canonical_path: Some(canonical),
                    status: IncludeTargetStatus::Cycle(target),
                });
            self.includes[context.edge_index].failure = Some(E_INCLUDE_CYCLE);
            self.source_error(
                E_INCLUDE_CYCLE,
                "Squid include cycle detected on the active expansion stack",
                Some(context.argument_span),
                context.include_stack,
            );
            return;
        }
        self.includes[context.edge_index]
            .targets
            .push(IncludeTarget {
                requested_path: path,
                canonical_path: Some(canonical),
                status: IncludeTargetStatus::Expanded(target),
            });
        let mut child_stack = context.include_stack.to_vec();
        child_stack.push(IncludeFrame {
            source: context.source,
            directive_span: context.directive.span,
            target,
        });
        active.push(target);
        self.expand_source(target, &child_stack, active);
        active.pop();
    }

    fn expand_paths(&mut self, pattern: &Path) -> Result<Vec<PathBuf>, GlobFailure> {
        if !path_has_glob(pattern) {
            return match fs::metadata(pattern) {
                Ok(metadata) if metadata.is_file() => Ok(vec![pattern.to_path_buf()]),
                Ok(_) => Ok(Vec::new()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
                Err(error) => Err(GlobFailure::Io(error)),
            };
        }
        let paths = expand_glob(
            pattern,
            &mut self.glob_work,
            self.limits.max_glob_work,
            self.limits.max_glob_matches,
        )?;
        self.glob_observations.push(GlobObservation {
            pattern: pattern.to_path_buf(),
            matches: paths.clone(),
        });
        Ok(paths)
    }

    fn final_recheck(&mut self) {
        if path_identity_changed(&self.root_observation) {
            self.snapshot_stable = false;
            self.diagnostics.push(Diagnostic::new(
                E_SOURCE_CHANGED,
                Severity::Error,
                DiagnosticStage::Source,
                "Squid root path identity changed while the include graph was being loaded",
            ));
        }
        for observation in &self.path_observations {
            if path_identity_changed(observation) {
                self.snapshot_stable = false;
                self.diagnostics.push(Diagnostic::new(
                    E_SOURCE_CHANGED,
                    Severity::Error,
                    DiagnosticStage::Source,
                    "Squid include path identity changed while the include graph was being loaded",
                ));
            }
        }
        for (source, first_include_stack) in self.source_catalog.changed_snapshots() {
            self.snapshot_stable = false;
            self.diagnostics.push(
                Diagnostic::new(
                    E_SOURCE_CHANGED,
                    Severity::Error,
                    DiagnosticStage::Source,
                    "Squid source changed while the include graph was being loaded",
                )
                .with_primary_span(source.full_span())
                .with_include_stack(first_include_stack),
            );
        }
        let mut glob_work = 0;
        for observation in &self.glob_observations {
            let (changed, work_exhausted) = match expand_glob(
                &observation.pattern,
                &mut glob_work,
                self.limits.max_glob_work,
                self.limits.max_glob_matches,
            ) {
                Ok(paths) => (paths != observation.matches, false),
                Err(GlobFailure::WorkLimit) => (true, true),
                Err(GlobFailure::MatchLimit | GlobFailure::Io(_)) => (true, false),
            };
            if changed {
                self.snapshot_stable = false;
                self.diagnostics.push(Diagnostic::new(
                    E_SOURCE_CHANGED,
                    Severity::Error,
                    DiagnosticStage::Source,
                    "Squid include glob result changed while the include graph was being loaded",
                ));
            }
            if work_exhausted {
                break;
            }
        }
    }

    fn finish(self) -> Report<SourceGraph> {
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
                snapshot_stable: self.snapshot_stable,
            },
            self.diagnostics,
        )
    }

    fn record(&self, source: SourceId) -> &SourceRecord {
        &self.sources[usize::try_from(source.get()).expect("source id fits usize")]
    }

    fn source_limit(
        &mut self,
        message: impl Into<String>,
        span: Option<Span>,
        include_stack: &[IncludeFrame],
    ) {
        self.source_error(E_SOURCE_LIMIT, message, span, include_stack);
    }

    fn aggregate_limit(&mut self, span: Option<Span>, include_stack: &[IncludeFrame]) {
        self.source_limit(
            format!(
                "Squid aggregate source size exceeds the maximum of {} bytes",
                self.limits.max_aggregate_source_bytes
            ),
            span,
            include_stack,
        );
    }

    fn source_error(
        &mut self,
        code: DiagnosticCode,
        message: impl Into<String>,
        span: Option<Span>,
        include_stack: &[IncludeFrame],
    ) {
        let mut diagnostic =
            Diagnostic::new(code, Severity::Error, DiagnosticStage::Source, message)
                .with_include_stack(include_stack.iter().map(|frame| frame.directive_span));
        if let Some(span) = span {
            diagnostic = diagnostic.with_primary_span(span);
        }
        self.diagnostics.push(diagnostic);
    }
}

struct SourceRecord {
    parsed: ParsedSource,
}

struct GlobObservation {
    pattern: PathBuf,
    matches: Vec<PathBuf>,
}

struct PathObservation {
    requested: PathBuf,
    canonical: PathBuf,
}

struct IncludeTargetContext<'a> {
    source: SourceId,
    directive: &'a Directive,
    include_stack: &'a [IncludeFrame],
    edge_index: usize,
    argument_span: Span,
}

#[derive(Clone, Copy)]
enum SourceLoadFailure {
    SourceIo,
    SourceChanged,
    SourceSizeLimit,
    SourceFileLimit,
    AggregateSourceLimit,
}

impl SourceLoadFailure {
    const fn status(self) -> IncludeTargetStatus {
        match self {
            Self::SourceIo => IncludeTargetStatus::SourceIo,
            Self::SourceChanged => IncludeTargetStatus::SourceChanged,
            Self::SourceSizeLimit => IncludeTargetStatus::SourceSizeLimit,
            Self::SourceFileLimit => IncludeTargetStatus::SourceFileLimit,
            Self::AggregateSourceLimit => IncludeTargetStatus::AggregateSourceLimit,
        }
    }

    const fn code(self) -> DiagnosticCode {
        match self {
            Self::SourceIo => E_SOURCE_IO,
            Self::SourceChanged => E_SOURCE_CHANGED,
            Self::SourceSizeLimit | Self::SourceFileLimit | Self::AggregateSourceLimit => {
                E_SOURCE_LIMIT
            }
        }
    }
}

enum GlobFailure {
    WorkLimit,
    MatchLimit,
    Io(io::Error),
}

fn path_identity_changed(observation: &PathObservation) -> bool {
    !fs::canonicalize(&observation.requested)
        .is_ok_and(|canonical| canonical == observation.canonical)
}

fn expand_glob(
    pattern: &Path,
    work: &mut usize,
    work_limit: usize,
    match_limit: usize,
) -> Result<Vec<PathBuf>, GlobFailure> {
    let mut paths = vec![PathBuf::from("/")];
    for component in pattern.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                for path in &mut paths {
                    path.pop();
                }
            }
            Component::Normal(segment) if has_glob(segment.as_bytes()) => {
                let mut matches = Vec::new();
                for parent in &paths {
                    for entry in fs::read_dir(parent).map_err(GlobFailure::Io)? {
                        let entry = entry.map_err(GlobFailure::Io)?;
                        if *work >= work_limit {
                            return Err(GlobFailure::WorkLimit);
                        }
                        *work = work.checked_add(1).ok_or(GlobFailure::WorkLimit)?;
                        let name = entry.file_name();
                        if glob_matches(segment.as_bytes(), name.as_bytes()) {
                            if matches.len() >= match_limit {
                                return Err(GlobFailure::MatchLimit);
                            }
                            matches.push(entry.path());
                        }
                    }
                }
                matches.sort_by(|left, right| {
                    left.as_os_str()
                        .as_bytes()
                        .cmp(right.as_os_str().as_bytes())
                });
                paths = matches;
            }
            Component::Normal(segment) => {
                for path in &mut paths {
                    path.push(segment);
                }
            }
            Component::Prefix(_) => unreachable!("Unix paths have no prefix component"),
        }
    }
    paths.retain(|path| fs::metadata(path).is_ok_and(|metadata| metadata.is_file()));
    paths.sort_by(|left, right| {
        left.as_os_str()
            .as_bytes()
            .cmp(right.as_os_str().as_bytes())
    });
    paths.dedup();
    if paths.len() > match_limit {
        return Err(GlobFailure::MatchLimit);
    }
    Ok(paths)
}

fn path_has_glob(path: &Path) -> bool {
    path.components().any(
        |component| matches!(component, Component::Normal(segment) if has_glob(segment.as_bytes())),
    )
}

fn has_glob(value: &[u8]) -> bool {
    value.iter().any(|byte| matches!(byte, b'*' | b'?' | b'['))
}

fn glob_matches(pattern: &[u8], value: &[u8]) -> bool {
    if value.first() == Some(&b'.') && pattern.first() != Some(&b'.') {
        return false;
    }
    glob_matches_inner(pattern, value)
}

fn glob_matches_inner(pattern: &[u8], value: &[u8]) -> bool {
    match pattern {
        [] => value.is_empty(),
        [b'*', rest @ ..] => {
            glob_matches_inner(rest, value)
                || (!value.is_empty() && glob_matches_inner(pattern, &value[1..]))
        }
        [b'?', rest @ ..] => !value.is_empty() && glob_matches_inner(rest, &value[1..]),
        [b'[', rest @ ..] => class_match(rest, value),
        [literal, rest @ ..] => {
            value.first() == Some(literal) && glob_matches_inner(rest, &value[1..])
        }
    }
}

fn class_match(pattern: &[u8], value: &[u8]) -> bool {
    let Some(close) = pattern.iter().position(|byte| *byte == b']') else {
        return value.first() == Some(&b'[') && glob_matches_inner(pattern, &value[1..]);
    };
    let Some(candidate) = value.first() else {
        return false;
    };
    let class = &pattern[..close];
    let negated = class
        .first()
        .is_some_and(|byte| matches!(byte, b'!' | b'^'));
    let class = if negated { &class[1..] } else { class };
    let mut matched = false;
    let mut index = 0;
    while index < class.len() {
        if index + 2 < class.len() && class[index + 1] == b'-' {
            matched |= class[index] <= *candidate && *candidate <= class[index + 2];
            index += 3;
        } else {
            matched |= class[index] == *candidate;
            index += 1;
        }
    }
    matched != negated && glob_matches_inner(&pattern[close + 1..], &value[1..])
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::symlink};

    use tempfile::tempdir;

    use crate::E_SOURCE_CHANGED;

    use super::{SquidLoadLimits, glob_matches, load_inner};

    #[test]
    fn byte_glob_supports_squid_include_shapes() {
        assert!(glob_matches(b"*.conf", b"10-base.conf"));
        assert!(glob_matches(b"[0-9]?.conf", b"10.conf"));
        assert!(!glob_matches(b"*.conf", b".private.conf"));
        assert!(!glob_matches(b"[!0-9]*", b"1.conf"));
    }

    #[test]
    fn final_recheck_detects_root_symlink_retarget() {
        let directory = tempdir().expect("tempdir");
        let original = directory.path().join("original.conf");
        let replacement = directory.path().join("replacement.conf");
        let root = directory.path().join("squid.conf");
        fs::write(&original, b"via off\n").expect("original root");
        fs::write(&replacement, b"via off\n").expect("replacement root");
        symlink(&original, &root).expect("root symlink");

        let report = load_inner(&root, SquidLoadLimits::default(), || {
            fs::remove_file(&root).expect("remove root symlink");
            symlink(&replacement, &root).expect("retarget root symlink");
        });

        assert_changed(
            &report,
            "Squid root path identity changed while the include graph was being loaded",
        );
    }

    #[test]
    fn final_recheck_detects_include_symlink_retarget() {
        let directory = tempdir().expect("tempdir");
        let original = directory.path().join("original.conf");
        let replacement = directory.path().join("replacement.conf");
        let included = directory.path().join("included.conf");
        let root = directory.path().join("squid.conf");
        fs::write(&original, b"via off\n").expect("original include");
        fs::write(&replacement, b"via off\n").expect("replacement include");
        symlink(&original, &included).expect("include symlink");
        fs::write(&root, b"include included.conf\n").expect("root source");

        let report = load_inner(&root, SquidLoadLimits::default(), || {
            fs::remove_file(&included).expect("remove include symlink");
            symlink(&replacement, &included).expect("retarget include symlink");
        });

        assert_changed(
            &report,
            "Squid include path identity changed while the include graph was being loaded",
        );
    }

    #[test]
    fn final_recheck_detects_glob_addition() {
        let directory = tempdir().expect("tempdir");
        let includes = directory.path().join("conf.d");
        let root = directory.path().join("squid.conf");
        fs::create_dir(&includes).expect("include directory");
        fs::write(includes.join("10-base.conf"), b"via off\n").expect("base include");
        fs::write(&root, b"include conf.d/*.conf\n").expect("root source");

        let report = load_inner(&root, SquidLoadLimits::default(), || {
            fs::write(includes.join("20-added.conf"), b"via off\n").expect("added include");
        });

        assert_changed(
            &report,
            "Squid include glob result changed while the include graph was being loaded",
        );
    }

    #[test]
    fn final_recheck_stops_at_configured_work_limit_after_glob_growth() {
        let directory = tempdir().expect("tempdir");
        let include_dir = directory.path().join("conf.d");
        let root = directory.path().join("squid.conf");
        fs::create_dir(&include_dir).expect("include directory");
        fs::write(include_dir.join("10-base.conf"), b"via off\n").expect("base include");
        fs::write(&root, b"include conf.d/*.conf\n").expect("root source");
        let limits = SquidLoadLimits {
            max_glob_work: 1,
            ..SquidLoadLimits::default()
        };

        let report = load_inner(&root, limits, || {
            for ordinal in 0..64 {
                fs::write(
                    include_dir.join(format!("20-added-{ordinal:02}.conf")),
                    b"via off\n",
                )
                .expect("added include");
            }
        });

        assert_single_glob_change(&report);
    }

    #[test]
    fn final_recheck_stops_at_configured_match_limit_after_glob_growth() {
        let directory = tempdir().expect("tempdir");
        let include_dir = directory.path().join("conf.d");
        let root = directory.path().join("squid.conf");
        fs::create_dir(&include_dir).expect("include directory");
        fs::write(include_dir.join("10-base.conf"), b"via off\n").expect("base include");
        fs::write(&root, b"include conf.d/*.conf\n").expect("root source");
        let limits = SquidLoadLimits {
            max_glob_matches: 1,
            ..SquidLoadLimits::default()
        };

        let report = load_inner(&root, limits, || {
            fs::write(include_dir.join("20-added.conf"), b"via off\n").expect("added include");
        });

        assert_single_glob_change(&report);
    }

    #[test]
    fn final_recheck_detects_glob_removal() {
        let directory = tempdir().expect("tempdir");
        let includes = directory.path().join("conf.d");
        let root = directory.path().join("squid.conf");
        fs::create_dir(&includes).expect("include directory");
        fs::write(includes.join("10-base.conf"), b"via off\n").expect("base include");
        let removed = includes.join("20-removed.conf");
        fs::write(&removed, b"via off\n").expect("removable include");
        fs::write(&root, b"include conf.d/*.conf\n").expect("root source");

        let report = load_inner(&root, SquidLoadLimits::default(), || {
            fs::remove_file(&removed).expect("remove include");
        });

        assert_changed(
            &report,
            "Squid include glob result changed while the include graph was being loaded",
        );
    }

    #[test]
    fn final_recheck_accepts_an_unchanged_source_graph() {
        let directory = tempdir().expect("tempdir");
        let includes = directory.path().join("conf.d");
        let root_target = directory.path().join("root-target.conf");
        let root = directory.path().join("squid.conf");
        let include_target = directory.path().join("include-target.conf");
        let include_link = directory.path().join("included.conf");
        fs::create_dir(&includes).expect("include directory");
        fs::write(&include_target, b"via off\n").expect("include target");
        symlink(&include_target, &include_link).expect("include symlink");
        fs::write(includes.join("10-base.conf"), b"via off\n").expect("glob include");
        fs::write(&root_target, b"include included.conf conf.d/*.conf\n").expect("root target");
        symlink(&root_target, &root).expect("root symlink");

        let report = load_inner(&root, SquidLoadLimits::default(), || {});

        assert!(report.value().snapshot_stable);
        assert!(report.diagnostics().is_empty());
    }

    fn assert_changed(report: &crate::Report<super::SourceGraph>, message: &str) {
        assert!(!report.value().snapshot_stable);
        assert!(report.diagnostics().iter().any(|diagnostic| {
            diagnostic.code() == E_SOURCE_CHANGED && diagnostic.message() == message
        }));
    }

    fn assert_single_glob_change(report: &crate::Report<super::SourceGraph>) {
        let message = "Squid include glob result changed while the include graph was being loaded";
        assert_changed(report, message);
        assert_eq!(
            report
                .diagnostics()
                .iter()
                .filter(|diagnostic| diagnostic.message() == message)
                .count(),
            1
        );
    }
}
