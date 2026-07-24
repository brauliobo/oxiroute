mod listener;
mod provenance;
mod report;
mod tls;
mod upstream;

use std::collections::HashMap;

use oxiroute_config::{Certificate, HttpService, Listener, ListenerBind, TlsProfile, UpstreamPool};

use crate::{CanonicalDraft, CanonicalProvenance, Diagnostic, DiagnosticCode};

use super::{DirectiveOrigin, HttpResolution, SourceGraph};

pub use report::{BlockedService, ImportReport, import_http_fragment};

struct Lowerer {
    graph: SourceGraph,
    resolution: HttpResolution,
    diagnostics: Vec<Diagnostic>,
    blocked_services: Vec<BlockedService>,
    certificate_identities: HashMap<tls::CertificateIdentity, tls::CertificateMetadata>,
    draft: CanonicalDraft,
    provenance: Vec<CanonicalProvenance<DirectiveOrigin>>,
}

impl Lowerer {
    fn new(graph: SourceGraph, resolution: HttpResolution, diagnostics: Vec<Diagnostic>) -> Self {
        Self {
            graph,
            resolution,
            diagnostics,
            blocked_services: Vec::new(),
            certificate_identities: HashMap::new(),
            draft: CanonicalDraft::default(),
            provenance: Vec::new(),
        }
    }
}

struct BindBlock {
    bind: Option<ListenerBind>,
    issues: Vec<LowerIssue>,
    candidate: Option<BindCandidate>,
}

struct BindCandidate {
    listener: Listener,
    service: HttpService,
    pools: Vec<PoolCandidate>,
    certificates: Vec<Certificate>,
    tls_profile: Option<TlsProfile>,
    origins: Vec<DirectiveOrigin>,
    route_origins: Vec<Vec<DirectiveOrigin>>,
}

#[derive(Clone)]
struct PoolCandidate {
    pool: UpstreamPool,
    origin: DirectiveOrigin,
    endpoint_origins: Vec<DirectiveOrigin>,
}

struct LowerIssue {
    origin: DirectiveOrigin,
    code: DiagnosticCode,
    message: String,
    emit: bool,
}
