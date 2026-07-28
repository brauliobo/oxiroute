mod listener;
mod provenance;
mod report;
mod tls;
mod upstream;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use oxiroute_config::{
    Certificate, HttpService, Listener, ListenerBind, TlsProfile, UpstreamPool, UpstreamTls,
};

use crate::{CanonicalDraft, CanonicalProvenance, Diagnostic, DiagnosticCode};

use super::{DirectiveOrigin, HttpResolution, SourceGraph};

pub(super) use report::lower_http_root_with_overlays;
pub use report::{BlockedService, ImportReport, import_http_fragment};

struct Lowerer {
    graph: SourceGraph,
    resolution: HttpResolution,
    diagnostics: Vec<Diagnostic>,
    blocked_services: Vec<BlockedService>,
    certificate_identities: HashMap<tls::CertificateIdentity, tls::CertificateMetadata>,
    draft: CanonicalDraft,
    provenance: Vec<CanonicalProvenance<DirectiveOrigin>>,
    upstream_tls_overlays: HashMap<Vec<u8>, UpstreamTls>,
    bearer_token_overlays: HashMap<Vec<u8>, PathBuf>,
    used_upstream_tls_overlays: RefCell<HashSet<Vec<u8>>>,
    used_bearer_token_overlays: RefCell<HashSet<Vec<u8>>>,
    used_certificate_overlays: RefCell<HashSet<super::OccurrenceId>>,
    used_htpasswd_overlays: RefCell<HashSet<super::OccurrenceId>>,
}

impl Lowerer {
    fn new(
        graph: SourceGraph,
        resolution: HttpResolution,
        diagnostics: Vec<Diagnostic>,
        upstream_tls_overlays: HashMap<Vec<u8>, UpstreamTls>,
        bearer_token_overlays: HashMap<Vec<u8>, PathBuf>,
    ) -> Self {
        Self {
            graph,
            resolution,
            diagnostics,
            blocked_services: Vec::new(),
            certificate_identities: HashMap::new(),
            draft: CanonicalDraft::default(),
            provenance: Vec::new(),
            upstream_tls_overlays,
            bearer_token_overlays,
            used_upstream_tls_overlays: RefCell::new(HashSet::new()),
            used_bearer_token_overlays: RefCell::new(HashSet::new()),
            used_certificate_overlays: RefCell::new(HashSet::new()),
            used_htpasswd_overlays: RefCell::new(HashSet::new()),
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
