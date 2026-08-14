use std::{fmt::Write as _, path::Path};

use openssl::sha::sha256;
use serde::Serialize;

use crate::{
    ActivationRequirement, CanonicalCandidate, CanonicalCandidateSummary, CanonicalProvenance,
    DeploymentRequirement, Diagnostic, OperationalOverlayRequirement, Report, Severity, SourceFile,
    SourceId, SourceImportMetadata, Span,
};

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

impl ImportReportEnvelope {
    /// Builds the shared evidence envelope for a complete nginx root import.
    #[must_use]
    pub fn from_nginx(report: &crate::nginx::NginxImportReport) -> Self {
        let candidate = candidate_evidence(&report.candidate, nginx_origin);
        let blockers = nginx_blockers(report);
        Self::assemble(
            source_metadata("nginx", "nginx-root"),
            nginx_graph(&report.source_graph),
            source_metadata_evidence(&graph_source_metadata(
                report
                    .source_graph
                    .sources
                    .iter()
                    .map(|source| &source.source),
            )),
            candidate,
            shared_requirements(&report.candidate, nginx_origin),
            shared_overlays(&report.candidate, nginx_origin),
            &report.diagnostics,
            blockers,
        )
    }

    /// Builds the shared evidence envelope for an ordered `HAProxy` root import.
    #[must_use]
    pub fn from_haproxy<P: AsRef<Path>>(
        report: &Report<crate::haproxy::CanonicalCandidate>,
        roots: &[P],
    ) -> Self {
        let candidate = report.value();
        Self::assemble(
            source_metadata("haproxy", "haproxy-strict"),
            haproxy_graph(&candidate.source_metadata, roots),
            source_metadata_evidence(&candidate.source_metadata),
            candidate_evidence(candidate, haproxy_origin),
            shared_requirements(candidate, haproxy_origin),
            shared_overlays(candidate, haproxy_origin),
            report.diagnostics(),
            Vec::new(),
        )
    }

    /// Builds the shared evidence envelope for a Squid forward-proxy import.
    #[cfg(unix)]
    #[must_use]
    pub fn from_squid(report: &crate::squid::ImportReport) -> Self {
        let candidate = candidate_evidence(&report.candidate, squid_origin);
        let mut envelope = Self::assemble(
            source_metadata_with_version(
                "squid",
                crate::squid::SQUID_CAPABILITY_PROFILE_ID,
                crate::squid::SQUID_CAPABILITY_PROFILE_VERSION,
                None,
                None,
            ),
            squid_graph(&report.source_graph),
            source_metadata_evidence(&report.candidate.source_metadata),
            candidate,
            RequirementsEvidence::default(),
            Vec::new(),
            &report.diagnostics,
            squid_blockers(report),
        );
        envelope.capabilities = Some(report.capabilities);
        envelope
    }

    /// Builds the shared evidence envelope for a static Apache httpd import.
    #[must_use]
    pub fn from_apache(report: &crate::apache::ApacheImportReport) -> Self {
        let candidate = &report.candidate;
        Self::assemble(
            source_metadata("apache", "apache-static-reverse-proxy"),
            apache_graph(&report.source_graph),
            source_metadata_evidence(&candidate.source_metadata),
            candidate_evidence(candidate, apache_origin),
            shared_requirements(candidate, apache_origin),
            shared_overlays(candidate, apache_origin),
            &report.diagnostics,
            apache_blockers(report),
        )
    }

    /// Builds the shared evidence envelope for an exact Varnish VCL import.
    #[must_use]
    pub fn from_varnish(report: &crate::varnish::ImportReport) -> Self {
        let candidate = &report.candidate;
        Self::assemble(
            varnish_source_metadata(report),
            varnish_graph(&report.source_graph),
            source_metadata_evidence(&candidate.source_metadata),
            candidate_evidence(candidate, varnish_origin),
            shared_requirements(candidate, varnish_origin),
            shared_overlays(candidate, varnish_origin),
            &report.diagnostics,
            varnish_blockers(report),
        )
    }

    /// Serializes the envelope as one stable compact JSON document.
    ///
    /// # Errors
    ///
    /// Returns a serialization error if the canonical candidate contains a value that JSON cannot
    /// represent, such as a non-UTF-8 path.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Serializes the envelope as one newline-terminated JSON document for CLI output.
    ///
    /// # Errors
    ///
    /// Returns the serialization error from [`Self::to_json`].
    pub fn to_json_line(&self) -> Result<String, serde_json::Error> {
        let mut json = self.to_json()?;
        json.push('\n');
        Ok(json)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the envelope keeps independent evidence sections in one stable constructor"
    )]
    fn assemble(
        source: ImportSourceMetadata,
        source_graph: SourceGraphEvidence,
        source_metadata: SourceMetadataEvidence,
        candidate: CandidateEvidence,
        requirements: RequirementsEvidence,
        overlays: Vec<OverlayEvidence>,
        diagnostics: &[Diagnostic],
        typed_blockers: Vec<ImportBlocker>,
    ) -> Self {
        let sorted = sorted_diagnostics(diagnostics);
        let mut blockers = diagnostic_blockers(&sorted);
        blockers.extend(typed_blockers);
        blockers.sort_by(|left, right| {
            (
                left.kind.as_str(),
                left.scope.as_deref().unwrap_or_default(),
                left.code.as_str(),
                left.id.as_str(),
            )
                .cmp(&(
                    right.kind.as_str(),
                    right.scope.as_deref().unwrap_or_default(),
                    right.code.as_str(),
                    right.id.as_str(),
                ))
        });
        Self {
            schema_version: IMPORT_REPORT_SCHEMA_VERSION,
            source,
            source_graph,
            source_metadata,
            candidate,
            blockers,
            requirements,
            overlays,
            diagnostics: sorted.iter().map(diagnostic_evidence).collect(),
            #[cfg(unix)]
            capabilities: None,
        }
    }
}

