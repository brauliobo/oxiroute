#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) const NGINX_DEFAULT_PROXY_CONNECT_TIMEOUT_MS: u64 = 60_000;
pub(super) const NGINX_DEFAULT_PROXY_TIMEOUT_MS: u64 = 600_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamResolution {
    pub stream_blocks: Vec<EffectiveStream>,
    pub decisions: Vec<OccurrenceDecision>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveStream {
    pub origin: DirectiveOrigin,
    pub declaration_order: Vec<StreamDeclaration>,
    pub upstreams: Vec<EffectiveStreamUpstream>,
    pub servers: Vec<EffectiveStreamServer>,
    pub connect_timeout_ms: u64,
    pub connect_timeout_origin: Option<DirectiveOrigin>,
    pub idle_timeout_ms: u64,
    pub idle_timeout_origin: Option<DirectiveOrigin>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamDeclaration {
    Upstream(OccurrenceId),
    Server(OccurrenceId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveStreamUpstream {
    pub origin: DirectiveOrigin,
    pub name: Option<NginxValue>,
    pub servers: Vec<EffectiveStreamUpstreamServer>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveStreamUpstreamServer {
    pub origin: DirectiveOrigin,
    pub address: Option<NginxValue>,
    pub endpoint: Option<StaticEndpoint>,
    pub parameters: Vec<NginxValue>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveStreamServer {
    pub origin: DirectiveOrigin,
    pub listens: Vec<EffectiveStreamListen>,
    pub proxy_pass: Option<EffectiveStreamProxyPass>,
    pub connect_timeout_ms: Option<u64>,
    pub connect_timeout_origin: Option<DirectiveOrigin>,
    pub idle_timeout_ms: Option<u64>,
    pub idle_timeout_origin: Option<DirectiveOrigin>,
    pub proxy_protocol: bool,
    pub proxy_protocol_origin: Option<DirectiveOrigin>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveStreamListen {
    pub origin: DirectiveOrigin,
    pub value: Option<NginxValue>,
    pub endpoint: Option<ListenEndpoint>,
    pub options: Vec<NginxValue>,
    pub default_server: bool,
    pub proxy_protocol: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveStreamProxyPass {
    pub origin: DirectiveOrigin,
    pub value: NginxValue,
    pub destination: StreamDestination,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StreamDestination {
    Upstream(OccurrenceId),
    Direct(StaticEndpoint),
    Unresolved,
    Variable,
}
