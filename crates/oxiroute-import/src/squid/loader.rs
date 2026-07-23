use std::{
    collections::HashMap,
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
};

use super::source::{FileFingerprint, ReadFailure, stable_read};
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
    let mut loader = Loader::new(limits);
    if let Ok(root_id) = loader.ensure_source(&canonical, None, &[]) {
        loader.root = Some(root_id);
        let mut active = vec![root_id];
        loader.expand_source(root_id, &[], &mut active);
    }
    loader.final_recheck();
    loader.finish()
}

struct Loader {
    limits: SquidLoadLimits,
    root: Option<SourceId>,
    sources: Vec<SourceRecord>,
    source_ids: HashMap<PathBuf, SourceId>,
    includes: Vec<IncludeEdge>,
    expanded_directives: Vec<ExpandedDirective>,
    diagnostics: Vec<Diagnostic>,
    aggregate_source_bytes: usize,
    expanded_count: usize,
    glob_work: usize,
    snapshot_stable: bool,
}

impl Loader {
    fn new(limits: SquidLoadLimits) -> Self {
        Self {
            limits,
            root: None,
            sources: Vec::new(),
            source_ids: HashMap::new(),
            includes: Vec::new(),
            expanded_directives: Vec::new(),
            diagnostics: Vec::new(),
            aggregate_source_bytes: 0,
            expanded_count: 0,
            glob_work: 0,
            snapshot_stable: true,
        }
    }

    fn ensure_source(
        &mut self,
        canonical_path: &Path,
        primary_span: Option<Span>,
        include_stack: &[IncludeFrame],
    ) -> Result<SourceId, SourceLoadFailure> {
        if let Some(source) = self.source_ids.get(canonical_path) {
            return Ok(*source);
        }
        if self.sources.len() == self.limits.max_source_files {
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
        let (bytes, fingerprint) = match stable_read(canonical_path, self.limits.max_source_bytes) {
            Ok(read) => read,
            Err(ReadFailure::TooLarge) => {
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
            Err(ReadFailure::Changed) => {
                self.source_error(
                    E_SOURCE_CHANGED,
                    "Squid source changed while it was being read",
                    primary_span,
                    include_stack,
                );
                return Err(SourceLoadFailure::SourceChanged);
            }
            Err(ReadFailure::Io(error)) => {
                self.source_error(
                    E_SOURCE_IO,
                    format!("failed to read Squid source: {error}"),
                    primary_span,
                    include_stack,
                );
                return Err(SourceLoadFailure::SourceIo);
            }
        };
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
            Err(GlobFailure::Limit) => {
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
                            self.glob_work =
                                self.glob_work.checked_add(1).ok_or(GlobFailure::Limit)?;
                            if self.glob_work > self.limits.max_glob_work {
                                return Err(GlobFailure::Limit);
                            }
                            let name = entry.file_name();
                            if glob_matches(segment.as_bytes(), name.as_bytes()) {
                                matches.push(entry.path());
                                if matches.len() > self.limits.max_glob_matches {
                                    return Err(GlobFailure::Limit);
                                }
                            }
                        }
                    }
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
        if paths.len() > self.limits.max_glob_matches {
            return Err(GlobFailure::Limit);
        }
        Ok(paths)
    }

    fn final_recheck(&mut self) {
        for record in &self.sources {
            let changed = stable_read(&record.parsed.canonical_path, self.limits.max_source_bytes)
                .map_or(true, |(bytes, fingerprint)| {
                    bytes != record.parsed.source.bytes() || fingerprint != record.fingerprint
                });
            if changed {
                self.snapshot_stable = false;
                self.diagnostics.push(
                    Diagnostic::new(
                        E_SOURCE_CHANGED,
                        Severity::Error,
                        DiagnosticStage::Source,
                        "Squid source changed while the include graph was being loaded",
                    )
                    .with_primary_span(record.parsed.source.full_span())
                    .with_include_stack(record.first_include_stack.iter().copied()),
                );
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
    fingerprint: FileFingerprint,
    first_include_stack: Vec<Span>,
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
    Limit,
    Io(io::Error),
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
    use super::glob_matches;

    #[test]
    fn byte_glob_supports_squid_include_shapes() {
        assert!(glob_matches(b"*.conf", b"10-base.conf"));
        assert!(glob_matches(b"[0-9]?.conf", b"10.conf"));
        assert!(!glob_matches(b"*.conf", b".private.conf"));
        assert!(!glob_matches(b"[!0-9]*", b"1.conf"));
    }
}