fn source_metadata(product: &str, profile: &str) -> ImportSourceMetadata {
    source_metadata_with_version(product, profile, 1, None, None)
}

fn source_metadata_with_version(
    product: &str,
    profile: &str,
    profile_version: u32,
    version: Option<String>,
    version_source: Option<String>,
) -> ImportSourceMetadata {
    ImportSourceMetadata {
        product: product.into(),
        version,
        version_source,
        capability_profile: CapabilityProfileMetadata {
            id: profile.into(),
            version: profile_version,
        },
    }
}

fn varnish_source_metadata(report: &crate::varnish::ImportReport) -> ImportSourceMetadata {
    let version = report.declarations.iter().find_map(|declaration| {
        declaration
            .version
            .effective
            .as_ref()
            .and_then(|version| String::from_utf8(version.as_bytes().to_vec()).ok())
            .map(|version| (version, declaration.version.origin))
    });
    source_metadata_with_version(
        "varnish",
        crate::varnish::VARNISH_CAPABILITY_PROFILE_ID,
        crate::varnish::VARNISH_CAPABILITY_PROFILE_VERSION,
        version.as_ref().map(|(version, _)| version.clone()),
        version.map(|(_, origin)| snake_case(&format!("{origin:?}"))),
    )
}

fn candidate_evidence<O>(
    candidate: &CanonicalCandidate<O>,
    origin: impl Fn(&O) -> OriginEvidence,
) -> CandidateEvidence {
    candidate_evidence_parts(
        candidate.summary(),
        candidate.validated(),
        &candidate.provenance,
        origin,
    )
}

fn candidate_evidence_parts<O>(
    summary: CanonicalCandidateSummary,
    config: Option<&oxiroute_config::ValidatedConfig>,
    provenance: &[CanonicalProvenance<O>],
    origin: impl Fn(&O) -> OriginEvidence,
) -> CandidateEvidence {
    CandidateEvidence {
        finalized: config.is_some(),
        config: config.cloned(),
        draft: draft_evidence(summary),
        provenance: provenance
            .iter()
            .map(|entry| CanonicalProvenanceEvidence {
                path: entry.path.clone(),
                origins: entry.origins.iter().map(&origin).collect(),
            })
            .collect(),
    }
}

fn draft_evidence(summary: CanonicalCandidateSummary) -> CandidateDraftEvidence {
    CandidateDraftEvidence {
        version: summary.version,
        max_connections: summary.max_connections,
        management: summary.management,
        stats: summary.stats,
        certificates: summary.certificates,
        tls_profiles: summary.tls_profiles,
        listeners: summary.listeners,
        upstream_pools: summary.upstream_pools,
        http_services: summary.http_services,
        cache_stores: summary.cache_stores,
        forward_proxy_services: summary.forward_proxy_services,
        rtmp_services: summary.rtmp_services,
        l4_services: summary.l4_services,
    }
}

fn shared_requirements<O>(
    candidate: &CanonicalCandidate<O>,
    origin: impl Fn(&O) -> OriginEvidence,
) -> RequirementsEvidence {
    RequirementsEvidence {
        deployment: candidate
            .deployment_requirements
            .iter()
            .map(|requirement| deployment_requirement(requirement, &origin))
            .collect(),
        activation: candidate
            .activation_requirements
            .iter()
            .map(|requirement| activation_requirement(requirement, &origin))
            .collect(),
    }
}

fn deployment_requirement<O>(
    requirement: &DeploymentRequirement<O>,
    origin: &impl Fn(&O) -> OriginEvidence,
) -> RequirementEvidence {
    RequirementEvidence {
        kind: snake_case(&format!("{:?}", requirement.kind)),
        directive: requirement.directive.clone(),
        values: requirement.value.clone(),
        origins: vec![origin(&requirement.origin)],
        equivalent_runtime_endpoint: None,
    }
}

fn activation_requirement<O>(
    requirement: &ActivationRequirement<O>,
    origin: &impl Fn(&O) -> OriginEvidence,
) -> RequirementEvidence {
    RequirementEvidence {
        kind: snake_case(&format!("{:?}", requirement.kind)),
        directive: requirement.directive.clone(),
        values: Vec::new(),
        origins: vec![origin(&requirement.origin)],
        equivalent_runtime_endpoint: Some(requirement.equivalent_runtime_endpoint),
    }
}

