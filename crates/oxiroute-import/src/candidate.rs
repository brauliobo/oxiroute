use std::{
    collections::{HashMap, HashSet},
    fmt,
    hash::Hash,
};

use oxiroute_config::{ConfigDraft, ConfigError, ValidatedConfig};

use crate::{ByteRange, SourceFile, SourceId, Span};

/// Deployment-owned behavior retained by an import without claiming runtime equivalence.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DeploymentRequirementKind {
    ProcessUser,
    ProcessGroup,
    Chroot,
    Daemonization,
    WorkerModel,
    ModuleLoad,
    EventCapacity,
    LogTransport,
    ErrorLogging,
    SocketPermissions,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeploymentRequirement<Origin> {
    pub kind: DeploymentRequirementKind,
    pub directive: String,
    pub value: Vec<String>,
    pub origin: Origin,
}

/// Endpoint behavior that requires an explicit operator decision before activation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ActivationRequirementKind {
    StatisticsEndpoint,
    PrometheusExporter,
    NativeTlsPolicy,
    UpstreamConnectivity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivationRequirement<Origin> {
    pub kind: ActivationRequirementKind,
    pub directive: String,
    pub origin: Origin,
    /// Always false for native facilities that `OxiRoute` does not implement equivalently.
    pub equivalent_runtime_endpoint: bool,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OperationalOverlayKind {
    BearerTokenFile,
    CertificateMaterial,
    DefaultErrorPageMigration,
    HostTimezone,
    OneRequestPerConnection,
    PrometheusMigration,
    RecordingRootMigration,
    StructuredAccessLogMigration,
    HtpasswdFile,
    UpstreamTlsPolicy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationalOverlayRequirement<Origin> {
    pub id: String,
    pub kind: OperationalOverlayKind,
    pub origin: Option<Origin>,
    /// True when the sanitized source contained only a redaction marker.
    pub redacted_evidence: bool,
    /// Non-secret material supplied by the native source or explicit import options.
    pub values: Vec<String>,
    /// True only when this overlay was consumed by one canonical lowering identity.
    pub satisfied: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InactiveSource {
    pub condition: String,
    pub origin: Span,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SourceImportMetadata {
    pub environment_fingerprint_sha256: Option<String>,
    pub inactive_sources: Vec<InactiveSource>,
    pub original_sources: Vec<SourceFile>,
    pub source_maps: Vec<SourceSpanMap>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceMapSegment {
    pub generated: ByteRange,
    pub original: ByteRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceSpanMap {
    pub source: SourceId,
    pub segments: Vec<SourceMapSegment>,
}

impl SourceSpanMap {
    /// Translates a generated preprocessor span to the smallest covering original-source span.
    #[must_use]
    pub fn translate(&self, span: Span) -> Option<Span> {
        if span.source() != self.source {
            return None;
        }
        let range = span.range();
        if range.is_empty() {
            let offset = self.translate_boundary(range.start(), true)?;
            return Some(Span::new(self.source, ByteRange::new(offset, offset)));
        }
        let first = self.segments.iter().find(|segment| {
            segment.generated.end() > range.start() && segment.generated.start() < range.end()
        })?;
        let last = self.segments.iter().rev().find(|segment| {
            segment.generated.end() > range.start() && segment.generated.start() < range.end()
        })?;
        let start = translate_segment_offset(*first, range.start(), false);
        let end = translate_segment_offset(*last, range.end(), true);
        (start <= end).then(|| Span::new(self.source, ByteRange::new(start, end)))
    }

    fn translate_boundary(&self, offset: usize, end: bool) -> Option<usize> {
        self.segments
            .iter()
            .find(|segment| {
                segment.generated.contains(offset) || (end && segment.generated.end() == offset)
            })
            .map(|segment| translate_segment_offset(*segment, offset, end))
            .or_else(|| (offset == 0 && self.segments.is_empty()).then_some(0))
    }
}

fn translate_segment_offset(segment: SourceMapSegment, offset: usize, end: bool) -> usize {
    if segment.generated.len() == segment.original.len() {
        return segment.original.start()
            + offset
                .clamp(segment.generated.start(), segment.generated.end())
                .saturating_sub(segment.generated.start());
    }
    if end {
        segment.original.end()
    } else {
        segment.original.start()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) enum CanonicalCandidateState {
    Blocked(ConfigDraft),
    Validated(ValidatedConfig),
}

impl CanonicalCandidateState {
    #[must_use]
    pub(crate) const fn draft(&self) -> &ConfigDraft {
        match self {
            Self::Blocked(config) => config,
            Self::Validated(config) => config.as_draft(),
        }
    }

    #[must_use]
    pub(crate) const fn validated(&self) -> Option<&ValidatedConfig> {
        match self {
            Self::Blocked(_) => None,
            Self::Validated(config) => Some(config),
        }
    }

    #[must_use]
    pub(crate) const fn summary(&self) -> CanonicalCandidateSummary {
        CanonicalCandidateSummary::from_config(self.draft())
    }
}

impl fmt::Debug for CanonicalCandidateState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CanonicalCandidateState")
            .field("validated", &self.validated().is_some())
            .field("summary", &self.summary())
            .finish()
    }
}

/// Report-safe counts and presence flags for a lowered canonical candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CanonicalCandidateSummary {
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

impl CanonicalCandidateSummary {
    const fn from_config(config: &ConfigDraft) -> Self {
        Self {
            version: config.version,
            max_connections: config.max_connections,
            management: config.management.is_some(),
            stats: config.stats.is_some(),
            certificates: config.certificates.len(),
            tls_profiles: config.tls_profiles.len(),
            listeners: config.listeners.len(),
            upstream_pools: config.upstream_pools.len(),
            http_services: config.http_services.len(),
            cache_stores: config.cache_stores.len(),
            forward_proxy_services: config.forward_proxy_services.len(),
            rtmp_services: config.rtmp_services.len(),
            l4_services: config.l4_services.len(),
        }
    }
}

pub(crate) fn empty_config() -> ConfigDraft {
    ConfigDraft {
        version: 1,
        max_connections: None,
        management: None,
        stats: None,
        certificates: Vec::new(),
        tls_profiles: Vec::new(),
        listeners: Vec::new(),
        upstream_pools: Vec::new(),
        http_services: Vec::new(),
        cache_stores: Vec::new(),
        forward_proxy_services: Vec::new(),
        rtmp_services: Vec::new(),
        l4_services: Vec::new(),
    }
}

pub(crate) fn finalize_candidate(
    draft: &ConfigDraft,
    eligible: bool,
) -> Result<Option<ValidatedConfig>, ConfigError> {
    if eligible {
        draft.clone().validate().map(Some)
    } else {
        Ok(None)
    }
}

/// Native origins that produced one stable canonical JSON-pointer path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalProvenance<Origin> {
    pub path: String,
    pub origins: Vec<Origin>,
}

#[derive(Clone, Copy)]
pub(crate) enum EmptyOriginPolicy {
    Discard,
    Preserve,
    Require,
}

#[derive(Clone)]
pub(crate) struct CanonicalProvenanceLedger<Origin> {
    entries: Vec<CanonicalProvenance<Origin>>,
    path_indexes: HashMap<String, usize>,
    empty_origin_policy: EmptyOriginPolicy,
}

impl<Origin> CanonicalProvenanceLedger<Origin> {
    pub(crate) fn new(empty_origin_policy: EmptyOriginPolicy) -> Self {
        Self {
            entries: Vec::new(),
            path_indexes: HashMap::new(),
            empty_origin_policy,
        }
    }

    pub(crate) fn record<Identity: Ord>(
        &mut self,
        path: String,
        mut origins: Vec<Origin>,
        origin_identity: impl Fn(&Origin) -> Identity,
    ) {
        origins.sort_by_key(&origin_identity);
        origins.dedup_by(|left, right| origin_identity(left) == origin_identity(right));
        if origins.is_empty() {
            match self.empty_origin_policy {
                EmptyOriginPolicy::Discard => return,
                EmptyOriginPolicy::Preserve => {}
                EmptyOriginPolicy::Require => {
                    panic!("canonical field lacks source provenance: {path}")
                }
            }
        }
        if let Some(index) = self.path_indexes.get(&path).copied() {
            let existing = &mut self.entries[index].origins;
            existing.extend(origins);
            existing.sort_by_key(&origin_identity);
            existing.dedup_by(|left, right| origin_identity(left) == origin_identity(right));
            return;
        }
        self.path_indexes.insert(path.clone(), self.entries.len());
        self.entries.push(CanonicalProvenance { path, origins });
    }

    pub(crate) fn record_in_order<Identity: Eq + Hash>(
        &mut self,
        path: String,
        mut origins: Vec<Origin>,
        origin_identity: impl Fn(&Origin) -> Identity,
    ) {
        let mut seen = HashSet::new();
        origins.retain(|origin| seen.insert(origin_identity(origin)));
        if let Some(index) = self.path_indexes.get(&path).copied() {
            seen.extend(self.entries[index].origins.iter().map(&origin_identity));
            origins.retain(|origin| seen.insert(origin_identity(origin)));
            self.entries[index].origins.extend(origins);
            return;
        }
        if origins.is_empty() && matches!(self.empty_origin_policy, EmptyOriginPolicy::Discard) {
            return;
        }
        assert!(
            !origins.is_empty() || !matches!(self.empty_origin_policy, EmptyOriginPolicy::Require),
            "canonical field lacks source provenance: {path}"
        );
        self.path_indexes.insert(path.clone(), self.entries.len());
        self.entries.push(CanonicalProvenance { path, origins });
    }

    pub(crate) fn into_entries(self) -> Vec<CanonicalProvenance<Origin>> {
        self.entries
    }

    pub(crate) fn first(&self) -> Option<&CanonicalProvenance<Origin>> {
        self.entries.first()
    }
}

/// Shared canonical provenance and validation result used by product import reports.
///
/// Downstream crates cannot extract the importer-owned blocked `ConfigDraft` from a candidate:
///
/// ```compile_fail
/// use oxiroute_config::ConfigDraft;
/// use oxiroute_import::CanonicalCandidate;
///
/// fn extract_importer_owned_blocked_config<Origin>(candidate: &CanonicalCandidate<Origin>) -> &ConfigDraft {
///     candidate.draft()
/// }
/// ```
///
/// Downstream crates also cannot destructure the internal candidate state to strip blockers:
///
/// ```compile_fail
/// use oxiroute_config::ConfigDraft;
/// use oxiroute_import::CanonicalCandidateState;
///
/// fn destructure_candidate_state(state: CanonicalCandidateState) -> ConfigDraft {
///     match state {
///         CanonicalCandidateState::Blocked(config) => config,
///         CanonicalCandidateState::Validated(config) => config.to_draft(),
///     }
/// }
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalCandidate<Origin> {
    pub provenance: Vec<CanonicalProvenance<Origin>>,
    pub deployment_requirements: Vec<DeploymentRequirement<Origin>>,
    pub activation_requirements: Vec<ActivationRequirement<Origin>>,
    pub operational_overlays: Vec<OperationalOverlayRequirement<Origin>>,
    pub source_metadata: SourceImportMetadata,
    state: CanonicalCandidateState,
}

impl<Origin> CanonicalCandidate<Origin> {
    pub(crate) fn new(
        state: CanonicalCandidateState,
        provenance: Vec<CanonicalProvenance<Origin>>,
        deployment_requirements: Vec<DeploymentRequirement<Origin>>,
        activation_requirements: Vec<ActivationRequirement<Origin>>,
        operational_overlays: Vec<OperationalOverlayRequirement<Origin>>,
        source_metadata: SourceImportMetadata,
    ) -> Self {
        Self {
            provenance,
            deployment_requirements,
            activation_requirements,
            operational_overlays,
            source_metadata,
            state,
        }
    }

    /// Returns the validated configuration capability when finalization succeeded.
    #[must_use]
    pub const fn validated(&self) -> Option<&ValidatedConfig> {
        self.state.validated()
    }

    /// Returns report-safe candidate counts and flags without exposing blocked configuration.
    #[must_use]
    pub const fn summary(&self) -> CanonicalCandidateSummary {
        self.state.summary()
    }
}

/// Why an `HAProxy` source span contributes to a canonical object.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ProvenanceRole {
    Declaration,
    Value,
    Inherited,
    Reference,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ProvenanceSpan {
    pub role: ProvenanceRole,
    pub span: Span,
}
