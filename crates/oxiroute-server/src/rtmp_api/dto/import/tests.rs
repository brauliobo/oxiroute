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
