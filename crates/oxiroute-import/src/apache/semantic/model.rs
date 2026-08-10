#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectiveOrigin {
    pub occurrence: OccurrenceId,
    pub span: Span,
    pub provenance: Provenance,
}

/// Source-aware origin used by Apache canonical provenance entries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApacheProvenance {
    pub role: crate::ProvenanceRole,
    pub source: crate::SourceId,
    pub path: PathBuf,
    pub line: usize,
    pub span: Span,
    pub include_stack: Vec<IncludeFrame>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApacheResolution {
    pub listens: Vec<EffectiveListen>,
    pub virtual_hosts: Vec<EffectiveVirtualHost>,
    pub balancers: Vec<EffectiveBalancer>,
    pub module_loads: Vec<EffectiveModuleLoad>,
    pub decisions: Vec<OccurrenceDecision>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OccurrenceDecision {
    pub occurrence: OccurrenceId,
    pub parent: Option<OccurrenceId>,
    pub name: Word,
    pub arguments: Vec<Word>,
    pub span: Span,
    pub provenance: Provenance,
    pub disposition: OccurrenceDisposition,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OccurrenceDisposition {
    Resolved,
    Structural,
    Blocking(DiagnosticCode),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveListen {
    pub origin: DirectiveOrigin,
    pub address: SocketAddr,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveVirtualHost {
    pub origin: DirectiveOrigin,
    pub address: SocketAddr,
    pub addresses: Vec<SocketAddr>,
    pub names: Vec<EffectiveServerName>,
    pub proxy_passes: Vec<EffectiveProxyPass>,
    pub tls: EffectiveTls,
    pub preserve_host: bool,
    pub preserve_host_origin: Option<DirectiveOrigin>,
    pub preserve_host_inherited: bool,
    pub blocked: Vec<DiagnosticCode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveServerName {
    pub origin: DirectiveOrigin,
    pub selector: HttpHostSelector,
    pub certificate_name: String,
    pub inherited: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EffectiveTls {
    pub engine_on: bool,
    pub engine_origin: Option<DirectiveOrigin>,
    pub engine_inherited: bool,
    pub certificate_chain: Option<EffectivePath>,
    pub private_key: Option<EffectivePath>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectivePath {
    pub path: PathBuf,
    pub origin: DirectiveOrigin,
    pub inherited: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveProxyPass {
    pub origin: DirectiveOrigin,
    pub path: String,
    pub target: ProxyTarget,
    pub inherited: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProxyTarget {
    Direct {
        scheme: ProxyScheme,
        endpoint: UpstreamEndpoint,
        host: String,
        port: u16,
        target_path: String,
    },
    Balancer {
        name: String,
        target_path: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProxyScheme {
    Http,
    Https,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveBalancer {
    pub origin: DirectiveOrigin,
    pub name: String,
    pub members: Vec<EffectiveBalancerMember>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveBalancerMember {
    pub origin: DirectiveOrigin,
    pub endpoint: UpstreamEndpoint,
    pub host: String,
    pub port: u16,
    pub scheme: ProxyScheme,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveModuleLoad {
    pub origin: DirectiveOrigin,
    pub module: String,
    pub path: String,
}
