use oxiroute_config_source::ConfigFormat;
use oxiroute_import::{CapabilityProfileMetadata, ImportReportEnvelope, OriginEvidence};
use serde::{Serialize, Serializer};

use super::RedactedConfigView;
use crate::config_coordinator::{
    AuthoredRevision, ConfigDiagnostic, EffectiveRevision, NativeImportSourceDocument,
};

const REDACTED_IMPORT_VALUE: &str = "<redacted>";

pub(crate) struct RedactedImportReport {
    report: ImportReportEnvelope,
    preview: Option<ImportReportPreview>,
}

impl RedactedImportReport {
    pub(crate) fn new(mut report: ImportReportEnvelope) -> Self {
        for root in &mut report.source_graph.roots {
            root.path = root.path.as_ref().map(|_| REDACTED_IMPORT_VALUE.to_owned());
        }
        for source in &mut report.source_graph.sources {
            source.name = format!("source-{}", source.id);
            source.path = source
                .path
                .as_ref()
                .map(|_| REDACTED_IMPORT_VALUE.to_owned());
        }
        for dependency in &mut report.source_graph.dependencies {
            dependency.requested_path = dependency
                .requested_path
                .as_ref()
                .map(|_| REDACTED_IMPORT_VALUE.to_owned());
            dependency.canonical_path = dependency
                .canonical_path
                .as_ref()
                .map(|_| REDACTED_IMPORT_VALUE.to_owned());
        }
        for provenance in &mut report.candidate.provenance {
            for origin in &mut provenance.origins {
                redact_origin(origin);
            }
        }
        for requirement in report
            .requirements
            .deployment
            .iter_mut()
            .chain(report.requirements.activation.iter_mut())
        {
            requirement.values.fill(REDACTED_IMPORT_VALUE.to_owned());
            for origin in &mut requirement.origins {
                redact_origin(origin);
            }
        }
        for overlay in &mut report.overlays {
            overlay.values.fill(REDACTED_IMPORT_VALUE.to_owned());
            if let Some(origin) = &mut overlay.origin {
                redact_origin(origin);
            }
        }
        for blocker in &mut report.blockers {
            REDACTED_IMPORT_VALUE.clone_into(&mut blocker.message);
            blocker.scope = blocker.scope.as_deref().map(redact_scope);
        }
        for diagnostic in &mut report.diagnostics {
            REDACTED_IMPORT_VALUE.clone_into(&mut diagnostic.message);
            diagnostic.help = diagnostic
                .help
                .as_ref()
                .map(|_| REDACTED_IMPORT_VALUE.to_owned());
            for related in &mut diagnostic.related_spans {
                REDACTED_IMPORT_VALUE.clone_into(&mut related.message);
            }
        }

        let preview = report.candidate.config.take().map(|config| {
            let (config, text, _) =
                RedactedConfigView::new(&config, ConfigFormat::Kdl).into_parts();
            report.candidate.config = Some(config);
            ImportReportPreview {
                format: "kdl",
                text,
            }
        });
        Self { report, preview }
    }

    pub(crate) fn preview(&self) -> Option<&ImportReportPreview> {
        if self.report.candidate.finalized && self.report.blockers.is_empty() {
            self.preview.as_ref()
        } else {
            None
        }
    }

    pub(crate) fn summary(&self, index: usize) -> ImportReportSummary {
        let report = &self.report;
        ImportReportSummary {
            index,
            product: report.source.product.clone(),
            version: report.source.version.clone(),
            version_source: report.source.version_source.clone(),
            capability_profile: report.source.capability_profile.clone(),
            status: if !report.blockers.is_empty() {
                "blocked"
            } else if report.candidate.finalized {
                "finalized"
            } else {
                "draft"
            },
            root_count: report.source_graph.roots.len(),
            source_count: report.source_graph.sources.len(),
            dependency_count: report.source_graph.dependencies.len(),
            blocker_count: report.blockers.len(),
            diagnostic_count: report.diagnostics.len(),
            provenance_count: report.candidate.provenance.len(),
            requirement_count: report.requirements.deployment.len()
                + report.requirements.activation.len(),
            overlay_count: report.overlays.len(),
            preview_available: self.preview().is_some(),
        }
    }
}

