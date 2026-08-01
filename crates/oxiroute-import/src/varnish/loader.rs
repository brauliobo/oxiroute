use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::{
    ffi::OsString,
    fs, io,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::Component,
};

use crate::{
    Diagnostic, DiagnosticCode, DiagnosticStage, E_SOURCE_CHANGED, E_SOURCE_IO, E_SOURCE_LIMIT,
    MAX_AGGREGATE_SOURCE_BYTES, MAX_DIRECTIVES_PER_SOURCE, MAX_EXPANDED_DIRECTIVES,
    MAX_GLOB_MATCHES, MAX_INCLUDE_DEPTH, MAX_SOURCE_BYTES, MAX_SOURCE_FILES, MAX_STRUCTURAL_DEPTH,
    MAX_TOKENS_PER_SOURCE, Report, Severity, SourceFile, SourceId, Span,
    source::{FileFingerprint, StableReadFailure, read_stable_file, stable_file_changed},
};

use super::{
    Declaration, Document, E_VCL_INCLUDE_CYCLE, E_VCL_INCLUDE_NOT_FOUND, IncludeDeclaration,
    ParserLimits, Provenance, parse_with_limits,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VarnishLoadLimits {
    pub source_bytes: usize,
    pub tokens_per_source: usize,
    pub statements_per_source: usize,
    pub structural_depth: usize,
    pub include_depth: usize,
    pub source_files: usize,
    pub aggregate_source_bytes: usize,
    pub glob_matches: usize,
    pub glob_work: usize,
    pub expanded_declarations: usize,
}

impl Default for VarnishLoadLimits {
    fn default() -> Self {
        Self {
            source_bytes: MAX_SOURCE_BYTES,
            tokens_per_source: MAX_TOKENS_PER_SOURCE,
            statements_per_source: MAX_DIRECTIVES_PER_SOURCE,
            structural_depth: MAX_STRUCTURAL_DEPTH,
            include_depth: MAX_INCLUDE_DEPTH,
            source_files: MAX_SOURCE_FILES,
            aggregate_source_bytes: MAX_AGGREGATE_SOURCE_BYTES,
            glob_matches: MAX_GLOB_MATCHES,
            glob_work: 1_000_000,
            expanded_declarations: MAX_EXPANDED_DIRECTIVES,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedSource {
    pub source: SourceFile,
    pub canonical_path: Option<PathBuf>,
    pub document: Document,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedDeclaration {
    pub declaration: Declaration,
    pub provenance: Provenance,
    pub effective_version: Option<VclVersion>,
    pub include_resolved: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VclVersion {
    V4_0,
    V4_1,
    Other(Vec<u8>),
}

impl VclVersion {
    #[must_use]
    pub fn from_bytes(value: &[u8]) -> Self {
        match value {
            b"4.0" => Self::V4_0,
            b"4.1" => Self::V4_1,
            value => Self::Other(value.to_vec()),
        }
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::V4_0 => b"4.0",
            Self::V4_1 => b"4.1",
            Self::Other(value) => value,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IncludeEdge {
    pub source: SourceId,
    pub span: Span,
    pub glob: bool,
    pub pattern: Vec<u8>,
    pub targets: Vec<IncludeTarget>,
    pub truncated: bool,
    pub failure: Option<DiagnosticCode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IncludeTarget {
    pub requested_path: PathBuf,
    pub canonical_path: Option<PathBuf>,
    pub status: IncludeTargetStatus,
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
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SourceGraph {
    pub root: Option<SourceId>,
    pub sources: Vec<ParsedSource>,
    pub includes: Vec<IncludeEdge>,
    pub expanded: Vec<LoadedDeclaration>,
    pub snapshot_stable: bool,
}

impl SourceGraph {
    #[must_use]
    pub fn source(&self, id: SourceId) -> Option<&ParsedSource> {
        self.sources.iter().find(|source| source.source.id() == id)
    }
}

#[cfg(unix)]
#[must_use]
pub fn load(root: &Path) -> Report<SourceGraph> {
    load_with_limits(root, VarnishLoadLimits::default())
}

#[cfg(unix)]
#[must_use]
pub fn load_with_limits(root: &Path, limits: VarnishLoadLimits) -> Report<SourceGraph> {
    load_inner(root, limits, || {})
}

#[cfg(unix)]
fn load_inner<F>(root: &Path, limits: VarnishLoadLimits, before_recheck: F) -> Report<SourceGraph>
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
                    format!("failed to resolve VCL root source: {error}"),
                )],
            );
        }
    };
    let mut loader = FilesystemLoader::new(limits, root.to_path_buf(), canonical.clone());
    if let Ok(root_id) = loader.ensure_source(&canonical, None, &[]) {
        loader.root = Some(root_id);
        let mut active = vec![root_id];
        loader.expand_source(root_id, &[], &mut active, None);
    }
    before_recheck();
    loader.final_recheck();
    loader.finish()
}

pub(super) fn load_memory(root: &SourceFile, includes: &[SourceFile]) -> Report<SourceGraph> {
    let mut loader = MemoryLoader {
        root,
        includes,
        graph: SourceGraph {
            root: Some(root.id()),
            snapshot_stable: true,
            ..SourceGraph::default()
        },
        diagnostics: Vec::new(),
        active: Vec::new(),
        loaded: HashMap::new(),
        aggregate_bytes: 0,
    };
    loader.expand(root, &[], None);
    Report::new(loader.graph, loader.diagnostics)
}

struct MemoryLoader<'a> {
    root: &'a SourceFile,
    includes: &'a [SourceFile],
    graph: SourceGraph,
    diagnostics: Vec<Diagnostic>,
    active: Vec<SourceId>,
    loaded: HashMap<SourceId, usize>,
    aggregate_bytes: usize,
}

impl MemoryLoader<'_> {
    fn expand(
        &mut self,
        source: &SourceFile,
        include_stack: &[Span],
        inherited: Option<VclVersion>,
    ) {
        if self.active.contains(&source.id()) {
            return;
        }
        if include_stack.len() > MAX_INCLUDE_DEPTH {
            self.memory_limit(
                "VCL include depth limit exceeded",
                source.full_span(),
                include_stack,
            );
            return;
        }
        self.active.push(source.id());
        let document = if let Some(index) = self.loaded.get(&source.id()).copied() {
            self.graph.sources[index].document.clone()
        } else {
            if self.loaded.len() == MAX_SOURCE_FILES {
                self.memory_limit(
                    "VCL source count limit exceeded",
                    source.full_span(),
                    include_stack,
                );
                self.active.pop();
                return;
            }
            if source.len() > MAX_SOURCE_BYTES {
                self.memory_limit(
                    "VCL source byte limit exceeded",
                    source.full_span(),
                    include_stack,
                );
                self.active.pop();
                return;
            }
            if self.aggregate_bytes.saturating_add(source.len()) > MAX_AGGREGATE_SOURCE_BYTES {
                self.memory_limit(
                    "VCL aggregate source byte limit exceeded",
                    source.full_span(),
                    include_stack,
                );
                self.active.pop();
                return;
            }
            let (document, diagnostics) = super::parse(source).into_parts();
            self.diagnostics.extend(
                diagnostics
                    .into_iter()
                    .map(|diagnostic| diagnostic.with_include_stack(include_stack.iter().copied())),
            );
            let index = self.graph.sources.len();
            self.loaded.insert(source.id(), index);
            self.aggregate_bytes += source.len();
            self.graph.sources.push(ParsedSource {
                source: source.clone(),
                canonical_path: None,
                document: document.clone(),
            });
            document
        };
        let mut version = inherited;
        for declaration in document.declarations {
            if self.graph.expanded.len() == MAX_EXPANDED_DIRECTIVES {
                self.memory_limit(
                    "VCL expanded declaration limit exceeded",
                    declaration.span(),
                    include_stack,
                );
                break;
            }
            if let Declaration::Version { value, .. } = &declaration {
                version = Some(VclVersion::from_bytes(&value.bytes));
            }
            let provenance = Provenance {
                span: declaration.span(),
                include_stack: include_stack.to_vec(),
            };
            let Declaration::Include(include) = &declaration else {
                self.graph.expanded.push(LoadedDeclaration {
                    declaration,
                    provenance,
                    effective_version: version.clone(),
                    include_resolved: None,
                });
                continue;
            };
            self.expand_memory_include(include, declaration.clone(), &provenance, version.as_ref());
        }
        self.active.pop();
    }

    fn expand_memory_include(
        &mut self,
        include: &IncludeDeclaration,
        declaration: Declaration,
        provenance: &Provenance,
        version: Option<&VclVersion>,
    ) {
        let mut matches = std::iter::once(self.root)
            .chain(self.includes)
            .filter(|source| {
                if include.glob {
                    glob_matches(&include.path.bytes, source.name().as_bytes())
                } else {
                    source.name().as_bytes() == include.path.bytes
                }
            })
            .cloned()
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| left.name().as_bytes().cmp(right.name().as_bytes()));
        let ambiguous = !include.glob && matches.len() > 1;
        let resolved = if include.glob {
            !matches.is_empty()
        } else {
            matches.len() == 1
        };
        let truncated = matches.len() > MAX_GLOB_MATCHES;
        if truncated {
            matches.truncate(MAX_GLOB_MATCHES);
            self.memory_limit(
                "VCL in-memory glob match limit exceeded",
                include.path.span,
                &provenance.include_stack,
            );
        }
        self.graph.expanded.push(LoadedDeclaration {
            declaration,
            provenance: provenance.clone(),
            effective_version: version.cloned(),
            include_resolved: Some(resolved),
        });
        let edge_index = self.graph.includes.len();
        self.graph.includes.push(IncludeEdge {
            source: provenance.span.source(),
            span: provenance.span,
            glob: include.glob,
            pattern: include.path.bytes.clone(),
            targets: Vec::new(),
            truncated,
            failure: truncated.then_some(E_SOURCE_LIMIT),
        });
        if !resolved {
            self.graph.includes[edge_index].failure = Some(E_VCL_INCLUDE_NOT_FOUND);
            self.diagnostics.push(
                Diagnostic::new(
                    E_VCL_INCLUDE_NOT_FOUND,
                    Severity::Error,
                    DiagnosticStage::Resolve,
                    if ambiguous {
                        "VCL exact include is ambiguous in the in-memory source catalog"
                    } else {
                        "VCL include did not match an in-memory source"
                    },
                )
                .with_primary_span(include.path.span)
                .with_include_stack(provenance.include_stack.iter().copied()),
            );
            return;
        }
        if truncated {
            return;
        }
        for target in matches {
            let cycle = self.active.contains(&target.id());
            self.graph.includes[edge_index].targets.push(IncludeTarget {
                requested_path: PathBuf::from(target.name()),
                canonical_path: None,
                status: if cycle {
                    IncludeTargetStatus::Cycle(target.id())
                } else {
                    IncludeTargetStatus::Expanded(target.id())
                },
            });
            if cycle {
                self.graph.includes[edge_index].failure = Some(E_VCL_INCLUDE_CYCLE);
                self.diagnostics.push(
                    Diagnostic::new(
                        E_VCL_INCLUDE_CYCLE,
                        Severity::Error,
                        DiagnosticStage::Resolve,
                        "VCL include cycle detected",
                    )
                    .with_primary_span(include.span)
                    .with_include_stack(provenance.include_stack.iter().copied()),
                );
            } else {
                let mut stack = provenance.include_stack.clone();
                stack.push(include.span);
                self.expand(&target, &stack, version.cloned());
            }
        }
    }

    fn memory_limit(&mut self, message: &'static str, span: Span, include_stack: &[Span]) {
        self.diagnostics.push(
            Diagnostic::new(
                E_SOURCE_LIMIT,
                Severity::Error,
                DiagnosticStage::Source,
                message,
            )
            .with_primary_span(span)
            .with_include_stack(include_stack.iter().copied()),
        );
    }
}

#[cfg(unix)]
struct FilesystemLoader {
    limits: VarnishLoadLimits,
    root: Option<SourceId>,
    records: Vec<SourceRecord>,
    source_ids: HashMap<PathBuf, SourceId>,
    includes: Vec<IncludeEdge>,
    expanded: Vec<LoadedDeclaration>,
    diagnostics: Vec<Diagnostic>,
    aggregate_bytes: usize,
    glob_work: usize,
    glob_observations: Vec<GlobObservation>,
    path_observations: Vec<PathObservation>,
    root_observation: PathObservation,
    snapshot_stable: bool,
}

#[cfg(unix)]
impl FilesystemLoader {
    fn new(limits: VarnishLoadLimits, requested_root: PathBuf, canonical_root: PathBuf) -> Self {
        Self {
            limits,
            root: None,
            records: Vec::new(),
            source_ids: HashMap::new(),
            includes: Vec::new(),
            expanded: Vec::new(),
            diagnostics: Vec::new(),
            aggregate_bytes: 0,
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
        path: &Path,
        span: Option<Span>,
        include_stack: &[Span],
    ) -> Result<SourceId, SourceLoadFailure> {
        if let Some(id) = self.source_ids.get(path) {
            return Ok(*id);
        }
        if self.records.len() == self.limits.source_files {
            self.error(
                E_SOURCE_LIMIT,
                format!(
                    "VCL source count exceeds the maximum of {}",
                    self.limits.source_files
                ),
                span,
                include_stack,
            );
            return Err(SourceLoadFailure::SourceFileLimit);
        }
        let snapshot = match read_stable_file(path, self.limits.source_bytes) {
            Ok(read) => read,
            Err(StableReadFailure::TooLarge) => {
                self.error(
                    E_SOURCE_LIMIT,
                    "VCL source exceeds its byte limit",
                    span,
                    include_stack,
                );
                return Err(SourceLoadFailure::SourceSizeLimit);
            }
            Err(StableReadFailure::Changed) => {
                self.snapshot_stable = false;
                self.error(
                    E_SOURCE_CHANGED,
                    "VCL source changed while being read",
                    span,
                    include_stack,
                );
                return Err(SourceLoadFailure::SourceChanged);
            }
            Err(StableReadFailure::Io(error)) => {
                self.error(
                    E_SOURCE_IO,
                    format!("failed to read VCL source: {error}"),
                    span,
                    include_stack,
                );
                return Err(SourceLoadFailure::SourceIo);
            }
        };
        let bytes = snapshot.bytes;
        let fingerprint = snapshot.fingerprint;
        if self.aggregate_bytes.saturating_add(bytes.len()) > self.limits.aggregate_source_bytes {
            self.error(
                E_SOURCE_LIMIT,
                "VCL aggregate source byte limit exceeded",
                span,
                include_stack,
            );
            return Err(SourceLoadFailure::AggregateSourceLimit);
        }
        let id = SourceId::new(u32::try_from(self.records.len()).expect("source limit fits u32"));
        let source = SourceFile::from_path(id, path, bytes);
        let (document, diagnostics) = parse_with_limits(
            &source,
            ParserLimits {
                source_bytes: self.limits.source_bytes,
                tokens: self.limits.tokens_per_source,
                statements: self.limits.statements_per_source,
                structural_depth: self.limits.structural_depth,
            },
        )
        .into_parts();
        self.diagnostics.extend(
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.with_include_stack(include_stack.iter().copied())),
        );
        self.aggregate_bytes += source.len();
        self.source_ids.insert(path.to_path_buf(), id);
        self.records.push(SourceRecord {
            parsed: ParsedSource {
                source,
                canonical_path: Some(path.to_path_buf()),
                document,
            },
            fingerprint,
            first_include_stack: include_stack.to_vec(),
        });
        Ok(id)
    }

    fn expand_source(
        &mut self,
        source: SourceId,
        include_stack: &[Span],
        active: &mut Vec<SourceId>,
        inherited: Option<VclVersion>,
    ) {
        let declarations = self.record(source).parsed.document.declarations.clone();
        let mut version = inherited;
        for declaration in declarations {
            if self.expanded.len() == self.limits.expanded_declarations {
                self.error(
                    E_SOURCE_LIMIT,
                    "VCL expanded declaration limit exceeded",
                    Some(declaration.span()),
                    include_stack,
                );
                return;
            }
            if let Declaration::Version { value, .. } = &declaration {
                version = Some(VclVersion::from_bytes(&value.bytes));
            }
            let provenance = Provenance {
                span: declaration.span(),
                include_stack: include_stack.to_vec(),
            };
            let Declaration::Include(include) = &declaration else {
                self.expanded.push(LoadedDeclaration {
                    declaration,
                    provenance,
                    effective_version: version.clone(),
                    include_resolved: None,
                });
                continue;
            };
            self.expand_include(
                source,
                include,
                declaration.clone(),
                provenance,
                active,
                version.clone(),
            );
        }
    }

    fn expand_include(
        &mut self,
        source: SourceId,
        include: &IncludeDeclaration,
        declaration: Declaration,
        provenance: Provenance,
        active: &mut Vec<SourceId>,
        version: Option<VclVersion>,
    ) {
        let edge_index = self.includes.len();
        self.includes.push(IncludeEdge {
            source,
            span: include.span,
            glob: include.glob,
            pattern: include.path.bytes.clone(),
            targets: Vec::new(),
            truncated: false,
            failure: None,
        });
        if provenance.include_stack.len() == self.limits.include_depth {
            self.includes[edge_index].failure = Some(E_SOURCE_LIMIT);
            self.error(
                E_SOURCE_LIMIT,
                "VCL include depth limit exceeded",
                Some(include.span),
                &provenance.include_stack,
            );
            self.push_include_declaration(declaration, provenance, version, false);
            return;
        }
        let pattern = self.include_pattern(source, &include.path.bytes);
        let paths = match self.expand_paths(&pattern, include.glob) {
            Ok(paths) => paths,
            Err(GlobFailure::Limit) => {
                self.includes[edge_index].truncated = true;
                self.includes[edge_index].failure = Some(E_SOURCE_LIMIT);
                self.error(
                    E_SOURCE_LIMIT,
                    "VCL +glob match/work limit exceeded",
                    Some(include.path.span),
                    &provenance.include_stack,
                );
                self.push_include_declaration(declaration, provenance, version, false);
                return;
            }
            Err(GlobFailure::Io(error)) => {
                self.includes[edge_index].failure = Some(E_SOURCE_IO);
                self.error(
                    E_SOURCE_IO,
                    format!("failed to expand VCL include: {error}"),
                    Some(include.path.span),
                    &provenance.include_stack,
                );
                self.push_include_declaration(declaration, provenance, version, false);
                return;
            }
        };
        let resolved = !paths.is_empty();
        self.push_include_declaration(declaration, provenance.clone(), version.clone(), resolved);
        if !resolved {
            self.includes[edge_index].failure = Some(E_VCL_INCLUDE_NOT_FOUND);
            self.error(
                E_VCL_INCLUDE_NOT_FOUND,
                "VCL include matched no regular file",
                Some(include.path.span),
                &provenance.include_stack,
            );
            return;
        }
        for path in paths {
            self.expand_include_target(
                edge_index,
                include,
                path,
                &provenance,
                active,
                version.clone(),
            );
        }
    }

    fn push_include_declaration(
        &mut self,
        declaration: Declaration,
        provenance: Provenance,
        version: Option<VclVersion>,
        resolved: bool,
    ) {
        self.expanded.push(LoadedDeclaration {
            declaration,
            provenance,
            effective_version: version,
            include_resolved: Some(resolved),
        });
    }

    fn expand_include_target(
        &mut self,
        edge_index: usize,
        include: &IncludeDeclaration,
        requested: PathBuf,
        provenance: &Provenance,
        active: &mut Vec<SourceId>,
        version: Option<VclVersion>,
    ) {
        let canonical = match fs::canonicalize(&requested) {
            Ok(path) => path,
            Err(error) => {
                self.includes[edge_index].targets.push(IncludeTarget {
                    requested_path: requested,
                    canonical_path: None,
                    status: IncludeTargetStatus::SourceIo,
                });
                self.includes[edge_index].failure = Some(E_SOURCE_IO);
                self.error(
                    E_SOURCE_IO,
                    format!("failed to resolve VCL include: {error}"),
                    Some(include.path.span),
                    &provenance.include_stack,
                );
                return;
            }
        };
        self.path_observations.push(PathObservation {
            requested: requested.clone(),
            canonical: canonical.clone(),
        });
        let target = match self.ensure_source(
            &canonical,
            Some(include.path.span),
            &provenance.include_stack,
        ) {
            Ok(target) => target,
            Err(failure) => {
                self.includes[edge_index].targets.push(IncludeTarget {
                    requested_path: requested,
                    canonical_path: Some(canonical),
                    status: failure.status(),
                });
                self.includes[edge_index].failure = Some(failure.code());
                return;
            }
        };
        if active.contains(&target) {
            self.includes[edge_index].targets.push(IncludeTarget {
                requested_path: requested,
                canonical_path: Some(canonical),
                status: IncludeTargetStatus::Cycle(target),
            });
            self.includes[edge_index].failure = Some(E_VCL_INCLUDE_CYCLE);
            self.error(
                E_VCL_INCLUDE_CYCLE,
                "VCL include cycle detected",
                Some(include.span),
                &provenance.include_stack,
            );
            return;
        }
        self.includes[edge_index].targets.push(IncludeTarget {
            requested_path: requested,
            canonical_path: Some(canonical),
            status: IncludeTargetStatus::Expanded(target),
        });
        let mut stack = provenance.include_stack.clone();
        stack.push(include.span);
        active.push(target);
        self.expand_source(target, &stack, active, version);
        active.pop();
    }

    fn include_pattern(&self, source: SourceId, value: &[u8]) -> PathBuf {
        let requested = PathBuf::from(OsString::from_vec(value.to_vec()));
        if requested.is_absolute() {
            requested
        } else {
            self.record(source)
                .parsed
                .canonical_path
                .as_deref()
                .and_then(Path::parent)
                .expect("filesystem source has a parent")
                .join(requested)
        }
    }

    fn expand_paths(&mut self, pattern: &Path, glob: bool) -> Result<Vec<PathBuf>, GlobFailure> {
        if !glob {
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
            self.limits.glob_work,
            self.limits.glob_matches,
        )?;
        self.glob_observations.push(GlobObservation {
            pattern: pattern.to_path_buf(),
            matches: paths.clone(),
        });
        Ok(paths)
    }

    fn final_recheck(&mut self) {
        if !fs::canonicalize(&self.root_observation.requested)
            .is_ok_and(|path| path == self.root_observation.canonical)
        {
            self.snapshot_stable = false;
            self.diagnostics.push(Diagnostic::new(
                E_SOURCE_CHANGED,
                Severity::Error,
                DiagnosticStage::Source,
                "VCL root path identity changed while its graph was loaded",
            ));
        }
        for observation in &self.path_observations {
            if !fs::canonicalize(&observation.requested)
                .is_ok_and(|path| path == observation.canonical)
            {
                self.snapshot_stable = false;
                self.diagnostics.push(Diagnostic::new(
                    E_SOURCE_CHANGED,
                    Severity::Error,
                    DiagnosticStage::Source,
                    "VCL include path identity changed while its graph was loaded",
                ));
            }
        }
        for record in &self.records {
            let path = record
                .parsed
                .canonical_path
                .as_deref()
                .expect("filesystem path");
            let changed = stable_file_changed(
                path,
                self.limits.source_bytes,
                record.parsed.source.bytes(),
                &record.fingerprint,
            );
            if changed {
                self.snapshot_stable = false;
                self.diagnostics.push(
                    Diagnostic::new(
                        E_SOURCE_CHANGED,
                        Severity::Error,
                        DiagnosticStage::Source,
                        "VCL source changed while its graph was loaded",
                    )
                    .with_primary_span(record.parsed.source.full_span())
                    .with_include_stack(record.first_include_stack.iter().copied()),
                );
            }
        }
        for observation in &self.glob_observations {
            let mut work = 0;
            let changed = expand_glob(&observation.pattern, &mut work, usize::MAX, usize::MAX)
                .map_or(true, |paths| paths != observation.matches);
            if changed {
                self.snapshot_stable = false;
                self.diagnostics.push(Diagnostic::new(
                    E_SOURCE_CHANGED,
                    Severity::Error,
                    DiagnosticStage::Source,
                    "VCL +glob result changed while its graph was loaded",
                ));
            }
        }
    }

    fn finish(self) -> Report<SourceGraph> {
        Report::new(
            SourceGraph {
                root: self.root,
                sources: self
                    .records
                    .into_iter()
                    .map(|record| record.parsed)
                    .collect(),
                includes: self.includes,
                expanded: self.expanded,
                snapshot_stable: self.snapshot_stable,
            },
            self.diagnostics,
        )
    }

    fn record(&self, source: SourceId) -> &SourceRecord {
        &self.records[usize::try_from(source.get()).expect("source id fits usize")]
    }

    fn error(
        &mut self,
        code: DiagnosticCode,
        message: impl Into<String>,
        span: Option<Span>,
        include_stack: &[Span],
    ) {
        let mut diagnostic =
            Diagnostic::new(code, Severity::Error, DiagnosticStage::Source, message)
                .with_include_stack(include_stack.iter().copied());
        if let Some(span) = span {
            diagnostic = diagnostic.with_primary_span(span);
        }
        self.diagnostics.push(diagnostic);
    }
}

#[cfg(unix)]
struct SourceRecord {
    parsed: ParsedSource,
    fingerprint: FileFingerprint,
    first_include_stack: Vec<Span>,
}

#[cfg(unix)]
struct GlobObservation {
    pattern: PathBuf,
    matches: Vec<PathBuf>,
}

#[cfg(unix)]
struct PathObservation {
    requested: PathBuf,
    canonical: PathBuf,
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum SourceLoadFailure {
    SourceIo,
    SourceChanged,
    SourceSizeLimit,
    SourceFileLimit,
    AggregateSourceLimit,
}

#[cfg(unix)]
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

#[cfg(unix)]
enum GlobFailure {
    Limit,
    Io(io::Error),
}

#[cfg(unix)]
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
            Component::ParentDir => paths.iter_mut().for_each(|path| {
                path.pop();
            }),
            Component::Normal(segment) if has_glob(segment.as_bytes()) => {
                let mut matches = Vec::new();
                for parent in &paths {
                    let mut entries = fs::read_dir(parent)
                        .map_err(GlobFailure::Io)?
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(GlobFailure::Io)?;
                    entries.sort_by(|left, right| {
                        left.file_name()
                            .as_bytes()
                            .cmp(right.file_name().as_bytes())
                    });
                    for entry in entries {
                        *work = work.checked_add(1).ok_or(GlobFailure::Limit)?;
                        if *work > work_limit {
                            return Err(GlobFailure::Limit);
                        }
                        if glob_matches(segment.as_bytes(), entry.file_name().as_bytes()) {
                            matches.push(entry.path());
                            if matches.len() > match_limit {
                                return Err(GlobFailure::Limit);
                            }
                        }
                    }
                }
                paths = matches;
            }
            Component::Normal(segment) => paths.iter_mut().for_each(|path| path.push(segment)),
            Component::Prefix(_) => unreachable!("Unix paths have no prefix"),
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
        return Err(GlobFailure::Limit);
    }
    Ok(paths)
}

