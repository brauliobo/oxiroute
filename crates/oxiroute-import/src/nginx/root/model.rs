#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NginxImportReport {
    pub source_graph: SourceGraph,
    pub root_occurrence_ledger: Vec<RootOccurrenceDecision>,
    pub http_occurrence_ledger: Vec<OccurrenceDecision>,
    pub rtmp_occurrence_ledger: Vec<OccurrenceDecision>,
    pub stream_occurrence_ledger: Vec<OccurrenceDecision>,
    pub diagnostics: Vec<Diagnostic>,
    pub blocked_http_services: Vec<BlockedService>,
    pub blocked_rtmp_services: Vec<BlockedRtmpService>,
    pub blocked_stream_services: Vec<BlockedStreamService>,
    pub candidate: CanonicalCandidate<DirectiveOrigin>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootOccurrenceDecision {
    pub occurrence: OccurrenceId,
    pub parent: Option<OccurrenceId>,
    pub name: NginxValue,
    pub arguments: Vec<NginxValue>,
    pub provenance: Provenance,
    pub disposition: RootOccurrenceDisposition,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RootOccurrenceDisposition {
    Http,
    Rtmp,
    Stream,
    Deployment(Vec<DeploymentRequirementKind>),
    Activation(Vec<ActivationRequirementKind>),
    OperationalOverlay(Vec<OperationalOverlayKind>),
    Structural,
    Blocking(crate::DiagnosticCode),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NginxUpstreamTlsOverlay {
    pub authority: String,
    pub tls: UpstreamTls,
    pub require_connectivity_activation: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NginxBearerTokenOverlay {
    pub server_name: String,
    pub token_file_path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NginxHostTimezoneOverlay {
    pub timezone: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NginxDefaultAccessLogOverlay {
    pub path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NginxRecordingRootOverlay {
    pub path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NginxDefaultErrorPageOverlay {
    pub server: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NginxImportOptions {
    pub upstream_tls: Vec<NginxUpstreamTlsOverlay>,
    pub bearer_tokens: Vec<NginxBearerTokenOverlay>,
    pub host_timezones: Vec<NginxHostTimezoneOverlay>,
    pub default_access_log: Option<NginxDefaultAccessLogOverlay>,
    pub recording_root: Option<NginxRecordingRootOverlay>,
    pub default_error_page: Option<NginxDefaultErrorPageOverlay>,
    pub x_accel_controls_absent: bool,
}

#[derive(Default)]
pub(super) struct OverlayOrigins {
    pub(super) upstream_tls: HashMap<Vec<u8>, DirectiveOrigin>,
    pub(super) bearer_tokens: HashMap<Vec<u8>, DirectiveOrigin>,
}

impl NginxImportReport {
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity() == Severity::Error)
    }
}