fn shared_overlays<O>(
    candidate: &CanonicalCandidate<O>,
    origin: impl Fn(&O) -> OriginEvidence,
) -> Vec<OverlayEvidence> {
    candidate
        .operational_overlays
        .iter()
        .map(|overlay| overlay_evidence(overlay, &origin))
        .collect()
}

fn overlay_evidence<O>(
    overlay: &OperationalOverlayRequirement<O>,
    origin: &impl Fn(&O) -> OriginEvidence,
) -> OverlayEvidence {
    OverlayEvidence {
        id: overlay.id.clone(),
        kind: snake_case(&format!("{:?}", overlay.kind)),
        origin: overlay.origin.as_ref().map(origin),
        redacted_evidence: overlay.redacted_evidence,
        values: overlay.values.clone(),
        satisfied: overlay.satisfied,
    }
}

fn source_metadata_evidence(metadata: &SourceImportMetadata) -> SourceMetadataEvidence {
    SourceMetadataEvidence {
        environment_fingerprint_sha256: metadata.environment_fingerprint_sha256.clone(),
        inactive_sources: metadata
            .inactive_sources
            .iter()
            .map(|source| InactiveSourceEvidence {
                condition: source.condition.clone(),
                origin: span_evidence(source.origin),
            })
            .collect(),
        original_source_ids: metadata
            .original_sources
            .iter()
            .map(|source| source.id().get())
            .collect(),
        source_maps: metadata
            .source_maps
            .iter()
            .map(source_map_evidence)
            .collect(),
    }
}

fn graph_source_metadata<'a>(
    sources: impl Iterator<Item = &'a SourceFile>,
) -> SourceImportMetadata {
    SourceImportMetadata {
        original_sources: sources.cloned().collect(),
        ..SourceImportMetadata::default()
    }
}

fn source_map_evidence(map: &crate::SourceSpanMap) -> SourceMapEvidence {
    SourceMapEvidence {
        source_id: map.source.get(),
        segments: map
            .segments
            .iter()
            .map(|segment| SourceMapSegmentEvidence {
                generated: byte_range_evidence(segment.generated),
                original: byte_range_evidence(segment.original),
            })
            .collect(),
    }
}

fn source_reference(source: &SourceFile, canonical_path: Option<&Path>) -> SourceReference {
    SourceReference {
        id: source.id().get(),
        name: source.name().to_owned(),
        path: canonical_path
            .and_then(path_string)
            .or_else(|| source.path().and_then(path_string)),
        byte_length: source.len(),
        fingerprint_sha256: fingerprint_sha256(source.bytes()),
    }
}

fn span_evidence(span: Span) -> SpanEvidence {
    SpanEvidence {
        source_id: span.source().get(),
        range: byte_range_evidence(span.range()),
    }
}

fn byte_range_evidence(range: crate::ByteRange) -> ByteRangeEvidence {
    ByteRangeEvidence {
        start: range.start(),
        end: range.end(),
    }
}

fn path_string(path: &Path) -> Option<String> {
    path.to_str().map(str::to_owned)
}

fn bytes_string(bytes: &[u8]) -> Option<String> {
    String::from_utf8(bytes.to_vec()).ok()
}

fn fingerprint_sha256(bytes: &[u8]) -> String {
    let mut fingerprint = String::with_capacity(64);
    for byte in sha256(bytes) {
        write!(fingerprint, "{byte:02x}").expect("writing to a string cannot fail");
    }
    fingerprint
}

fn source_fingerprint(sources: &[SourceReference], source_id: Option<u32>) -> Option<String> {
    source_id.and_then(|source_id| {
        sources
            .iter()
            .find(|source| source.id == source_id)
            .map(|source| source.fingerprint_sha256.clone())
    })
}

fn source_roots(root: Option<SourceId>, sources: &[SourceReference]) -> Vec<SourceRootEvidence> {
    let Some(root) = root else {
        return vec![SourceRootEvidence {
            ordinal: 0,
            path: None,
            source_ids: Vec::new(),
            outcome: Some("not_loaded".into()),
        }];
    };
    let source = sources.iter().find(|source| source.id == root.get());
    vec![SourceRootEvidence {
        ordinal: 0,
        path: source.and_then(|source| source.path.clone()),
        source_ids: vec![root.get()],
        outcome: Some("loaded".into()),
    }]
}

struct SourceDependencyAdapter {
    source_id: u32,
    target_source_id: Option<u32>,
    requested_path: Option<String>,
    canonical_path: Option<String>,
    optional: Option<bool>,
    status: &'static str,
    span: Option<SpanEvidence>,
    failure_code: Option<String>,
    truncated: bool,
}

struct SourceGraphEvidenceBuilder {
    roots: Vec<SourceRootEvidence>,
    sources: Vec<SourceReference>,
    dependencies: Vec<DependencyEvidence>,
    dependencies_complete: bool,
    snapshot_stable: Option<bool>,
}

impl SourceGraphEvidenceBuilder {
    fn new(
        root: Option<SourceId>,
        sources: Vec<SourceReference>,
        dependencies_complete: bool,
        snapshot_stable: Option<bool>,
    ) -> Self {
        Self {
            roots: source_roots(root, &sources),
            sources,
            dependencies: Vec::new(),
            dependencies_complete,
            snapshot_stable,
        }
    }