fn has_glob(value: &[u8]) -> bool {
    value.iter().any(|byte| matches!(byte, b'*' | b'?' | b'['))
}

fn glob_matches(pattern: &[u8], value: &[u8]) -> bool {
    if value.first() == Some(&b'.') && pattern.first() != Some(&b'.') {
        return false;
    }
    if pattern.len() > 4_096 || value.len() > 4_096 {
        return false;
    }
    let mut memo = vec![vec![None; value.len() + 1]; pattern.len() + 1];
    glob_matches_at(pattern, value, 0, 0, &mut memo)
}

fn glob_matches_at(
    pattern: &[u8],
    value: &[u8],
    pattern_index: usize,
    value_index: usize,
    memo: &mut [Vec<Option<bool>>],
) -> bool {
    if let Some(result) = memo[pattern_index][value_index] {
        return result;
    }
    let result = match pattern.get(pattern_index) {
        None => value_index == value.len(),
        Some(b'*') => {
            glob_matches_at(pattern, value, pattern_index + 1, value_index, memo)
                || (value_index < value.len()
                    && glob_matches_at(pattern, value, pattern_index, value_index + 1, memo))
        }
        Some(b'?') => {
            value_index < value.len()
                && glob_matches_at(pattern, value, pattern_index + 1, value_index + 1, memo)
        }
        Some(b'[') => class_match_at(pattern, value, pattern_index, value_index, memo),
        Some(literal) => {
            value.get(value_index) == Some(literal)
                && glob_matches_at(pattern, value, pattern_index + 1, value_index + 1, memo)
        }
    };
    memo[pattern_index][value_index] = Some(result);
    result
}

