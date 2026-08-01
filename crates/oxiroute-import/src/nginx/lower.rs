mod listener;
mod provenance;
mod report;
mod tls;
mod upstream;

use std::cell::{Cell, RefCell};
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
    default_access_log_path: Option<PathBuf>,
    default_error_server: Option<String>,
    x_accel_controls_absent: bool,
    used_upstream_tls_overlays: RefCell<HashSet<Vec<u8>>>,
    used_bearer_token_overlays: RefCell<HashSet<Vec<u8>>>,
    used_certificate_overlays: RefCell<HashSet<super::OccurrenceId>>,
    used_htpasswd_overlays: RefCell<HashSet<super::OccurrenceId>>,
    used_default_access_log_overlay: bool,
    used_default_error_overlay: Cell<bool>,
}

impl Lowerer {
    fn new(
        graph: SourceGraph,
        resolution: HttpResolution,
        diagnostics: Vec<Diagnostic>,
        upstream_tls_overlays: HashMap<Vec<u8>, UpstreamTls>,
        bearer_token_overlays: HashMap<Vec<u8>, PathBuf>,
        default_access_log_path: Option<PathBuf>,
        default_error_server: Option<String>,
        x_accel_controls_absent: bool,
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
            default_access_log_path,
            default_error_server,
            x_accel_controls_absent,
            used_upstream_tls_overlays: RefCell::new(HashSet::new()),
            used_bearer_token_overlays: RefCell::new(HashSet::new()),
            used_certificate_overlays: RefCell::new(HashSet::new()),
            used_htpasswd_overlays: RefCell::new(HashSet::new()),
            used_default_access_log_overlay: false,
            used_default_error_overlay: Cell::new(false),
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
    gzip_origins: GzipOrigins,
    route_origins: Vec<Vec<DirectiveOrigin>>,
}

#[derive(Default)]
struct GzipOrigins {
    gzip: Vec<DirectiveOrigin>,
    level: Vec<DirectiveOrigin>,
    content_types: Vec<DirectiveOrigin>,
    min_length_bytes: Vec<DirectiveOrigin>,
    min_http_version: Vec<DirectiveOrigin>,
    disable_on_via: Vec<DirectiveOrigin>,
    vary: Vec<DirectiveOrigin>,
}

impl GzipOrigins {
    fn extend(&mut self, other: Self) {
        self.gzip.extend(other.gzip);
        self.level.extend(other.level);
        self.content_types.extend(other.content_types);
        self.min_length_bytes.extend(other.min_length_bytes);
        self.min_http_version.extend(other.min_http_version);
        self.disable_on_via.extend(other.disable_on_via);
        self.vary.extend(other.vary);
    }

    fn all(&self) -> Vec<DirectiveOrigin> {
        let mut origins = self.gzip.clone();
        origins.extend(self.level.clone());
        origins.extend(self.content_types.clone());
        origins.extend(self.min_length_bytes.clone());
        origins.extend(self.min_http_version.clone());
        origins.extend(self.disable_on_via.clone());
        origins.extend(self.vary.clone());
        origins
    }
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
