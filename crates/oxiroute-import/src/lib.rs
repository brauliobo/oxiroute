//! Shared primitives for parsing native configuration sources.

mod candidate;
mod canonical;
mod diagnostic;
pub mod haproxy;
mod limits;
pub mod nginx;
mod source;
#[cfg(unix)]
pub mod squid;
pub mod varnish;

pub use candidate::{
    ActivationRequirement, ActivationRequirementKind, CanonicalCandidate, CanonicalDraft,
    CanonicalProvenance, DeploymentRequirement, DeploymentRequirementKind, InactiveSource,
    OperationalOverlayKind, OperationalOverlayRequirement, ProvenanceRole, ProvenanceSpan,
    SourceImportMetadata, SourceMapSegment, SourceSpanMap,
};
pub use diagnostic::{
    Diagnostic, DiagnosticCode, DiagnosticStage, E_DUPLICATE_IDENTITY, E_INVALID_VALUE,
    E_SEMANTICS_NOT_REPRESENTABLE, E_UNRESOLVED_REFERENCE, E_UNSUPPORTED_FEATURE, RelatedSpan,
    Report, Severity,
};
pub use limits::{
    E_INCLUDE_CYCLE, E_INCLUDE_NOT_FOUND, E_SOURCE_CHANGED, E_SOURCE_IO, E_SOURCE_LIMIT,
    MAX_AGGREGATE_SOURCE_BYTES, MAX_DIRECTIVES_PER_SOURCE, MAX_EXPANDED_DIRECTIVES,
    MAX_GLOB_MATCHES, MAX_INCLUDE_DEPTH, MAX_SOURCE_BYTES, MAX_SOURCE_FILES, MAX_STRUCTURAL_DEPTH,
    MAX_TOKENS_PER_SOURCE,
};
pub use source::{ByteRange, SourceFile, SourceId, Span};
