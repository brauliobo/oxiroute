use oxiroute_config::{
    CacheStore, Certificate, Config, ForwardProxyService, HttpService, L4Service, Listener,
    Management, RtmpService, Stats, TlsProfile, UpstreamPool,
};

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
    HostTimezone,
    OneRequestPerConnection,
    PrometheusMigration,
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

/// Canonical objects that were safely lowered, even when another service blocks finalization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalDraft {
    pub version: u32,
    pub max_connections: Option<u64>,
    pub management: Option<Management>,
    pub stats: Option<Stats>,
    pub certificates: Vec<Certificate>,
    pub tls_profiles: Vec<TlsProfile>,
    pub listeners: Vec<Listener>,
    pub upstream_pools: Vec<UpstreamPool>,
    pub http_services: Vec<HttpService>,
    pub cache_stores: Vec<CacheStore>,
    pub forward_proxy_services: Vec<ForwardProxyService>,
    pub rtmp_services: Vec<RtmpService>,
    pub l4_services: Vec<L4Service>,
}

impl Default for CanonicalDraft {
    fn default() -> Self {
        Self {
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
}

impl CanonicalDraft {
    #[must_use]
    pub(crate) fn to_config(&self) -> Config {
        Config {
            version: self.version,
            max_connections: self.max_connections,
            management: self.management.clone(),
            stats: self.stats.clone(),
            certificates: self.certificates.clone(),
            tls_profiles: self.tls_profiles.clone(),
            listeners: self.listeners.clone(),
            upstream_pools: self.upstream_pools.clone(),
            http_services: self.http_services.clone(),
            cache_stores: self.cache_stores.clone(),
            forward_proxy_services: self.forward_proxy_services.clone(),
            rtmp_services: self.rtmp_services.clone(),
            l4_services: self.l4_services.clone(),
        }
    }
}

/// Native origins that produced one stable canonical JSON-pointer path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalProvenance<Origin> {
    pub path: String,
    pub origins: Vec<Origin>,
}

/// Shared canonical draft/provenance/finalization result used by product import reports.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalCandidate<Origin> {
    pub draft: CanonicalDraft,
    pub provenance: Vec<CanonicalProvenance<Origin>>,
    pub deployment_requirements: Vec<DeploymentRequirement<Origin>>,
    pub activation_requirements: Vec<ActivationRequirement<Origin>>,
    pub operational_overlays: Vec<OperationalOverlayRequirement<Origin>>,
    pub source_metadata: SourceImportMetadata,
    pub config: Option<Config>,
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
