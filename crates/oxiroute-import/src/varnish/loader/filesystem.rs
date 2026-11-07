#[allow(clippy::wildcard_imports)]
use super::*;

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
pub(super) fn load_inner<F>(
    root: &Path,
    limits: VarnishLoadLimits,
    before_recheck: F,
) -> Report<SourceGraph>
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

#[cfg(unix)]
struct FilesystemLoader {
    limits: VarnishLoadLimits,
    root: Option<SourceId>,
    records: Vec<SourceRecord>,
    source_catalog: SourceCatalog,
    includes: Vec<IncludeEdge>,
    expanded: Vec<LoadedDeclaration>,
    diagnostics: Vec<Diagnostic>,
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
            source_catalog: SourceCatalog::new(SourceBudget {
                files: limits.source_files,
                source_bytes: limits.source_bytes,
                aggregate_bytes: limits.aggregate_source_bytes,
            }),
            includes: Vec::new(),
            expanded: Vec::new(),
            diagnostics: Vec::new(),
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
        if let Some(id) = self.source_catalog.source_id(path) {
            return Ok(id);
        }
        let source = match self.source_catalog.load(
            path,
            include_stack.to_vec(),
            SourceIdentity::Catalog,
            SourceNaming::Path,
        ) {
            Ok(source) => source,
            Err(SourceCatalogFailure::SourceFileLimit) => {
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
            Err(SourceCatalogFailure::SourceSizeLimit) => {
                self.error(
                    E_SOURCE_LIMIT,
                    "VCL source exceeds its byte limit",
                    span,
                    include_stack,
                );
                return Err(SourceLoadFailure::SourceSizeLimit);
            }
            Err(SourceCatalogFailure::Changed) => {
                self.snapshot_stable = false;
                self.error(
                    E_SOURCE_CHANGED,
                    "VCL source changed while being read",
                    span,
                    include_stack,
                );
                return Err(SourceLoadFailure::SourceChanged);
            }
            Err(SourceCatalogFailure::Io(error)) => {
                self.error(
                    E_SOURCE_IO,
                    format!("failed to read VCL source: {error}"),
                    span,
                    include_stack,
                );
                return Err(SourceLoadFailure::SourceIo);
            }
            Err(SourceCatalogFailure::AggregateSourceLimit) => {
                self.error(
                    E_SOURCE_LIMIT,
                    "VCL aggregate source byte limit exceeded",
                    span,
                    include_stack,
                );
                return Err(SourceLoadFailure::AggregateSourceLimit);
            }
        };
        let id = source.id();
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
        self.records.push(SourceRecord {
            parsed: ParsedSource {
                source,
                canonical_path: Some(path.to_path_buf()),
                document,
            },
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
        for (source, first_include_stack) in self.source_catalog.changed_snapshots() {
            self.snapshot_stable = false;
            self.diagnostics.push(
                Diagnostic::new(
                    E_SOURCE_CHANGED,
                    Severity::Error,
                    DiagnosticStage::Source,
                    "VCL source changed while its graph was loaded",
                )
                .with_primary_span(source.full_span())
                .with_include_stack(first_include_stack),
            );
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
