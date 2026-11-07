#[allow(clippy::wildcard_imports)]
use super::*;

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
