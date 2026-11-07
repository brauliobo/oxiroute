#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) const MAX_UPSTREAM_WEIGHT: u16 = 100;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpResolution {
    pub http_blocks: Vec<EffectiveHttp>,
    /// One entry for every bounded expanded occurrence, in expansion order.
    pub decisions: Vec<OccurrenceDecision>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OccurrenceDecision {
    pub occurrence: OccurrenceId,
    pub parent: Option<OccurrenceId>,
    pub name: NginxValue,
    pub arguments: Vec<NginxValue>,
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
pub struct DirectiveOrigin {
    pub occurrence: OccurrenceId,
    pub span: Span,
    pub provenance: Provenance,
}

/// Escape-normalized and exact source bytes for one nginx word.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NginxValue {
    pub value: Vec<u8>,
    pub raw: Vec<u8>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveHttp {
    pub origin: DirectiveOrigin,
    pub declaration_order: Vec<HttpDeclaration>,
    pub upstreams: Vec<EffectiveUpstream>,
    pub servers: Vec<EffectiveServer>,
    pub binds: Vec<EffectiveBind>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HttpDeclaration {
    Upstream(OccurrenceId),
    Server(OccurrenceId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveUpstream {
    pub origin: DirectiveOrigin,
    pub name: Option<NginxValue>,
    pub servers: Vec<EffectiveUpstreamServer>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveUpstreamServer {
    pub origin: DirectiveOrigin,
    pub address: Option<NginxValue>,
    pub endpoint: Option<StaticEndpoint>,
    pub weight: Option<EffectiveUpstreamWeight>,
    pub parameters: Vec<NginxValue>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveUpstreamWeight {
    pub value: u16,
    pub origin: DirectiveOrigin,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum StaticEndpoint {
    Socket { address: SocketAddr },
    Dns { host: String, port: u16 },
    Unix { path: PathBuf },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveServer {
    pub origin: DirectiveOrigin,
    pub declaration_order: Vec<ServerDeclaration>,
    pub listens: Vec<EffectiveListen>,
    pub server_names: Vec<EffectiveServerName>,
    pub locations: Vec<EffectiveLocation>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServerDeclaration {
    Listen(OccurrenceId),
    ServerName(OccurrenceId),
    Location(OccurrenceId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveListen {
    pub origin: DirectiveOrigin,
    pub value: Option<NginxValue>,
    pub endpoint: Option<ListenEndpoint>,
    pub options: Vec<NginxValue>,
    pub default_server: bool,
    pub implicit: bool,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ListenEndpoint {
    Socket { address: Vec<u8>, port: u16 },
    Unix { path: PathBuf },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveBind {
    pub endpoint: ListenEndpoint,
    /// Virtual servers in nginx declaration order.
    pub servers: Vec<OccurrenceId>,
    pub default_server: OccurrenceId,
    pub default_selection: DefaultServerSelection,
    pub names: Vec<BoundServerName>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DefaultServerSelection {
    First,
    Explicit { listen: OccurrenceId },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundServerName {
    pub server: OccurrenceId,
    pub name: EffectiveServerName,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveServerName {
    pub origin: DirectiveOrigin,
    pub value: NginxValue,
    pub normalized: Vec<u8>,
    pub kind: ServerNameKind,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ServerNameKind {
    Exact,
    LeadingWildcard,
    LeadingWildcardAndExact,
    TrailingWildcard,
    Regex,
    Variable,
    Invalid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveLocation {
    pub origin: DirectiveOrigin,
    pub modifier: Option<NginxValue>,
    pub path: Option<NginxValue>,
    pub kind: LocationKind,
    pub proxy_pass: Option<EffectiveProxyPass>,
    pub proxy_pass_inherited: bool,
    pub children: Vec<Self>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LocationKind {
    Exact,
    Prefix,
    PrefixNoRegex,
    Regex,
    RegexInsensitive,
    Named,
    Variable,
    Invalid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveProxyPass {
    pub origin: DirectiveOrigin,
    pub value: NginxValue,
    pub scheme: ProxyPassScheme,
    pub authority: Vec<u8>,
    /// `None` preserves nginx's no-URI replacement form; `Some` preserves the exact replacement.
    pub replacement_uri: Option<Vec<u8>>,
    pub upstream: UpstreamReference,
    pub direct_endpoint: Option<StaticEndpoint>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProxyPassScheme {
    Http,
    Https,
    Downstream,
    Unsupported(Vec<u8>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpstreamReference {
    Resolved(OccurrenceId),
    Direct,
    Unresolved,
    Variable,
}
