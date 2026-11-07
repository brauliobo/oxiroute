#[allow(clippy::wildcard_imports)]
use super::*;

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