    fn dependency(&mut self, adapter: SourceDependencyAdapter) {
        self.dependencies.push(DependencyEvidence {
            source_id: adapter.source_id,
            target_source_id: adapter.target_source_id,
            kind: "include".into(),
            requested_path: adapter.requested_path,
            canonical_path: adapter.canonical_path,
            optional: adapter.optional,
            status: adapter.status.into(),
            span: adapter.span,
            failure_code: adapter.failure_code,
            fingerprint_sha256: source_fingerprint(&self.sources, adapter.target_source_id),
            truncated: adapter.truncated,
        });
    }

    fn finish(self) -> SourceGraphEvidence {
        SourceGraphEvidence {
            roots: self.roots,
            sources: self.sources,
            dependencies: self.dependencies,
            dependencies_complete: self.dependencies_complete,
            snapshot_stable: self.snapshot_stable,
        }
    }
}

fn nginx_graph(graph: &crate::nginx::SourceGraph) -> SourceGraphEvidence {
    let sources = graph
        .sources
        .iter()
        .map(|source| source_reference(&source.source, Some(&source.canonical_path)))
        .collect::<Vec<_>>();
    let mut builder =
        SourceGraphEvidenceBuilder::new(graph.root, sources, true, Some(graph.snapshot_stable));
    for edge in &graph.includes {
        if edge.candidates.is_empty() {
            for target in &edge.targets {
                builder.dependency(SourceDependencyAdapter {
                    source_id: edge.source.get(),
                    target_source_id: Some(target.get()),
                    requested_path: bytes_string(&edge.pattern),
                    canonical_path: None,
                    optional: Some(false),
                    status: "expanded",
                    span: Some(span_evidence(edge.span)),
                    failure_code: edge.failure.map(|code| code.as_str().into()),
                    truncated: edge.truncated,
                });
            }
            if edge.targets.is_empty() {
                builder.dependency(SourceDependencyAdapter {
                    source_id: edge.source.get(),
                    target_source_id: None,
                    requested_path: bytes_string(&edge.pattern),
                    canonical_path: None,
                    optional: Some(false),
                    status: "failed",
                    span: Some(span_evidence(edge.span)),
                    failure_code: edge.failure.map(|code| code.as_str().into()),
                    truncated: edge.truncated,
                });
            }
            continue;
        }
        for candidate in &edge.candidates {
            let (target_source_id, status) = nginx_target_status(candidate.status);
            builder.dependency(SourceDependencyAdapter {
                source_id: edge.source.get(),
                target_source_id,
                requested_path: path_string(&candidate.path),
                canonical_path: candidate.canonical_path.as_deref().and_then(path_string),
                optional: Some(false),
                status,
                span: Some(span_evidence(edge.span)),
                failure_code: edge.failure.map(|code| code.as_str().into()),
                truncated: edge.truncated,
            });
        }
    }
    builder.finish()
}

fn nginx_target_status(
    status: crate::nginx::IncludeCandidateStatus,
) -> (Option<u32>, &'static str) {
    match status {
        crate::nginx::IncludeCandidateStatus::Expanded(source)
        | crate::nginx::IncludeCandidateStatus::Cycle(source)
        | crate::nginx::IncludeCandidateStatus::ExpansionLimit(source) => {
            let status = match status {
                crate::nginx::IncludeCandidateStatus::Expanded(_) => "expanded",
                crate::nginx::IncludeCandidateStatus::Cycle(_) => "cycle",
                crate::nginx::IncludeCandidateStatus::ExpansionLimit(_) => "expansion_limit",
                _ => unreachable!(),
            };
            (Some(source.get()), status)
        }
        crate::nginx::IncludeCandidateStatus::CanonicalizeFailed => (None, "canonicalize_failed"),
        crate::nginx::IncludeCandidateStatus::SourceIo => (None, "source_io"),
        crate::nginx::IncludeCandidateStatus::SourceChanged => (None, "source_changed"),
        crate::nginx::IncludeCandidateStatus::SourceSizeLimit => (None, "source_size_limit"),
        crate::nginx::IncludeCandidateStatus::SourceFileLimit => (None, "source_file_limit"),
        crate::nginx::IncludeCandidateStatus::AggregateSourceLimit => {
            (None, "aggregate_source_limit")
        }
    }
}