impl Serialize for RedactedImportReport {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.report.serialize(serializer)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportReportSummary {
    index: usize,
    product: String,
    version: Option<String>,
    version_source: Option<String>,
    capability_profile: CapabilityProfileMetadata,
    status: &'static str,
    root_count: usize,
    source_count: usize,
    dependency_count: usize,
    blocker_count: usize,
    diagnostic_count: usize,
    provenance_count: usize,
    requirement_count: usize,
    overlay_count: usize,
    preview_available: bool,
}

#[derive(Serialize)]
pub(crate) struct ImportReportPreview {
    format: &'static str,
    text: String,
}

#[derive(Serialize)]
pub(crate) struct ImportReportSelection {
    index: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportReportResponse {
    schema_version: u8,
    disk_revision: AuthoredRevision,
    candidate_revision: EffectiveRevision,
    active_revision: EffectiveRevision,
    config_format: ConfigFormat,
    compositional: bool,
    reports: Vec<ImportReportSummary>,
    selection: Option<ImportReportSelection>,
    report: Option<RedactedImportReport>,
    preview: Option<ImportReportPreview>,
    diagnostics: Vec<ConfigDiagnostic>,
}

impl ImportReportResponse {
    pub(crate) fn new(
        source: &NativeImportSourceDocument,
        active_revision: EffectiveRevision,
        reports: Vec<ImportReportSummary>,
        selection: Option<usize>,
        report: Option<RedactedImportReport>,
    ) -> Self {
        let preview = report
            .as_ref()
            .and_then(RedactedImportReport::preview)
            .map(|preview| ImportReportPreview {
                format: preview.format,
                text: preview.text.clone(),
            });
        Self {
            schema_version: 1,
            disk_revision: source.disk_revision.clone(),
            candidate_revision: source.candidate_revision.clone(),
            active_revision,
            config_format: source.format,
            compositional: source.compositional,
            reports,
            selection: selection.map(|index| ImportReportSelection { index }),
            report,
            preview,
            diagnostics: Vec::new(),
        }
    }
}

fn redact_origin(origin: &mut OriginEvidence) {
    origin.path = origin
        .path
        .as_ref()
        .map(|_| REDACTED_IMPORT_VALUE.to_owned());
}

fn redact_scope(scope: &str) -> String {
    if scope.contains('/') || scope.contains('\\') {
        REDACTED_IMPORT_VALUE.to_owned()
    } else {
        scope.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxiroute_import::evidence::{RelatedSpanEvidence, SourceMetadataEvidence};
    use oxiroute_import::{
        ByteRangeEvidence, CandidateDraftEvidence, CandidateEvidence, CanonicalProvenanceEvidence,
        CapabilityProfileMetadata, DependencyEvidence, DiagnosticEvidence, ImportBlocker,
        ImportSourceMetadata, OverlayEvidence, RequirementEvidence, RequirementsEvidence,
        SourceGraphEvidence, SourceReference, SourceRootEvidence, SpanEvidence,
    };
    use serde_json::json;

    fn origin(path: &str) -> OriginEvidence {
        OriginEvidence {
            role: None,
            source_id: 7,
            range: Some(ByteRangeEvidence { start: 0, end: 1 }),
            path: Some(path.into()),
            line: Some(1),
            include_stack: Vec::new(),
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the fixture spells out one complete import-report wire shape"
    )]
    fn report() -> ImportReportEnvelope {
        ImportReportEnvelope {
            schema_version: 1,
            source: ImportSourceMetadata {
                product: "native".into(),
                version: None,
                version_source: None,
                capability_profile: CapabilityProfileMetadata {
                    id: "strict".into(),
                    version: 1,
                },
            },
            source_graph: SourceGraphEvidence {
                roots: vec![SourceRootEvidence {
                    ordinal: 0,
                    path: Some("/secret/root.conf".into()),
                    source_ids: vec![7],
                    outcome: None,
                }],
                sources: vec![SourceReference {
                    id: 7,
                    name: "root.conf".into(),
                    path: Some("/secret/root.conf".into()),
                    byte_length: 10,
                    fingerprint_sha256: "fingerprint".into(),
                }],
                dependencies: vec![DependencyEvidence {
                    source_id: 7,
                    target_source_id: None,
                    kind: "include".into(),
                    requested_path: Some("token=private".into()),
                    canonical_path: Some("/secret/include.conf".into()),
                    optional: None,
                    status: "missing".into(),
                    span: Some(SpanEvidence {
                        source_id: 7,
                        range: ByteRangeEvidence { start: 0, end: 1 },
                    }),
                    failure_code: None,
                    fingerprint_sha256: None,
                    truncated: false,
                }],
                dependencies_complete: true,
                snapshot_stable: Some(true),
            },
            source_metadata: SourceMetadataEvidence {
                environment_fingerprint_sha256: None,
                inactive_sources: Vec::new(),
                original_source_ids: vec![7],
                source_maps: Vec::new(),
            },
            candidate: CandidateEvidence {
                finalized: false,
                config: None,
                draft: CandidateDraftEvidence {
                    version: 1,
                    max_connections: None,
                    management: false,
                    stats: false,
                    certificates: 0,
                    tls_profiles: 0,
                    listeners: 0,
                    upstream_pools: 0,
                    http_services: 0,
                    cache_stores: 0,
                    forward_proxy_services: 0,
                    rtmp_services: 0,
                    l4_services: 0,
                },
                provenance: vec![CanonicalProvenanceEvidence {
                    path: "/listeners".into(),
                    origins: vec![origin("/secret/root.conf")],
                }],
            },
            blockers: vec![ImportBlocker {
                id: "blocker".into(),
                kind: "source".into(),
                code: "E_SOURCE".into(),
                message: "private blocker message /secret/blocker.conf".into(),
                scope: Some("/secret/root.conf".into()),
                occurrence_ids: Vec::new(),
                origins: Vec::new(),
            }],
            requirements: RequirementsEvidence {
                deployment: vec![RequirementEvidence {
                    kind: "secret".into(),
                    directive: "token".into(),
                    values: vec!["private-token".into()],
                    origins: vec![origin("/secret/root.conf")],
                    equivalent_runtime_endpoint: None,
                }],
                activation: Vec::new(),
            },
            overlays: vec![OverlayEvidence {
                id: "overlay".into(),
                kind: "secret".into(),
                origin: Some(origin("/secret/root.conf")),
                redacted_evidence: true,
                values: vec!["private-token".into()],
                satisfied: false,
            }],
            diagnostics: vec![DiagnosticEvidence {
                code: "E_SOURCE".into(),
                severity: "error".into(),
                stage: "load".into(),
                message: "private diagnostic message /secret/diagnostic.conf".into(),
                primary_span: None,
                include_stack: Vec::new(),
                related_spans: vec![RelatedSpanEvidence {
                    span: SpanEvidence {
                        source_id: 7,
                        range: ByteRangeEvidence { start: 2, end: 3 },
                    },
                    message: "private related message /secret/related.conf".into(),
                }],
                help: Some("read /secret/help.conf".into()),
            }],
            #[cfg(unix)]
            capabilities: None,
        }
    }

    #[test]
    fn redacted_report_and_summary_have_exact_safe_json() {
        let report = RedactedImportReport::new(report());
        let summary = serde_json::to_value(report.summary(3)).unwrap();
        assert_eq!(
            summary,
            json!({
                "index": 3,
                "product": "native",
                "version": null,
                "versionSource": null,
                "capabilityProfile": { "id": "strict", "version": 1 },
                "status": "blocked",
                "rootCount": 1,
                "sourceCount": 1,
                "dependencyCount": 1,
                "blockerCount": 1,
                "diagnosticCount": 1,
                "provenanceCount": 1,
                "requirementCount": 1,
                "overlayCount": 1,
                "previewAvailable": false,
            })
        );

        let value = serde_json::to_value(report).unwrap();
        assert_eq!(value["sourceGraph"]["roots"][0]["path"], "<redacted>");
        assert_eq!(value["sourceGraph"]["sources"][0]["name"], "source-7");
        assert_eq!(value["blockers"][0]["message"], "<redacted>");
        assert_eq!(value["diagnostics"][0]["message"], "<redacted>");
        assert_eq!(value["diagnostics"][0]["help"], "<redacted>");
        assert_eq!(
            value["diagnostics"][0]["relatedSpans"][0]["message"],
            "<redacted>"
        );
        assert_eq!(
            value["requirements"]["deployment"][0]["values"][0],
            "<redacted>"
        );
        let wire = value.to_string();
        for private in ["/secret/root.conf", "/secret/include.conf", "private-token"] {
            assert!(!wire.contains(private));
        }
    }
}