fn class_match_at(
    pattern: &[u8],
    value: &[u8],
    pattern_index: usize,
    value_index: usize,
    memo: &mut [Vec<Option<bool>>],
) -> bool {
    let Some(close) = pattern[pattern_index + 1..]
        .iter()
        .position(|byte| *byte == b']')
        .map(|offset| pattern_index + 1 + offset)
    else {
        return value.get(value_index) == Some(&b'[')
            && glob_matches_at(pattern, value, pattern_index + 1, value_index + 1, memo);
    };
    let Some(candidate) = value.get(value_index) else {
        return false;
    };
    let class = &pattern[pattern_index + 1..close];
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
    matched != negated && glob_matches_at(pattern, value, close + 1, value_index + 1, memo)
}

#[cfg(all(test, unix))]
mod tests {
    use std::{fs, path::PathBuf};

    use crate::E_SOURCE_CHANGED;

    use super::{VarnishLoadLimits, load_inner};

    #[test]
    fn final_recheck_detects_source_and_glob_changes() {
        let directory = temporary_directory();
        let includes = directory.join("conf.d");
        fs::create_dir_all(&includes).expect("include directory");
        let root = directory.join("root.vcl");
        fs::write(
            &root,
            b"vcl 4.1; include +glob \"conf.d/*.vcl\"; sub vcl_recv { return(hash); }",
        )
        .expect("root fixture");
        fs::write(
            includes.join("10-base.vcl"),
            b"sub vcl_hash { return(lookup); }",
        )
        .expect("include fixture");

        let changed_root = root.clone();
        let changed_includes = includes.clone();
        let report = load_inner(&root, VarnishLoadLimits::default(), move || {
            fs::write(changed_root, b"vcl 4.1; sub vcl_recv { return(pass); }")
                .expect("changed root");
            fs::write(
                changed_includes.join("20-added.vcl"),
                b"sub vcl_deliver { return(deliver); }",
            )
            .expect("added include");
        });

        assert!(!report.value().snapshot_stable);
        assert!(
            report
                .diagnostics()
                .iter()
                .filter(|diagnostic| diagnostic.code() == E_SOURCE_CHANGED)
                .count()
                >= 2
        );
        fs::remove_dir_all(directory).expect("remove fixture directory");
    }

    fn temporary_directory() -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "oxiroute-varnish-loader-{}-{nonce}",
            std::process::id()
        ))
    }
}
