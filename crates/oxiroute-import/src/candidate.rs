use oxiroute_config::{
    CacheStore, Certificate, Config, ForwardProxyService, HttpService, L4Service, Listener,
    Management, RtmpService, TlsProfile, UpstreamPool,
};

use crate::Span;

/// Canonical objects that were safely lowered, even when another service blocks finalization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalDraft {
    pub version: u32,
    pub management: Option<Management>,
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
            management: None,
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
            management: self.management.clone(),
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