fn apache_graph(graph: &crate::apache::SourceGraph) -> SourceGraphEvidence {
    let sources = graph
        .sources
        .iter()
        .map(|source| source_reference(&source.source, Some(&source.canonical_path)))
        .collect::<Vec<_>>();
    let mut builder =
        SourceGraphEvidenceBuilder::new(graph.root, sources, true, Some(graph.snapshot_stable));
    for edge in &graph.includes {
        if edge.candidates.is_empty() {
            for target in &edge.targets {
                builder.dependency(SourceDependencyAdapter {
                    source_id: edge.source.get(),
                    target_source_id: Some(target.get()),
                    requested_path: bytes_string(&edge.pattern),
                    canonical_path: None,
                    optional: Some(edge.optional),
                    status: "expanded",
                    span: Some(span_evidence(edge.span)),
                    failure_code: edge.failure.map(|code| code.as_str().into()),
                    truncated: edge.truncated,
                });
            }
            if edge.targets.is_empty() {
                builder.dependency(SourceDependencyAdapter {
                    source_id: edge.source.get(),
                    target_source_id: None,
                    requested_path: bytes_string(&edge.pattern),
                    canonical_path: None,
                    optional: Some(edge.optional),
                    status: if edge.optional && edge.failure.is_none() {
                        "optional_missing"
                    } else {
                        "failed"
                    },
                    span: Some(span_evidence(edge.span)),
                    failure_code: edge.failure.map(|code| code.as_str().into()),
                    truncated: edge.truncated,
                });
            }
            continue;
        }
        for candidate in &edge.candidates {
            let (target_source_id, status) = apache_target_status(candidate.status);
            builder.dependency(SourceDependencyAdapter {
                source_id: edge.source.get(),
                target_source_id,
                requested_path: path_string(&candidate.path),
                canonical_path: candidate.canonical_path.as_deref().and_then(path_string),
                optional: Some(edge.optional),
                status,
                span: Some(span_evidence(edge.span)),
                failure_code: edge.failure.map(|code| code.as_str().into()),
                truncated: edge.truncated,
            });
        }
    }
    builder.finish()
}

fn apache_target_status(
    status: crate::apache::IncludeCandidateStatus,
) -> (Option<u32>, &'static str) {
    match status {
        crate::apache::IncludeCandidateStatus::Expanded(source) => (Some(source.get()), "expanded"),
        crate::apache::IncludeCandidateStatus::Cycle(source) => (Some(source.get()), "cycle"),
        crate::apache::IncludeCandidateStatus::CanonicalizeFailed => (None, "canonicalize_failed"),
        crate::apache::IncludeCandidateStatus::SourceIo => (None, "source_io"),
        crate::apache::IncludeCandidateStatus::SourceChanged => (None, "source_changed"),
        crate::apache::IncludeCandidateStatus::SourceSizeLimit => (None, "source_size_limit"),
        crate::apache::IncludeCandidateStatus::SourceFileLimit => (None, "source_file_limit"),
        crate::apache::IncludeCandidateStatus::AggregateSourceLimit => {
            (None, "aggregate_source_limit")
        }
    }
}

fn varnish_graph(graph: &crate::varnish::SourceGraph) -> SourceGraphEvidence {
    let sources = graph
        .sources
        .iter()
        .map(|source| source_reference(&source.source, source.canonical_path.as_deref()))
        .collect::<Vec<_>>();
    let mut builder =
        SourceGraphEvidenceBuilder::new(graph.root, sources, true, Some(graph.snapshot_stable));
    for edge in &graph.includes {
        if edge.targets.is_empty() {
            builder.dependency(SourceDependencyAdapter {
                source_id: edge.source.get(),
                target_source_id: None,
                requested_path: bytes_string(&edge.pattern),
                canonical_path: None,
                optional: Some(false),
                status: "failed",
                span: Some(span_evidence(edge.span)),
                failure_code: edge.failure.map(|code| code.as_str().into()),
                truncated: edge.truncated,
            });
            continue;
        }
        for target in &edge.targets {
            let (target_source_id, status) = varnish_target_status(target.status);
            builder.dependency(SourceDependencyAdapter {
                source_id: edge.source.get(),
                target_source_id,
                requested_path: path_string(&target.requested_path),
                canonical_path: target.canonical_path.as_deref().and_then(path_string),
                optional: Some(false),
                status,
                span: Some(span_evidence(edge.span)),
                failure_code: edge.failure.map(|code| code.as_str().into()),
                truncated: edge.truncated,
            });
        }
    }
    builder.finish()
}

fn varnish_target_status(
    status: crate::varnish::IncludeTargetStatus,
) -> (Option<u32>, &'static str) {
    match status {
        crate::varnish::IncludeTargetStatus::Expanded(source) => (Some(source.get()), "expanded"),
        crate::varnish::IncludeTargetStatus::Cycle(source) => (Some(source.get()), "cycle"),
        crate::varnish::IncludeTargetStatus::SourceIo => (None, "source_io"),
        crate::varnish::IncludeTargetStatus::SourceChanged => (None, "source_changed"),
        crate::varnish::IncludeTargetStatus::SourceSizeLimit => (None, "source_size_limit"),
        crate::varnish::IncludeTargetStatus::SourceFileLimit => (None, "source_file_limit"),
        crate::varnish::IncludeTargetStatus::AggregateSourceLimit => {
            (None, "aggregate_source_limit")
        }
        crate::varnish::IncludeTargetStatus::ExpansionLimit => (None, "expansion_limit"),
    }
}

