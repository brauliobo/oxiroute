use std::{
    collections::HashMap,
    ffi::OsString,
    fs, io,
    path::{Component, Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use crate::{
    Diagnostic, DiagnosticCode, DiagnosticStage, E_INCLUDE_CYCLE, E_INCLUDE_NOT_FOUND,
    E_SOURCE_CHANGED, E_SOURCE_IO, E_SOURCE_LIMIT, E_UNSUPPORTED_FEATURE,
    MAX_AGGREGATE_SOURCE_BYTES, MAX_DIRECTIVES_PER_SOURCE, MAX_EXPANDED_DIRECTIVES,
    MAX_GLOB_MATCHES, MAX_INCLUDE_DEPTH, MAX_SOURCE_BYTES, MAX_SOURCE_FILES, MAX_STRUCTURAL_DEPTH,
    MAX_TOKENS_PER_SOURCE, Report, Severity, SourceFile, SourceId, Span,
    source::{FileFingerprint, StableReadFailure, read_stable_file, stable_file_changed},
};

use super::{Directive, Document, E_SYNTAX, parser::parse_with_limits};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApacheLoadLimits {
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

impl Default for ApacheLoadLimits {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IncludeCandidateStatus {
    Expanded(SourceId),
    Cycle(SourceId),
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
pub struct IncludeEdge {
    pub occurrence: OccurrenceId,
    pub source: SourceId,
    pub span: Span,
    pub pattern: Vec<u8>,
    pub optional: bool,
    pub targets: Vec<SourceId>,
    pub candidates: Vec<IncludeCandidate>,
    pub truncated: bool,
    pub failure: Option<DiagnosticCode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpandedOccurrence {
    pub id: OccurrenceId,
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
        usize::try_from(id.get())
            .ok()
            .and_then(|index| self.sources.get(index))
            .filter(|source| source.source.id() == id)
    }
}

#[must_use]
pub fn load(root: &Path) -> Report<SourceGraph> {
    load_with_limits(root, ApacheLoadLimits::default())
}

#[must_use]
pub fn load_with_limits(root: &Path, limits: ApacheLoadLimits) -> Report<SourceGraph> {
    let canonical_root = match fs::canonicalize(root) {
        Ok(path) => path,
        Err(error) => {
            return Report::new(
                SourceGraph::default(),
                vec![Diagnostic::new(
                    E_SOURCE_IO,
                    Severity::Error,
                    DiagnosticStage::Source,
                    format!("failed to resolve Apache root source: {error}"),
                )],
            );
        }
    };
    let include_base = canonical_root
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let mut loader = Loader::new(
        limits,
        root.to_path_buf(),
        canonical_root.clone(),
        include_base,
    );
    if let Ok(root_id) = loader.ensure_source(&canonical_root, None, &[]) {
        loader.root = Some(root_id);
        let mut active = vec![root_id];
        loader.expanded_directives = loader.expand_source(root_id, &[], &mut active, None);
    }
    loader.final_recheck();
    loader.finish()
}

struct Loader {
    limits: ApacheLoadLimits,
    root: Option<SourceId>,
    root_observation: PathObservation,
    include_base: PathBuf,
    sources: Vec<SourceRecord>,
    source_ids: HashMap<PathBuf, SourceId>,
    includes: Vec<IncludeEdge>,
    expanded_directives: Vec<ExpandedDirective>,
    expanded_occurrences: Vec<ExpandedOccurrence>,
    diagnostics: Vec<Diagnostic>,
    aggregate_source_bytes: usize,
    expanded_count: usize,
    observations: Vec<IncludeObservation>,
}

impl Loader {
    fn new(
        limits: ApacheLoadLimits,
        requested_root: PathBuf,
        canonical_root: PathBuf,
        include_base: PathBuf,
    ) -> Self {
        Self {
            limits,
            root: None,
            root_observation: PathObservation {
                requested: requested_root,
                canonical: canonical_root,
            },
            include_base,
            sources: Vec::new(),
            source_ids: HashMap::new(),
            includes: Vec::new(),
            expanded_directives: Vec::new(),
            expanded_occurrences: Vec::new(),
            diagnostics: Vec::new(),
            aggregate_source_bytes: 0,
            expanded_count: 0,
            observations: Vec::new(),
        }
    }

    fn ensure_source(
        &mut self,
        canonical_path: &Path,
        primary_span: Option<Span>,
        include_stack: &[IncludeFrame],
    ) -> Result<SourceId, SourceLoadFailure> {
        if let Some(id) = self.source_ids.get(canonical_path) {
            return Ok(*id);
        }
        if self.sources.len() >= self.limits.max_source_files {
            self.source_limit(
                format!(
                    "Apache source file count exceeds the maximum of {}",
                    self.limits.max_source_files
                ),
                primary_span,
                include_stack,
            );
            return Err(SourceLoadFailure::SourceFileLimit);
        }
        let snapshot = match read_stable_file(canonical_path, self.limits.max_source_bytes) {
            Ok(snapshot) => snapshot,
            Err(StableReadFailure::TooLarge) => {
                self.source_limit(
                    format!(
                        "Apache source exceeds the maximum size of {} bytes",
                        self.limits.max_source_bytes
                    ),
                    primary_span,
                    include_stack,
                );
                return Err(SourceLoadFailure::SourceSizeLimit);
            }
            Err(StableReadFailure::Changed) => {
                self.source_error(
                    E_SOURCE_CHANGED,
                    "Apache source changed while it was being read",
                    primary_span,
                    include_stack,
                );
                return Err(SourceLoadFailure::SourceChanged);
            }
            Err(StableReadFailure::Io(error)) => {
                self.source_error(
                    E_SOURCE_IO,
                    format!("failed to read Apache source: {error}"),
                    primary_span,
                    include_stack,
                );
                return Err(SourceLoadFailure::SourceIo);
            }
        };
        let bytes = snapshot.bytes;
        let fingerprint = snapshot.fingerprint;
        let Some(aggregate) = self.aggregate_source_bytes.checked_add(bytes.len()) else {
            self.aggregate_limit(primary_span, include_stack);
            return Err(SourceLoadFailure::AggregateSourceLimit);
        };
        if aggregate > self.limits.max_aggregate_source_bytes {
            self.aggregate_limit(primary_span, include_stack);
            return Err(SourceLoadFailure::AggregateSourceLimit);
        }

        let id = SourceId::new(u32::try_from(self.sources.len()).expect("source limit fits u32"));
        let source = SourceFile::from_path(id, canonical_path.to_path_buf(), bytes);
        let (document, parse_diagnostics) = parse_with_limits(
            &source,
            self.limits.max_source_bytes,
            self.limits.max_tokens_per_source,
            self.limits.max_directives_per_source,
            self.limits.max_structural_depth,
        )
        .into_parts();
        let stack = include_stack
            .iter()
            .map(|frame| frame.directive_span)
            .collect::<Vec<_>>();
        self.diagnostics.extend(
            parse_diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.with_include_stack(stack.iter().copied())),
        );
        self.aggregate_source_bytes = aggregate;
        self.source_ids.insert(canonical_path.to_path_buf(), id);
        self.sources.push(SourceRecord {
            parsed: ParsedSource {
                source,
                canonical_path: canonical_path.to_path_buf(),
                document,
            },
            fingerprint,
            first_include_stack: stack,
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
            if self.expanded_count >= self.limits.max_expanded_directives {
                self.source_limit(
                    format!(
                        "Apache expanded directive count exceeds the maximum of {}",
                        self.limits.max_expanded_directives
                    ),
                    Some(directive.span),
                    include_stack,
                );
                break;
            }
            let occurrence = OccurrenceId::new(self.expanded_count);
            self.expanded_count += 1;
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
            if is_include(directive) {
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

    #[allow(clippy::too_many_lines)]
    fn expand_include(
        &mut self,
        occurrence: OccurrenceId,
        source: SourceId,
        directive: &Directive,
        include_stack: &[IncludeFrame],
        active: &mut Vec<SourceId>,
        parent: Option<OccurrenceId>,
    ) -> Vec<ExpandedDirective> {
        let optional = directive
            .name
            .value
            .eq_ignore_ascii_case(b"IncludeOptional");
        let span = directive
            .arguments
            .first()
            .map_or(directive.span, |argument| argument.span);
        let pattern = directive
            .arguments
            .first()
            .map_or_else(Vec::new, |argument| argument.value.clone());
        let edge_index = self.includes.len();
        self.includes.push(IncludeEdge {
            occurrence,
            source,
            span,
            pattern: pattern.clone(),
            optional,
            targets: Vec::new(),
            candidates: Vec::new(),
            truncated: false,
            failure: None,
        });
        if directive.arguments.len() != 1 || directive.children.is_some() {
            self.includes[edge_index].failure = Some(E_SYNTAX);
            self.source_error(
                E_SYNTAX,
                "Apache Include requires exactly one path and no block",
                Some(directive.span),
                include_stack,
            );
            return Vec::new();
        }
        if include_stack.len() >= self.limits.max_include_depth {
            self.includes[edge_index].failure = Some(E_SOURCE_LIMIT);
            self.source_limit(
                format!(
                    "Apache include depth exceeds the maximum of {}",
                    self.limits.max_include_depth
                ),
                Some(span),
                include_stack,
            );
            return Vec::new();
        }
        let resolution = match resolve_pattern(
            &self.include_base,
            &pattern,
            self.limits.max_glob_matches,
            self.limits.max_glob_work,
        ) {
            Ok(resolution) => resolution,
            Err(GlobFailure::Missing) if optional => {
                self.record_observation(
                    occurrence,
                    span,
                    pattern,
                    optional,
                    Vec::new(),
                    Vec::new(),
                );
                return Vec::new();
            }
            Err(GlobFailure::Missing) => {
                self.includes[edge_index].failure = Some(E_INCLUDE_NOT_FOUND);
                self.source_error(
                    E_INCLUDE_NOT_FOUND,
                    "required Apache Include did not match a regular file",
                    Some(span),
                    include_stack,
                );
                self.record_observation(
                    occurrence,
                    span,
                    pattern,
                    optional,
                    Vec::new(),
                    Vec::new(),
                );
                return Vec::new();
            }
            Err(GlobFailure::UnsupportedGlob) => {
                self.includes[edge_index].failure = Some(E_UNSUPPORTED_FEATURE);
                self.source_error(
                    E_UNSUPPORTED_FEATURE,
                    "Apache Include uses unsupported glob grammar",
                    Some(span),
                    include_stack,
                );
                return Vec::new();
            }
            Err(GlobFailure::WorkLimit | GlobFailure::MatchLimit) => {
                self.includes[edge_index].truncated = true;
                self.includes[edge_index].failure = Some(E_SOURCE_LIMIT);
                self.source_limit(
                    "Apache Include expansion exceeded its bounded match/work limit",
                    Some(span),
                    include_stack,
                );
                return Vec::new();
            }
            Err(GlobFailure::Io(error)) => {
                self.includes[edge_index].failure = Some(E_SOURCE_IO);
                self.source_error(
                    E_SOURCE_IO,
                    format!("failed to expand Apache Include: {error}"),
                    Some(span),
                    include_stack,
                );
                return Vec::new();
            }
        };
        if resolution.truncated {
            self.includes[edge_index].truncated = true;
            self.includes[edge_index].failure = Some(E_SOURCE_LIMIT);
            self.source_limit(
                "Apache Include match count exceeds its configured bound",
                Some(span),
                include_stack,
            );
        }

        let observed_paths = resolution.paths.clone();
        let mut canonical_paths = Vec::new();
        let mut expanded = Vec::new();
        for path in resolution.paths {
            let provenance = Provenance {
                source,
                include_stack: include_stack.to_vec(),
            };
            let canonical = match fs::canonicalize(&path) {
                Ok(canonical) => canonical,
                Err(error) => {
                    self.includes[edge_index].failure = Some(E_SOURCE_CHANGED);
                    self.includes[edge_index].candidates.push(IncludeCandidate {
                        path,
                        canonical_path: None,
                        status: IncludeCandidateStatus::CanonicalizeFailed,
                        provenance,
                    });
                    self.source_error(
                        E_SOURCE_CHANGED,
                        format!("Apache Include target changed before it could be read: {error}"),
                        Some(span),
                        include_stack,
                    );
                    continue;
                }
            };
            canonical_paths.push(canonical.clone());
            let target = match self.ensure_source(&canonical, Some(span), include_stack) {
                Ok(target) => target,
                Err(failure) => {
                    self.includes[edge_index].failure = Some(failure.code());
                    self.includes[edge_index].candidates.push(IncludeCandidate {
                        path,
                        canonical_path: Some(canonical),
                        status: failure.status(),
                        provenance,
                    });
                    continue;
                }
            };
            if active.contains(&target) {
                self.includes[edge_index].failure = Some(E_INCLUDE_CYCLE);
                self.includes[edge_index].candidates.push(IncludeCandidate {
                    path,
                    canonical_path: Some(canonical),
                    status: IncludeCandidateStatus::Cycle(target),
                    provenance,
                });
                self.source_error(
                    E_INCLUDE_CYCLE,
                    "Apache Include cycle detected on the active expansion stack",
                    Some(span),
                    include_stack,
                );
                continue;
            }
            self.includes[edge_index].targets.push(target);
            self.includes[edge_index].candidates.push(IncludeCandidate {
                path,
                canonical_path: Some(canonical),
                status: IncludeCandidateStatus::Expanded(target),
                provenance,
            });
            let mut child_stack = include_stack.to_vec();
            child_stack.push(IncludeFrame {
                source,
                directive_span: directive.span,
                target,
            });
            active.push(target);
            expanded.extend(self.expand_source(target, &child_stack, active, parent));
            active.pop();
            if self.expanded_count >= self.limits.max_expanded_directives {
                self.includes[edge_index].failure = Some(E_SOURCE_LIMIT);
            }
        }
        self.record_observation(
            occurrence,
            span,
            pattern,
            optional,
            observed_paths,
            canonical_paths,
        );
        expanded
    }

    fn record_observation(
        &mut self,
        occurrence: OccurrenceId,
        span: Span,
        pattern: Vec<u8>,
        optional: bool,
        paths: Vec<PathBuf>,
        canonical_paths: Vec<PathBuf>,
    ) {
        self.observations.push(IncludeObservation {
            occurrence,
            span,
            pattern,
            optional,
            paths,
            canonical_paths,
        });
    }

    fn final_recheck(&mut self) {
        if path_identity_changed(&self.root_observation) {
            self.diagnostics.push(Diagnostic::new(
                E_SOURCE_CHANGED,
                Severity::Error,
                DiagnosticStage::Source,
                "Apache root path identity changed while loading the graph",
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
                        || canonical_paths.as_ref().ok() != Some(&observation.canonical_paths)
                        || (resolution.paths.is_empty() && !observation.optional)
                }
                Err(GlobFailure::Missing) => !observation.optional,
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
                        "Apache Include matches changed while loading the graph",
                    )
                    .with_primary_span(observation.span),
                );
            }
        }
        for record in &self.sources {
            if stable_file_changed(
                &record.parsed.canonical_path,
                self.limits.max_source_bytes,
                record.parsed.source.bytes(),
                &record.fingerprint,
            ) {
                self.diagnostics.push(
                    Diagnostic::new(
                        E_SOURCE_CHANGED,
                        Severity::Error,
                        DiagnosticStage::Source,
                        "Apache source changed while loading the graph",
                    )
                    .with_primary_span(record.parsed.source.full_span())
                    .with_include_stack(record.first_include_stack.iter().copied()),
                );
            }
        }
    }

    fn source_error(
        &mut self,
        code: DiagnosticCode,
        message: impl Into<String>,
        primary_span: Option<Span>,
        include_stack: &[IncludeFrame],
    ) {
        let mut diagnostic =
            Diagnostic::new(code, Severity::Error, DiagnosticStage::Source, message);
        if let Some(span) = primary_span {
            diagnostic = diagnostic.with_primary_span(span);
        }
        self.diagnostics.push(
            diagnostic.with_include_stack(include_stack.iter().map(|frame| frame.directive_span)),
        );
    }

    fn source_limit(
        &mut self,
        message: impl Into<String>,
        primary_span: Option<Span>,
        include_stack: &[IncludeFrame],
    ) {
        self.source_error(E_SOURCE_LIMIT, message, primary_span, include_stack);
    }

    fn aggregate_limit(&mut self, primary_span: Option<Span>, include_stack: &[IncludeFrame]) {
        self.source_limit(
            format!(
                "Apache aggregate source size exceeds the maximum of {} bytes",
                self.limits.max_aggregate_source_bytes
            ),
            primary_span,
            include_stack,
        );
    }

    fn record(&self, source: SourceId) -> &SourceRecord {
        &self.sources[usize::try_from(source.get()).expect("source identifiers fit usize")]
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

fn is_include(directive: &Directive) -> bool {
    directive.name.value.eq_ignore_ascii_case(b"Include")
        || directive
            .name
            .value
            .eq_ignore_ascii_case(b"IncludeOptional")
}

#[derive(Clone)]
struct SourceRecord {
    parsed: ParsedSource,
    fingerprint: FileFingerprint,
    first_include_stack: Vec<Span>,
}

#[derive(Clone)]
struct IncludeObservation {
    occurrence: OccurrenceId,
    span: Span,
    pattern: Vec<u8>,
    optional: bool,
    paths: Vec<PathBuf>,
    canonical_paths: Vec<PathBuf>,
}

#[derive(Clone)]
struct PathObservation {
    requested: PathBuf,
    canonical: PathBuf,
}

fn path_identity_changed(observation: &PathObservation) -> bool {
    fs::canonicalize(&observation.requested).map_or(true, |path| path != observation.canonical)
}

enum SourceLoadFailure {
    SourceIo,
    SourceChanged,
    SourceSizeLimit,
    SourceFileLimit,
    AggregateSourceLimit,
}

impl SourceLoadFailure {
    const fn code(&self) -> DiagnosticCode {
        match self {
            Self::SourceIo => E_SOURCE_IO,
            Self::SourceChanged => E_SOURCE_CHANGED,
            Self::SourceSizeLimit | Self::SourceFileLimit | Self::AggregateSourceLimit => {
                E_SOURCE_LIMIT
            }
        }
    }

    const fn status(self) -> IncludeCandidateStatus {
        match self {
            Self::SourceIo => IncludeCandidateStatus::SourceIo,
            Self::SourceChanged => IncludeCandidateStatus::SourceChanged,
            Self::SourceSizeLimit => IncludeCandidateStatus::SourceSizeLimit,
            Self::SourceFileLimit => IncludeCandidateStatus::SourceFileLimit,
            Self::AggregateSourceLimit => IncludeCandidateStatus::AggregateSourceLimit,
        }
    }
}

struct Resolution {
    paths: Vec<PathBuf>,
    truncated: bool,
}

enum GlobFailure {
    Missing,
    Io(io::Error),
    UnsupportedGlob,
    WorkLimit,
    MatchLimit,
}

fn resolve_pattern(
    base: &Path,
    pattern: &[u8],
    max_matches: usize,
    max_work: usize,
) -> Result<Resolution, GlobFailure> {
    if pattern.is_empty() || pattern.contains(&0) {
        return Err(GlobFailure::UnsupportedGlob);
    }
    let requested = path_from_bytes(pattern);
    validate_pattern(&requested)?;
    let absolute = requested.is_absolute();
    let mut states = vec![if absolute {
        PathBuf::from(std::path::MAIN_SEPARATOR.to_string())
    } else {
        base.to_path_buf()
    }];
    let mut saw_glob = false;
    for component in requested.components() {
        let component = match component {
            Component::RootDir | Component::CurDir | Component::Prefix(_) => continue,
            Component::ParentDir => {
                for state in &mut states {
                    state.push("..");
                }
                continue;
            }
            Component::Normal(component) => component,
        };
        let component_bytes = os_bytes(component);
        if has_glob_meta(component_bytes) {
            saw_glob = true;
            let mut next = Vec::new();
            for state in states {
                let entries = fs::read_dir(&state).map_err(|error| {
                    if error.kind() == io::ErrorKind::NotFound {
                        GlobFailure::Missing
                    } else {
                        GlobFailure::Io(error)
                    }
                })?;
                let mut names = entries
                    .map(|entry| entry.map_err(GlobFailure::Io))
                    .collect::<Result<Vec<_>, _>>()?;
                names.sort_by_key(|entry| os_bytes(entry.file_name().as_os_str()).to_vec());
                for entry in names {
                    let name = entry.file_name();
                    if !wildcard_match(component_bytes, os_bytes(name.as_os_str())) {
                        continue;
                    }
                    if next.len() >= max_work {
                        return Err(GlobFailure::WorkLimit);
                    }
                    next.push(state.join(name));
                }
            }
            states = next;
        } else {
            for state in &mut states {
                state.push(component);
            }
        }
        if states.is_empty() {
            return Err(GlobFailure::Missing);
        }
    }
    states.sort_by_key(|path| os_bytes(path.as_os_str()).to_vec());
    states.dedup();
    states.retain(|path| fs::metadata(path).is_ok_and(|metadata| metadata.is_file()));
    if states.is_empty() {
        return Err(GlobFailure::Missing);
    }
    if !saw_glob && states.len() != 1 {
        return Err(GlobFailure::Missing);
    }
    let truncated = states.len() > max_matches;
    if truncated && max_matches == 0 {
        return Err(GlobFailure::MatchLimit);
    }
    states.truncate(max_matches);
    Ok(Resolution {
        paths: states,
        truncated,
    })
}

fn validate_pattern(path: &Path) -> Result<(), GlobFailure> {
    for component in path.components() {
        let Component::Normal(component) = component else {
            continue;
        };
        let bytes = os_bytes(component);
        let mut brackets = 0;
        for byte in bytes {
            match byte {
                b'[' => brackets += 1,
                b']' if brackets > 0 => brackets -= 1,
                _ => {}
            }
        }
        if brackets != 0 {
            return Err(GlobFailure::UnsupportedGlob);
        }
    }
    Ok(())
}

fn has_glob_meta(bytes: &[u8]) -> bool {
    bytes.iter().any(|byte| matches!(*byte, b'*' | b'?' | b'['))
}

fn wildcard_match(pattern: &[u8], value: &[u8]) -> bool {
    fn match_from(pattern: &[u8], value: &[u8]) -> bool {
        match pattern.split_first() {
            None => value.is_empty(),
            Some((b'*', rest)) => {
                match_from(rest, value)
                    || value
                        .split_first()
                        .is_some_and(|(_, rest_value)| match_from(pattern, rest_value))
            }
            Some((b'?', rest)) => value
                .split_first()
                .is_some_and(|(_, rest_value)| match_from(rest, rest_value)),
            Some((b'[', rest)) => {
                let Some(end) = rest.iter().position(|byte| *byte == b']') else {
                    return false;
                };
                let matches = rest[..end].contains(&value.first().copied().unwrap_or_default());
                matches
                    && value
                        .split_first()
                        .is_some_and(|(_, rest_value)| match_from(&rest[end + 1..], rest_value))
            }
            Some((byte, rest)) => value
                .split_first()
                .is_some_and(|(value, rest_value)| byte == value && match_from(rest, rest_value)),
        }
    }
    match_from(pattern, value)
}

#[cfg(unix)]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(OsString::from_vec(bytes.to_vec()))
}

#[cfg(not(unix))]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

#[cfg(unix)]
fn os_bytes(value: &std::ffi::OsStr) -> &[u8] {
    value.as_bytes()
}

#[cfg(not(unix))]
fn os_bytes(value: &std::ffi::OsStr) -> &[u8] {
    value.to_str().map_or(&[], str::as_bytes)
}
