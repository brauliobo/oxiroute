use oxiroute_config_source::ConfigFormat;
use oxiroute_import::{CapabilityProfileMetadata, ImportReportEnvelope, OriginEvidence};
use serde::{Serialize, Serializer};

use super::RedactedConfigView;
use crate::config::coordinator::{
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
mod tests;