#[cfg(unix)]
fn squid_graph(graph: &crate::squid::SourceGraph) -> SourceGraphEvidence {
    let sources = graph
        .sources
        .iter()
        .map(|source| source_reference(&source.source, Some(&source.canonical_path)))
        .collect::<Vec<_>>();
    let mut builder =
        SourceGraphEvidenceBuilder::new(graph.root, sources, true, Some(graph.snapshot_stable));
    for edge in &graph.includes {
        if edge.targets.is_empty() {
            builder.dependency(SourceDependencyAdapter {
                source_id: edge.source.get(),
                target_source_id: None,
                requested_path: None,
                canonical_path: None,
                optional: Some(false),
                status: "failed",
                span: Some(span_evidence(edge.span)),
                failure_code: edge.failure.map(|code| code.as_str().into()),
                truncated: edge.truncated,
            });
        }
        for target in &edge.targets {
            let (target_source_id, status) = squid_target_status(target.status);
            builder.dependency(SourceDependencyAdapter {
                source_id: edge.source.get(),
                target_source_id,
                requested_path: path_string(&target.requested_path),
                canonical_path: target.canonical_path.as_deref().and_then(path_string),
                optional: Some(false),
                status,
                span: Some(span_evidence(edge.span)),
                failure_code: edge.failure.map(|code| code.as_str().into()),
                truncated: edge.truncated,
            });
        }
    }
    builder.finish()
}

#[cfg(unix)]
fn squid_target_status(status: crate::squid::IncludeTargetStatus) -> (Option<u32>, &'static str) {
    match status {
        crate::squid::IncludeTargetStatus::Expanded(source) => (Some(source.get()), "expanded"),
        crate::squid::IncludeTargetStatus::Cycle(source) => (Some(source.get()), "cycle"),
        crate::squid::IncludeTargetStatus::SourceIo => (None, "source_io"),
        crate::squid::IncludeTargetStatus::SourceChanged => (None, "source_changed"),
        crate::squid::IncludeTargetStatus::SourceSizeLimit => (None, "source_size_limit"),
        crate::squid::IncludeTargetStatus::SourceFileLimit => (None, "source_file_limit"),
        crate::squid::IncludeTargetStatus::AggregateSourceLimit => (None, "aggregate_source_limit"),
        crate::squid::IncludeTargetStatus::ExpansionLimit => (None, "expansion_limit"),
        crate::squid::IncludeTargetStatus::UnsupportedPipe => (None, "unsupported_pipe"),
    }
}

fn haproxy_graph<P: AsRef<Path>>(
    metadata: &SourceImportMetadata,
    roots: &[P],
) -> SourceGraphEvidence {
    let sources = metadata
        .original_sources
        .iter()
        .map(|source| source_reference(source, None))
        .collect::<Vec<_>>();
    let root_evidence = roots
        .iter()
        .enumerate()
        .map(|(ordinal, root)| {
            let path = root.as_ref();
            let source_ids = sources
                .iter()
                .filter(|source| {
                    source
                        .path
                        .as_deref()
                        .is_some_and(|loaded| path.to_str() == Some(loaded))
                })
                .map(|source| source.id)
                .collect::<Vec<u32>>();
            SourceRootEvidence {
                ordinal,
                path: path_string(path),
                outcome: Some(if source_ids.is_empty() {
                    "unknown".into()
                } else {
                    "loaded".into()
                }),
                source_ids,
            }
        })
        .collect();
    let snapshot_stable = if metadata.original_sources.is_empty() {
        None
    } else {
        Some(true)
    };
    SourceGraphEvidenceBuilder {
        roots: root_evidence,
        sources,
        dependencies: Vec::new(),
        dependencies_complete: false,
        snapshot_stable,
    }
    .finish()
}

fn nginx_origin(origin: &crate::nginx::DirectiveOrigin) -> OriginEvidence {
    OriginEvidence {
        role: None,
        source_id: origin.span.source().get(),
        range: Some(byte_range_evidence(origin.span.range())),
        path: None,
        line: None,
        include_stack: origin
            .provenance
            .include_stack
            .iter()
            .map(|frame| span_evidence(frame.directive_span))
            .collect(),
    }
}

fn apache_origin(origin: &crate::apache::ApacheProvenance) -> OriginEvidence {
    OriginEvidence {
        role: Some(snake_case(&format!("{:?}", origin.role))),
        source_id: origin.span.source().get(),
        range: Some(byte_range_evidence(origin.span.range())),
        path: path_string(&origin.path),
        line: Some(origin.line),
        include_stack: origin
            .include_stack
            .iter()
            .map(|frame| span_evidence(frame.directive_span))
            .collect(),
    }
}

fn varnish_origin(origin: &crate::varnish::Provenance) -> OriginEvidence {
    OriginEvidence {
        role: None,
        source_id: origin.span.source().get(),
        range: Some(byte_range_evidence(origin.span.range())),
        path: None,
        line: None,
        include_stack: origin
            .include_stack
            .iter()
            .copied()
            .map(span_evidence)
            .collect(),
    }
}

fn haproxy_origin(origin: &crate::ProvenanceSpan) -> OriginEvidence {
    OriginEvidence {
        role: Some(snake_case(&format!("{:?}", origin.role))),
        source_id: origin.span.source().get(),
        range: Some(byte_range_evidence(origin.span.range())),
        path: None,
        line: None,
        include_stack: Vec::new(),
    }
}

