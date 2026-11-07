#[allow(clippy::wildcard_imports)]
use super::*;

/// Stable schema version for the standalone machine-readable import report.
pub const IMPORT_REPORT_SCHEMA_VERSION: u32 = 1;

/// One source product and the capability profile used to interpret its snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSourceMetadata {
    pub product: String,
    /// The native version when the source itself provides one. Current importers do not infer it.
    pub version: Option<String>,
    /// Evidence for `version`, when present. This is null when no version evidence exists.
    pub version_source: Option<String>,
    pub capability_profile: CapabilityProfileMetadata,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityProfileMetadata {
    pub id: String,
    pub version: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReportEnvelope {
    pub schema_version: u32,
    pub source: ImportSourceMetadata,
    pub source_graph: SourceGraphEvidence,
    pub source_metadata: SourceMetadataEvidence,
    pub candidate: CandidateEvidence,
    pub blockers: Vec<ImportBlocker>,
    pub requirements: RequirementsEvidence,
    pub overlays: Vec<OverlayEvidence>,
    pub diagnostics: Vec<DiagnosticEvidence>,
    #[cfg(unix)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<crate::squid::SquidCapabilityReport>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceGraphEvidence {
    pub roots: Vec<SourceRootEvidence>,
    pub sources: Vec<SourceReference>,
    pub dependencies: Vec<DependencyEvidence>,
    /// False when the importer only exposes ordered roots and does not expose include edges.
    pub dependencies_complete: bool,
    pub snapshot_stable: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRootEvidence {
    pub ordinal: usize,
    pub path: Option<String>,
    pub source_ids: Vec<u32>,
    pub outcome: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceReference {
    pub id: u32,
    pub name: String,
    pub path: Option<String>,
    pub byte_length: usize,
    /// SHA-256 of the exact bounded source snapshot used by the importer.
    pub fingerprint_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DependencyEvidence {
    pub source_id: u32,
    pub target_source_id: Option<u32>,
    pub kind: String,
    pub requested_path: Option<String>,
    pub canonical_path: Option<String>,
    pub optional: Option<bool>,
    pub status: String,
    pub span: Option<SpanEvidence>,
    pub failure_code: Option<String>,
    /// The target source snapshot fingerprint when the dependency was loaded.
    pub fingerprint_sha256: Option<String>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceMetadataEvidence {
    pub environment_fingerprint_sha256: Option<String>,
    pub inactive_sources: Vec<InactiveSourceEvidence>,
    pub original_source_ids: Vec<u32>,
    pub source_maps: Vec<SourceMapEvidence>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InactiveSourceEvidence {
    pub condition: String,
    pub origin: SpanEvidence,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceMapEvidence {
    pub source_id: u32,
    pub segments: Vec<SourceMapSegmentEvidence>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceMapSegmentEvidence {
    pub generated: ByteRangeEvidence,
    pub original: ByteRangeEvidence,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ByteRangeEvidence {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpanEvidence {
    pub source_id: u32,
    pub range: ByteRangeEvidence,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateEvidence {
    pub finalized: bool,
    pub config: Option<oxiroute_config::ValidatedConfig>,
    pub draft: CandidateDraftEvidence,
    pub provenance: Vec<CanonicalProvenanceEvidence>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateDraftEvidence {
    pub version: u32,
    pub max_connections: Option<u64>,
    pub management: bool,
    pub stats: bool,
    pub certificates: usize,
    pub tls_profiles: usize,
    pub listeners: usize,
    pub upstream_pools: usize,
    pub http_services: usize,
    pub cache_stores: usize,
    pub forward_proxy_services: usize,
    pub rtmp_services: usize,
    pub l4_services: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalProvenanceEvidence {
    pub path: String,
    pub origins: Vec<OriginEvidence>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OriginEvidence {
    pub role: Option<String>,
    pub source_id: u32,
    pub range: Option<ByteRangeEvidence>,
    pub path: Option<String>,
    pub line: Option<usize>,
    pub include_stack: Vec<SpanEvidence>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequirementsEvidence {
    pub deployment: Vec<RequirementEvidence>,
    pub activation: Vec<RequirementEvidence>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequirementEvidence {
    pub kind: String,
    pub directive: String,
    pub values: Vec<String>,
    pub origins: Vec<OriginEvidence>,
    pub equivalent_runtime_endpoint: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayEvidence {
    pub id: String,
    pub kind: String,
    pub origin: Option<OriginEvidence>,
    pub redacted_evidence: bool,
    pub values: Vec<String>,
    pub satisfied: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportBlocker {
    pub id: String,
    pub kind: String,
    pub code: String,
    pub message: String,
    pub scope: Option<String>,
    pub occurrence_ids: Vec<String>,
    pub origins: Vec<SpanEvidence>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticEvidence {
    pub code: String,
    pub severity: String,
    pub stage: String,
    pub message: String,
    pub primary_span: Option<SpanEvidence>,
    pub include_stack: Vec<SpanEvidence>,
    pub related_spans: Vec<RelatedSpanEvidence>,
    pub help: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelatedSpanEvidence {
    pub span: SpanEvidence,
    pub message: String,
}