#[cfg(unix)]
fn squid_origin(origin: &crate::squid::DirectiveOrigin) -> OriginEvidence {
    OriginEvidence {
        role: None,
        source_id: origin.directive_span.source().get(),
        range: Some(byte_range_evidence(origin.directive_span.range())),
        path: None,
        line: None,
        include_stack: origin
            .provenance
            .include_stack
            .iter()
            .map(|frame| span_evidence(frame.directive_span))
            .collect(),
    }
}

fn sorted_diagnostics(diagnostics: &[Diagnostic]) -> Vec<Diagnostic> {
    Report::new((), diagnostics.to_vec()).into_parts().1
}

fn diagnostic_evidence(diagnostic: &Diagnostic) -> DiagnosticEvidence {
    DiagnosticEvidence {
        code: diagnostic.code().as_str().into(),
        severity: snake_case(&format!("{:?}", diagnostic.severity())),
        stage: snake_case(&format!("{:?}", diagnostic.stage())),
        message: diagnostic.message().to_owned(),
        primary_span: diagnostic.primary_span().map(span_evidence),
        include_stack: diagnostic
            .include_stack()
            .iter()
            .copied()
            .map(span_evidence)
            .collect(),
        related_spans: diagnostic
            .related_spans()
            .iter()
            .map(|related| RelatedSpanEvidence {
                span: span_evidence(related.span()),
                message: related.message().to_owned(),
            })
            .collect(),
        help: diagnostic.help().map(str::to_owned),
    }
}

fn diagnostic_blockers(diagnostics: &[Diagnostic]) -> Vec<ImportBlocker> {
    diagnostics
        .iter()
        .enumerate()
        .filter(|(_, diagnostic)| diagnostic.severity() == Severity::Error)
        .map(|(index, diagnostic)| {
            let mut origins = diagnostic
                .primary_span()
                .into_iter()
                .map(span_evidence)
                .collect::<Vec<_>>();
            origins.extend(
                diagnostic
                    .include_stack()
                    .iter()
                    .copied()
                    .map(span_evidence),
            );
            ImportBlocker {
                id: format!("diagnostic-{index:04}"),
                kind: "diagnostic".into(),
                code: diagnostic.code().as_str().into(),
                message: diagnostic.message().to_owned(),
                scope: None,
                occurrence_ids: Vec::new(),
                origins,
            }
        })
        .collect()
}

fn typed_blocker(
    id: String,
    kind: impl Into<String>,
    code: crate::DiagnosticCode,
    message: String,
    scope: Option<String>,
    occurrence_ids: Vec<String>,
    origins: Vec<SpanEvidence>,
) -> ImportBlocker {
    ImportBlocker {
        id,
        kind: kind.into(),
        code: code.as_str().into(),
        message,
        scope,
        occurrence_ids,
        origins,
    }
}

fn varnish_blockers(report: &crate::varnish::ImportReport) -> Vec<ImportBlocker> {
    let crate::varnish::LoweringStatus::Blocked(blocker) = report.lowering else {
        return Vec::new();
    };
    let (code, message) = match blocker {
        crate::varnish::LoweringBlocker::NoCanonicalGraph => (
            crate::varnish::E_VCL_LOWERING_BLOCKED,
            "Varnish source contains no canonical HTTP lowering graph",
        ),
        crate::varnish::LoweringBlocker::InvalidSource => (
            crate::varnish::E_VCL_LOWERING_BLOCKED,
            "Varnish source is invalid for exact canonical lowering",
        ),
        crate::varnish::LoweringBlocker::UnsupportedBehavior => (
            crate::varnish::E_VCL_LOWERING_BLOCKED,
            "Varnish behavior is outside the exact canonical lowering subset",
        ),
        crate::varnish::LoweringBlocker::UnsupportedSubroutine => (
            crate::varnish::E_VCL_UNSUPPORTED_SUBROUTINE,
            "Varnish subroutine behavior is outside the exact canonical lowering graph",
        ),
        crate::varnish::LoweringBlocker::SemanticMismatch => (
            crate::varnish::E_VCL_SEMANTIC_MISMATCH,
            "Varnish behavior has no semantics-preserving canonical representation",
        ),
        crate::varnish::LoweringBlocker::Invocation => (
            crate::varnish::E_VCL_LOWERING_BLOCKED,
            "Varnish invocation facts are insufficient for exact canonical lowering",
        ),
        crate::varnish::LoweringBlocker::Validation => (
            crate::E_INVALID_VALUE,
            "lowered Varnish configuration failed canonical validation",
        ),
    };
    let origins = report
        .source_graph
        .root
        .and_then(|root| report.source_graph.source(root))
        .map(|source| span_evidence(source.source.full_span()))
        .into_iter()
        .collect();
    vec![typed_blocker(
        "varnish-lowering".into(),
        "lowering",
        code,
        message.into(),
        None,
        Vec::new(),
        origins,
    )]
}

fn message_for_code(diagnostics: &[Diagnostic], code: crate::DiagnosticCode) -> String {
    diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code() == code)
        .map_or_else(
            || format!("import blocked by diagnostic {}", code.as_str()),
            |diagnostic| diagnostic.message().to_owned(),
        )
}

fn nginx_blockers(report: &crate::nginx::NginxImportReport) -> Vec<ImportBlocker> {
    let mut blockers = Vec::new();
    for blocked in &report.blocked_http_services {
        let origins = blocked
            .servers
            .iter()
            .filter_map(|occurrence| nginx_occurrence_span(report, *occurrence))
            .map(span_evidence)
            .collect::<Vec<_>>();
        for code in &blocked.diagnostic_codes {
            blockers.push(typed_blocker(
                format!("nginx-http:{}:{}", blocked.path, code.as_str()),
                "service",
                *code,
                message_for_code(&report.diagnostics, *code),
                Some(blocked.path.clone()),
                blocked
                    .servers
                    .iter()
                    .map(|occurrence| occurrence.get().to_string())
                    .collect(),
                origins.clone(),
            ));
        }
    }
    for blocked in &report.blocked_rtmp_services {
        let origins: Vec<SpanEvidence> = nginx_occurrence_span(report, blocked.server)
            .into_iter()
            .map(span_evidence)
            .collect();
        for code in &blocked.diagnostic_codes {
            blockers.push(typed_blocker(
                format!("nginx-rtmp:{}:{}", blocked.path, code.as_str()),
                "rtmp_service",
                *code,
                message_for_code(&report.diagnostics, *code),
                Some(blocked.path.clone()),
                vec![blocked.server.get().to_string()],
                origins.clone(),
            ));
        }
    }
    for blocked in &report.blocked_stream_services {
        let origins: Vec<SpanEvidence> = nginx_occurrence_span(report, blocked.server)
            .into_iter()
            .map(span_evidence)
            .collect();
        for code in &blocked.diagnostic_codes {
            blockers.push(typed_blocker(
                format!("nginx-stream:{}:{}", blocked.path, code.as_str()),
                "stream_service",
                *code,
                message_for_code(&report.diagnostics, *code),
                Some(blocked.path.clone()),
                vec![blocked.server.get().to_string()],
                origins.clone(),
            ));
        }
    }
    blockers
}

fn nginx_occurrence_span(
    report: &crate::nginx::NginxImportReport,
    occurrence: crate::nginx::OccurrenceId,
) -> Option<Span> {
    report
        .http_occurrence_ledger
        .iter()
        .find(|decision| decision.occurrence == occurrence)
        .map(|decision| decision.span)
        .or_else(|| {
            report
                .rtmp_occurrence_ledger
                .iter()
                .find(|decision| decision.occurrence == occurrence)
                .map(|decision| decision.span)
        })
        .or_else(|| {
            report
                .stream_occurrence_ledger
                .iter()
                .find(|decision| decision.occurrence == occurrence)
                .map(|decision| decision.span)
        })
}

fn apache_blockers(report: &crate::apache::ApacheImportReport) -> Vec<ImportBlocker> {
    report
        .blocked_virtual_hosts
        .iter()
        .flat_map(|blocked| {
            blocked.diagnostic_codes.iter().map(|code| {
                let mut origins = vec![span_evidence(blocked.origin.span)];
                origins.extend(
                    blocked
                        .origin
                        .include_stack
                        .iter()
                        .map(|frame| span_evidence(frame.directive_span)),
                );
                typed_blocker(
                    format!("apache-vhost:{}:{}", blocked.address, code.as_str()),
                    "virtual_host",
                    *code,
                    message_for_code(&report.diagnostics, *code),
                    Some(blocked.address.to_string()),
                    Vec::new(),
                    origins,
                )
            })
        })
        .collect()
}

#[cfg(unix)]
fn squid_blockers(report: &crate::squid::ImportReport) -> Vec<ImportBlocker> {
    report
        .blocked_capabilities
        .iter()
        .flat_map(|blocked| {
            let scope = snake_case(&format!("{:?}", blocked.kind));
            let origins = blocked
                .occurrences
                .iter()
                .filter_map(|occurrence| {
                    report
                        .effective
                        .ledger
                        .decision(*occurrence)
                        .map(|decision| span_evidence(decision.origin.directive_span))
                })
                .collect::<Vec<_>>();
            blocked.occurrences.iter().map(move |occurrence| {
                typed_blocker(
                    format!("squid-{}:{}", scope, occurrence.get()),
                    "capability",
                    blocked.diagnostic_code,
                    message_for_code(&report.diagnostics, blocked.diagnostic_code),
                    Some(scope.clone()),
                    vec![occurrence.get().to_string()],
                    origins.clone(),
                )
            })
        })
        .collect()
}

fn snake_case(value: &str) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    let mut result = String::with_capacity(value.len() + 4);
    for (index, character) in chars.iter().copied().enumerate() {
        if character.is_ascii_uppercase() && index > 0 {
            let previous = chars[index - 1];
            let next_is_lowercase = chars.get(index + 1).is_some_and(char::is_ascii_lowercase);
            if previous.is_ascii_lowercase()
                || previous.is_ascii_digit()
                || (previous.is_ascii_uppercase() && next_is_lowercase)
            {
                result.push('_');
            }
        }
        result.push(character.to_ascii_lowercase());
    }
    result
}
