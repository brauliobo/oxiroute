use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, SocketAddr},
    path::PathBuf,
};

use crate::canonical::{dns_name, ip_address, unix_socket_path};
use crate::{
    Diagnostic, DiagnosticCode, DiagnosticStage, E_DUPLICATE_IDENTITY, E_INCLUDE_CYCLE,
    E_INVALID_VALUE, E_SOURCE_CHANGED, E_SOURCE_IO, E_SOURCE_LIMIT, E_UNRESOLVED_REFERENCE,
    E_UNSUPPORTED_FEATURE, Report, Severity, Span,
};

use super::{
    ExpandedDirective, ExpandedOccurrence, IncludeCandidateStatus, OccurrenceId, Provenance,
    SourceGraph, Word,
};

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
    pub parameters: Vec<NginxValue>,
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

/// Resolves an expanded nginx fragment whose root contains only an `http` block.
#[must_use]
pub fn resolve_http_fragment(loaded: Report<SourceGraph>) -> Report<HttpResolution> {
    let (graph, mut diagnostics) = loaded.into_parts();
    let (resolution, resolve_diagnostics) = resolve_http_graph(&graph).into_parts();
    diagnostics.extend(resolve_diagnostics);
    Report::new(resolution, diagnostics)
}

#[must_use]
pub(super) fn resolve_http_graph(graph: &SourceGraph) -> Report<HttpResolution> {
    Resolver::new(graph, false).run()
}

pub(super) fn resolve_http_root_graph(graph: &SourceGraph) -> Report<HttpResolution> {
    Resolver::new(graph, true).run()
}

struct Resolver<'a> {
    graph: &'a SourceGraph,
    dispositions: Vec<Option<OccurrenceDisposition>>,
    diagnostics: Vec<Diagnostic>,
    complete_root: bool,
}

impl<'a> Resolver<'a> {
    fn new(graph: &'a SourceGraph, complete_root: bool) -> Self {
        Self {
            graph,
            dispositions: vec![None; graph.expanded_occurrences.len()],
            diagnostics: Vec::new(),
            complete_root,
        }
    }

    fn run(mut self) -> Report<HttpResolution> {
        let mut http_blocks = Vec::new();
        let mut first_http = None;

        for directive in &self.graph.expanded_directives {
            if directive.directive.name.value == b"http" {
                if let Some(first) = first_http {
                    self.block_related(
                        directive.occurrence,
                        E_DUPLICATE_IDENTITY,
                        "nginx permits only one effective http block",
                        first,
                    );
                } else {
                    first_http = Some(directive.occurrence);
                }
                http_blocks.push(self.resolve_http_block(directive));
            } else if self.complete_root {
                self.structural_subtree(directive);
            } else {
                let message = if directive.directive.name.value == b"events" {
                    "complete nginx configuration is not an HTTP fragment; expected only an http block"
                } else {
                    "directive is outside the nginx HTTP fragment root"
                };
                self.block_subtree(directive, message);
            }
        }

        self.classify_remaining();
        let decisions = self
            .graph
            .expanded_occurrences
            .iter()
            .map(|occurrence| self.decision(occurrence))
            .collect();

        Report::new(
            HttpResolution {
                http_blocks,
                decisions,
            },
            self.diagnostics,
        )
    }

    fn resolve_http_block(&mut self, directive: &ExpandedDirective) -> EffectiveHttp {
        let shape_valid = directive.directive.arguments.is_empty() && directive.children.is_some();
        if shape_valid {
            self.resolved(directive.occurrence);
        } else {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "http must be an argument-free block",
            );
        }

        let children = directive.children.as_deref().unwrap_or_default();
        let mut upstreams = Vec::new();
        let mut upstream_by_name = HashMap::new();

        for child in children {
            if child.directive.name.value != b"upstream" {
                continue;
            }
            let upstream = self.resolve_upstream(child);
            if child.directive.arguments.len() == 1 && child.directive.children.is_some() {
                if let Some(name) = &upstream.name {
                    let normalized = ascii_lowercase(&name.value);
                    if has_variable(&name.value) {
                        self.block(
                            child.occurrence,
                            E_UNSUPPORTED_FEATURE,
                            "variables in upstream names are unsupported",
                        );
                    } else if let Some(first) = upstream_by_name.get(&normalized).copied() {
                        self.block_related(
                            child.occurrence,
                            E_DUPLICATE_IDENTITY,
                            "duplicate nginx upstream identity",
                            first,
                        );
                    } else {
                        upstream_by_name.insert(normalized, child.occurrence);
                    }
                }
            }
            upstreams.push(upstream);
        }

        let mut servers = Vec::new();
        let mut declaration_order = Vec::new();
        let mut scalar_policies = HashMap::new();
        for child in children {
            match child.directive.name.value.as_slice() {
                b"upstream" => declaration_order.push(HttpDeclaration::Upstream(child.occurrence)),
                b"server" => {
                    declaration_order.push(HttpDeclaration::Server(child.occurrence));
                    servers.push(self.resolve_server(child, &upstream_by_name));
                }
                b"types" => self.resolve_types(child),
                name if is_http_policy(name) => {
                    self.resolve_policy(child);
                    self.reject_duplicate_scalar(child, &mut scalar_policies);
                }
                _ => {
                    self.block_subtree(child, "directive is unsupported in the nginx http context");
                }
            }
        }

        let binds = self.resolve_binds(&servers);
        EffectiveHttp {
            origin: Self::origin(directive),
            declaration_order,
            upstreams,
            servers,
            binds,
        }
    }

    fn resolve_upstream(&mut self, directive: &ExpandedDirective) -> EffectiveUpstream {
        let name = directive
            .directive
            .arguments
            .first()
            .map(|word| self.value(word));
        if directive.directive.arguments.len() == 1 && directive.children.is_some() {
            self.resolved(directive.occurrence);
        } else {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "upstream requires one name and a block",
            );
        }

        let mut servers = Vec::new();
        let mut endpoint_identities = HashMap::new();
        for child in directive.children.as_deref().unwrap_or_default() {
            if child.directive.name.value != b"server" {
                self.block_subtree(child, "directive is unsupported in an nginx upstream");
                continue;
            }
            let server = self.resolve_upstream_server(child);
            if let Some(endpoint) = &server.endpoint {
                if let Some(first) = endpoint_identities.get(endpoint).copied() {
                    self.block_related(
                        child.occurrence,
                        E_DUPLICATE_IDENTITY,
                        "duplicate endpoint identity in nginx upstream",
                        first,
                    );
                } else {
                    endpoint_identities.insert(endpoint.clone(), child.occurrence);
                }
            }
            servers.push(server);
        }
        if servers.is_empty() {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "upstream requires at least one static server",
            );
        }

        EffectiveUpstream {
            origin: Self::origin(directive),
            name,
            servers,
        }
    }

    fn resolve_upstream_server(
        &mut self,
        directive: &ExpandedDirective,
    ) -> EffectiveUpstreamServer {
        let address = directive
            .directive
            .arguments
            .first()
            .map(|word| self.value(word));
        let parameters = directive
            .directive
            .arguments
            .iter()
            .skip(1)
            .map(|word| self.value(word))
            .collect::<Vec<_>>();
        let endpoint = address
            .as_ref()
            .and_then(|address| parse_static_endpoint(&address.value, 80));

        let outcome =
            if directive.directive.children.is_some() || directive.directive.arguments.is_empty() {
                Some((
                    E_INVALID_VALUE,
                    "upstream server requires a static address and a semicolon",
                ))
            } else if address
                .as_ref()
                .is_some_and(|address| has_variable(&address.value))
            {
                Some((
                    E_UNSUPPORTED_FEATURE,
                    "variables in upstream server addresses are unsupported",
                ))
            } else if endpoint.is_none() {
                Some((E_INVALID_VALUE, "invalid static upstream server address"))
            } else if !parameters.is_empty() {
                Some((
                    E_UNSUPPORTED_FEATURE,
                    "upstream server parameters are unsupported",
                ))
            } else {
                None
            };
        self.finish_occurrence(directive.occurrence, outcome);

        EffectiveUpstreamServer {
            origin: Self::origin(directive),
            address,
            endpoint,
            parameters,
        }
    }

    fn resolve_server(
        &mut self,
        directive: &ExpandedDirective,
        upstreams: &HashMap<Vec<u8>, OccurrenceId>,
    ) -> EffectiveServer {
        if directive.directive.arguments.is_empty() && directive.children.is_some() {
            self.resolved(directive.occurrence);
        } else {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "server in the http context must be an argument-free block",
            );
        }

        let children = directive.children.as_deref().unwrap_or_default();
        let mut listens = Vec::new();
        let mut server_names = Vec::new();
        let mut locations = Vec::new();
        let mut declaration_order = Vec::new();
        let mut saw_listen = false;
        let mut name_identities = HashMap::new();
        let mut location_identities = HashMap::new();
        let mut scalar_policies = HashMap::new();

        for child in children {
            match child.directive.name.value.as_slice() {
                b"listen" => {
                    saw_listen = true;
                    declaration_order.push(ServerDeclaration::Listen(child.occurrence));
                    listens.push(self.resolve_listen(child));
                }
                b"server_name" => {
                    declaration_order.push(ServerDeclaration::ServerName(child.occurrence));
                    for name in self.resolve_server_names(child) {
                        if is_supported_server_name(name.kind) {
                            let identity = (name.kind, name.normalized.clone());
                            if let Some(first) = name_identities.get(&identity).copied() {
                                self.block_related(
                                    name.origin.occurrence,
                                    E_DUPLICATE_IDENTITY,
                                    "duplicate server_name identity in virtual server",
                                    first,
                                );
                            } else {
                                name_identities.insert(identity, name.origin.occurrence);
                            }
                        }
                        server_names.push(name);
                    }
                }
                b"location" => {
                    declaration_order.push(ServerDeclaration::Location(child.occurrence));
                    let location = self.resolve_location(child, None, upstreams);
                    if is_supported_location(location.kind) {
                        if let Some(path) = &location.path {
                            let identity = (location.kind, path.value.clone());
                            if let Some(first) = location_identities.get(&identity).copied() {
                                self.block_related(
                                    location.origin.occurrence,
                                    E_DUPLICATE_IDENTITY,
                                    "duplicate location identity in virtual server",
                                    first,
                                );
                            } else {
                                location_identities.insert(identity, location.origin.occurrence);
                            }
                        }
                    }
                    locations.push(location);
                }
                b"types" => self.resolve_types(child),
                b"if" => {
                    if self.resolve_supported_if(child) {
                        locations.push(Self::synthetic_if_location(child));
                    }
                }
                name if is_server_policy(name) => {
                    self.resolve_policy(child);
                    self.reject_duplicate_scalar(child, &mut scalar_policies);
                }
                _ => self.block_subtree(child, "directive is unsupported in an nginx HTTP server"),
            }
        }

        if !saw_listen {
            listens.push(EffectiveListen {
                origin: Self::origin(directive),
                value: None,
                endpoint: Some(ListenEndpoint::Socket {
                    address: b"*".to_vec(),
                    port: 80,
                }),
                options: Vec::new(),
                default_server: false,
                implicit: true,
            });
        }

        EffectiveServer {
            origin: Self::origin(directive),
            declaration_order,
            listens,
            server_names,
            locations,
        }
    }

    fn resolve_listen(&mut self, directive: &ExpandedDirective) -> EffectiveListen {
        let value = directive
            .directive
            .arguments
            .first()
            .map(|word| self.value(word));
        let options = directive
            .directive
            .arguments
            .iter()
            .skip(1)
            .map(|word| self.value(word))
            .collect::<Vec<_>>();
        let endpoint = value
            .as_ref()
            .and_then(|value| parse_listen_endpoint(&value.value));
        let default_server = options
            .iter()
            .any(|option| matches!(option.value.as_slice(), b"default_server" | b"default"));
        let option_count = |expected: &[u8]| {
            options
                .iter()
                .filter(|option| option.value == expected)
                .count()
        };
        let default_option_count = options
            .iter()
            .filter(|option| matches!(option.value.as_slice(), b"default_server" | b"default"))
            .count();
        let unsupported_option = options.iter().any(|option| {
            !matches!(
                option.value.as_slice(),
                b"default_server" | b"default" | b"ssl" | b"http2"
            )
        });
        let repeated_option = option_count(b"ssl") > 1 || option_count(b"http2") > 1;

        let outcome =
            if directive.directive.children.is_some() || directive.directive.arguments.is_empty() {
                Some((
                    E_INVALID_VALUE,
                    "listen requires an address and a semicolon",
                ))
            } else if directive
                .directive
                .arguments
                .iter()
                .any(|argument| has_variable(&argument.value))
            {
                Some((E_UNSUPPORTED_FEATURE, "variables in listen are unsupported"))
            } else if endpoint.is_none() {
                Some((
                    E_INVALID_VALUE,
                    "unsupported or invalid nginx listen address",
                ))
            } else if default_option_count > 1 {
                Some((E_INVALID_VALUE, "listen repeats the default server option"))
            } else if repeated_option {
                Some((E_INVALID_VALUE, "listen repeats a protocol option"))
            } else if unsupported_option {
                Some((E_UNSUPPORTED_FEATURE, "nginx listen option is unsupported"))
            } else {
                None
            };
        self.finish_occurrence(directive.occurrence, outcome);

        EffectiveListen {
            origin: Self::origin(directive),
            value,
            endpoint,
            options,
            default_server,
            implicit: false,
        }
    }

    fn resolve_server_names(&mut self, directive: &ExpandedDirective) -> Vec<EffectiveServerName> {
        let names = directive
            .directive
            .arguments
            .iter()
            .map(|word| {
                let value = self.value(word);
                EffectiveServerName {
                    normalized: ascii_lowercase(&value.value),
                    kind: server_name_kind(&value.value),
                    origin: Self::origin(directive),
                    value,
                }
            })
            .collect::<Vec<_>>();

        let outcome = if directive.directive.children.is_some() || names.is_empty() {
            Some((
                E_INVALID_VALUE,
                "server_name requires at least one name and a semicolon",
            ))
        } else if names
            .iter()
            .any(|name| name.kind == ServerNameKind::Variable)
        {
            Some((
                E_UNSUPPORTED_FEATURE,
                "variables in server_name are unsupported",
            ))
        } else if names.iter().any(|name| name.kind == ServerNameKind::Regex) {
            Some((E_UNSUPPORTED_FEATURE, "regex server names are unsupported"))
        } else if names
            .iter()
            .any(|name| name.kind == ServerNameKind::Invalid)
        {
            Some((E_INVALID_VALUE, "invalid nginx server_name wildcard"))
        } else {
            None
        };
        self.finish_occurrence(directive.occurrence, outcome);
        names
    }

    fn resolve_location(
        &mut self,
        directive: &ExpandedDirective,
        inherited_proxy: Option<&EffectiveProxyPass>,
        upstreams: &HashMap<Vec<u8>, OccurrenceId>,
    ) -> EffectiveLocation {
        let (modifier, path, kind) = self.location_header(directive);
        let static_path_invalid = path.as_ref().is_some_and(|path| {
            matches!(kind, LocationKind::Exact | LocationKind::Prefix)
                && !path.value.starts_with(b"/")
        });
        let outcome = if directive.children.is_none() || path.is_none() || static_path_invalid {
            Some((E_INVALID_VALUE, "location requires a matcher and a block"))
        } else {
            match kind {
                LocationKind::Exact | LocationKind::Prefix => None,
                LocationKind::Variable => Some((
                    E_UNSUPPORTED_FEATURE,
                    "variables in location matchers are unsupported",
                )),
                LocationKind::Regex | LocationKind::RegexInsensitive => {
                    Some((E_UNSUPPORTED_FEATURE, "regex locations are unsupported"))
                }
                LocationKind::Named => {
                    Some((E_UNSUPPORTED_FEATURE, "named locations are unsupported"))
                }
                LocationKind::PrefixNoRegex => Some((
                    E_UNSUPPORTED_FEATURE,
                    "the nginx ^~ location modifier is unsupported",
                )),
                LocationKind::Invalid => Some((E_INVALID_VALUE, "invalid nginx location matcher")),
            }
        };
        self.finish_occurrence(directive.occurrence, outcome);

        let children = directive.children.as_deref().unwrap_or_default();
        let mut proxy_pass = None;
        for child in children {
            if child.directive.name.value != b"proxy_pass" {
                continue;
            }
            let parsed = self.resolve_proxy_pass(child, upstreams);
            if proxy_pass.is_some() {
                self.block(
                    child.occurrence,
                    E_DUPLICATE_IDENTITY,
                    "duplicate proxy_pass scalar in one location",
                );
            } else {
                proxy_pass = parsed;
            }
        }

        let (effective_proxy, proxy_pass_inherited) = if let Some(proxy_pass) = proxy_pass {
            (Some(proxy_pass), false)
        } else {
            (inherited_proxy.cloned(), inherited_proxy.is_some())
        };
        let mut nested_locations = Vec::new();
        let mut location_identities = HashMap::new();
        let mut scalar_policies = HashMap::new();
        for child in children {
            match child.directive.name.value.as_slice() {
                b"proxy_pass" => {}
                b"location" => {
                    let location =
                        self.resolve_location(child, effective_proxy.as_ref(), upstreams);
                    if is_supported_location(location.kind) {
                        if let Some(path) = &location.path {
                            let identity = (location.kind, path.value.clone());
                            if let Some(first) = location_identities.get(&identity).copied() {
                                self.block_related(
                                    location.origin.occurrence,
                                    E_DUPLICATE_IDENTITY,
                                    "duplicate nested location identity",
                                    first,
                                );
                            } else {
                                location_identities.insert(identity, location.origin.occurrence);
                            }
                        }
                    }
                    nested_locations.push(location);
                }
                b"types" => self.resolve_types(child),
                b"if" => {
                    self.resolve_supported_if(child);
                }
                name if is_location_policy(name) => {
                    self.resolve_policy(child);
                    self.reject_duplicate_scalar(child, &mut scalar_policies);
                }
                _ => self.block_subtree(child, "directive is unsupported in an nginx location"),
            }
        }

        EffectiveLocation {
            origin: Self::origin(directive),
            modifier,
            path,
            kind,
            proxy_pass: effective_proxy,
            proxy_pass_inherited,
            children: nested_locations,
        }
    }

    fn location_header(
        &self,
        directive: &ExpandedDirective,
    ) -> (Option<NginxValue>, Option<NginxValue>, LocationKind) {
        let arguments = &directive.directive.arguments;
        match arguments.as_slice() {
            [path] if path.value.starts_with(b"@") => {
                (None, Some(self.value(path)), LocationKind::Named)
            }
            [path] => {
                let kind = if has_variable(&path.value) {
                    LocationKind::Variable
                } else {
                    LocationKind::Prefix
                };
                (None, Some(self.value(path)), kind)
            }
            [modifier, path] => {
                let kind = if has_variable(&path.value) {
                    LocationKind::Variable
                } else {
                    match modifier.value.as_slice() {
                        b"=" => LocationKind::Exact,
                        b"^~" => LocationKind::PrefixNoRegex,
                        b"~" => LocationKind::Regex,
                        b"~*" => LocationKind::RegexInsensitive,
                        _ => LocationKind::Invalid,
                    }
                };
                (Some(self.value(modifier)), Some(self.value(path)), kind)
            }
            _ => (None, None, LocationKind::Invalid),
        }
    }

    fn resolve_proxy_pass(
        &mut self,
        directive: &ExpandedDirective,
        upstreams: &HashMap<Vec<u8>, OccurrenceId>,
    ) -> Option<EffectiveProxyPass> {
        let Some(word) = directive.directive.arguments.first() else {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "proxy_pass requires one URL and a semicolon",
            );
            return None;
        };
        let value = self.value(word);
        let parsed = parse_proxy_pass(&value.value);
        let invalid_url = parsed.is_none();
        let (scheme, authority, replacement_uri) =
            parsed.unwrap_or_else(|| (ProxyPassScheme::Unsupported(Vec::new()), Vec::new(), None));
        let variable = has_variable(&value.value);
        let bounded_downstream_scheme = scheme == ProxyPassScheme::Downstream
            && !has_variable(&authority)
            && replacement_uri
                .as_deref()
                .is_none_or(|uri| !has_variable(uri));
        let default_port = if scheme == ProxyPassScheme::Https {
            443
        } else {
            80
        };
        let direct_endpoint = parse_static_endpoint(&authority, default_port);
        let upstream = if variable && !bounded_downstream_scheme {
            UpstreamReference::Variable
        } else if let Some(upstream) = upstreams.get(&ascii_lowercase(&authority)).copied() {
            UpstreamReference::Resolved(upstream)
        } else if direct_endpoint.is_some() {
            UpstreamReference::Direct
        } else {
            UpstreamReference::Unresolved
        };

        let outcome =
            if directive.directive.children.is_some() || directive.directive.arguments.len() != 1 {
                Some((
                    E_INVALID_VALUE,
                    "proxy_pass requires one URL and a semicolon",
                ))
            } else if variable && !bounded_downstream_scheme {
                Some((
                    E_UNSUPPORTED_FEATURE,
                    "variables in proxy_pass are unsupported",
                ))
            } else if invalid_url {
                Some((
                    E_INVALID_VALUE,
                    "proxy_pass requires a static http or https URL",
                ))
            } else if matches!(scheme, ProxyPassScheme::Unsupported(_)) {
                Some((
                    E_UNSUPPORTED_FEATURE,
                    "proxy_pass URL scheme is unsupported",
                ))
            } else if upstream == UpstreamReference::Unresolved {
                Some((
                    E_UNRESOLVED_REFERENCE,
                    "proxy_pass upstream is not declared",
                ))
            } else {
                None
            };
        self.finish_occurrence(directive.occurrence, outcome);

        Some(EffectiveProxyPass {
            origin: Self::origin(directive),
            value,
            scheme,
            authority,
            replacement_uri,
            upstream,
            direct_endpoint,
        })
    }

    fn resolve_policy(&mut self, directive: &ExpandedDirective) {
        let name = directive.directive.name.value.as_slice();
        let argument_count_valid =
            policy_argument_count_valid(name, directive.directive.arguments.len());
        let outcome = if directive.directive.children.is_some() || !argument_count_valid {
            Some((
                E_INVALID_VALUE,
                "supported nginx policy has an invalid directive shape",
            ))
        } else if !policy_allows_variables(name)
            && directive
                .directive
                .arguments
                .iter()
                .any(|argument| has_variable(&argument.value))
        {
            Some((
                E_UNSUPPORTED_FEATURE,
                "variables in nginx policy values are unsupported",
            ))
        } else {
            None
        };
        self.finish_occurrence(directive.occurrence, outcome);
    }

    fn resolve_types(&mut self, directive: &ExpandedDirective) {
        if !directive.directive.arguments.is_empty() || directive.children.is_none() {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "types must be an argument-free block",
            );
            for child in directive.children.as_deref().unwrap_or_default() {
                self.structural_subtree(child);
            }
            return;
        }
        self.resolved(directive.occurrence);
        for mapping in directive.children.as_deref().unwrap_or_default() {
            let valid = mapping.directive.children.is_none()
                && !has_variable(&mapping.directive.name.value)
                && mapping
                    .directive
                    .arguments
                    .iter()
                    .all(|extension| !has_variable(&extension.value));
            if valid {
                self.resolved(mapping.occurrence);
            } else {
                self.block(
                    mapping.occurrence,
                    E_INVALID_VALUE,
                    "nginx MIME mapping requires a static content type and one or more extensions",
                );
            }
        }
    }

    fn resolve_supported_if(&mut self, directive: &ExpandedDirective) -> bool {
        let condition = directive
            .directive
            .arguments
            .iter()
            .flat_map(|argument| argument.value.iter().copied())
            .collect::<Vec<_>>();
        let certbot_host_redirect = condition
            .windows(b"$host".len())
            .any(|window| window == b"$host")
            && condition.contains(&b'=');
        let redacted_authorization = condition
            .windows(b"$http_authorization".len())
            .any(|window| window == b"$http_authorization")
            && condition
                .windows(b"<redacted>".len())
                .any(|window| window == b"<redacted>");
        let children = directive.children.as_deref().unwrap_or_default();
        let supported = directive.children.is_some()
            && !directive.directive.arguments.is_empty()
            && (certbot_host_redirect || redacted_authorization)
            && children
                .iter()
                .all(|child| child.directive.name.value == b"return");
        if supported {
            self.resolved(directive.occurrence);
            for child in children {
                self.resolve_policy(child);
            }
        } else {
            self.block_subtree(
                directive,
                "nginx if condition is outside the bounded host-redirect or redacted-authorization subset",
            );
        }
        supported && certbot_host_redirect
    }

    fn synthetic_if_location(directive: &ExpandedDirective) -> EffectiveLocation {
        EffectiveLocation {
            origin: Self::origin(directive),
            modifier: None,
            path: Some(NginxValue {
                value: b"/".to_vec(),
                raw: b"/".to_vec(),
                span: directive.directive.span,
            }),
            kind: LocationKind::Prefix,
            proxy_pass: None,
            proxy_pass_inherited: false,
            children: Vec::new(),
        }
    }

    fn reject_duplicate_scalar(
        &mut self,
        directive: &ExpandedDirective,
        scalar_policies: &mut HashMap<Vec<u8>, OccurrenceId>,
    ) {
        if !is_scalar_policy(&directive.directive.name.value) {
            return;
        }
        if let Some(first) =
            scalar_policies.insert(directive.directive.name.value.clone(), directive.occurrence)
        {
            self.block_related(
                directive.occurrence,
                E_DUPLICATE_IDENTITY,
                "duplicate nginx scalar directive in one context",
                first,
            );
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "socket identity, overlap, protocol, default, and name resolution are one pass"
    )]
    fn resolve_binds(&mut self, servers: &[EffectiveServer]) -> Vec<EffectiveBind> {
        let mut binds: Vec<EffectiveBind> = Vec::new();
        let mut endpoint_origins = Vec::new();
        let mut endpoint_protocols = HashMap::new();
        for server in servers {
            let mut server_endpoints = HashSet::new();
            for listen in &server.listens {
                let Some(endpoint) = &listen.endpoint else {
                    continue;
                };
                if !server_endpoints.insert(endpoint.clone()) {
                    if !listen.implicit {
                        let first = server
                            .listens
                            .iter()
                            .find(|candidate| {
                                candidate.origin.occurrence != listen.origin.occurrence
                                    && candidate.endpoint.as_ref() == Some(endpoint)
                            })
                            .map_or(server.origin.occurrence, |candidate| {
                                candidate.origin.occurrence
                            });
                        self.warn_related(
                            listen.origin.occurrence,
                            E_DUPLICATE_IDENTITY,
                            "paired nginx wildcard listen is represented by one canonical listener",
                            first,
                        );
                    }
                    continue;
                }

                if !endpoint_origins
                    .iter()
                    .any(|(candidate, _)| candidate == endpoint)
                {
                    endpoint_origins.push((endpoint.clone(), listen.origin.occurrence));
                }
                let protocols = listen_protocols(listen);
                if let Some((first_protocols, first)) = endpoint_protocols.get(endpoint).copied() {
                    if first_protocols != protocols {
                        self.warn_related(
                            listen.origin.occurrence,
                            E_INVALID_VALUE,
                            "nginx listen protocol options are reconciled across one effective socket",
                            first,
                        );
                    }
                } else {
                    endpoint_protocols
                        .insert(endpoint.clone(), (protocols, listen.origin.occurrence));
                }

                let bind_index = binds
                    .iter()
                    .position(|bind| bind.endpoint == *endpoint)
                    .unwrap_or_else(|| {
                        binds.push(EffectiveBind {
                            endpoint: endpoint.clone(),
                            servers: Vec::new(),
                            default_server: server.origin.occurrence,
                            default_selection: DefaultServerSelection::First,
                            names: Vec::new(),
                        });
                        binds.len() - 1
                    });
                let bind = &mut binds[bind_index];
                bind.servers.push(server.origin.occurrence);
                if listen.default_server {
                    match bind.default_selection {
                        DefaultServerSelection::First => {
                            bind.default_server = server.origin.occurrence;
                            bind.default_selection = DefaultServerSelection::Explicit {
                                listen: listen.origin.occurrence,
                            };
                        }
                        DefaultServerSelection::Explicit { listen: first } => {
                            self.block_related(
                                listen.origin.occurrence,
                                E_DUPLICATE_IDENTITY,
                                "duplicate explicit default server for nginx listen identity",
                                first,
                            );
                        }
                    }
                }
            }
        }

        for (index, (first_endpoint, first_origin)) in endpoint_origins.iter().enumerate() {
            for (second_endpoint, second_origin) in &endpoint_origins[index + 1..] {
                if listen_endpoints_overlap(first_endpoint, second_endpoint) {
                    self.block_related(
                        *second_origin,
                        E_INVALID_VALUE,
                        "conflicting listen sockets overlap in canonical listener semantics",
                        *first_origin,
                    );
                    self.block_related(
                        *first_origin,
                        E_INVALID_VALUE,
                        "conflicting listen sockets overlap in canonical listener semantics",
                        *second_origin,
                    );
                }
            }
        }

        self.resolve_bind_names(servers, &mut binds);
        binds
    }

    fn resolve_bind_names(&mut self, servers: &[EffectiveServer], binds: &mut [EffectiveBind]) {
        for bind in binds {
            let mut claimed_names = HashMap::new();
            for server_id in &bind.servers {
                let server = servers
                    .iter()
                    .find(|server| server.origin.occurrence == *server_id)
                    .expect("bind server came from the effective server list");
                if server.server_names.is_empty() {
                    let identity = (ServerNameKind::Exact, Vec::new());
                    if let Some((_, first)) = claimed_names.get(&identity).copied() {
                        self.warn_related(
                            *server_id,
                            E_DUPLICATE_IDENTITY,
                            "duplicate empty server name is ignored; nginx keeps the first-loaded virtual server",
                            first,
                        );
                    } else {
                        claimed_names.insert(identity, (*server_id, *server_id));
                    }
                    continue;
                }
                for name in &server.server_names {
                    if !is_supported_server_name(name.kind) {
                        continue;
                    }
                    let mut accepted = Vec::new();
                    let mut conflict = None;
                    for claim in server_name_claims(name) {
                        let identity = (claim.kind, claim.normalized.clone());
                        if let Some((first_server, first_origin)) =
                            claimed_names.get(&identity).copied()
                        {
                            if first_server != *server_id && conflict.is_none() {
                                conflict = Some(first_origin);
                            }
                        } else {
                            claimed_names.insert(identity, (*server_id, name.origin.occurrence));
                            accepted.push(claim);
                        }
                    }
                    if let Some(first_origin) = conflict {
                        self.warn_related(
                            name.origin.occurrence,
                            E_DUPLICATE_IDENTITY,
                            "duplicate virtual server name claim is ignored; nginx keeps the first-loaded virtual server",
                            first_origin,
                        );
                    }
                    if name.kind == ServerNameKind::LeadingWildcardAndExact && accepted.len() == 2 {
                        accepted.clear();
                        accepted.push(name.clone());
                    }
                    bind.names
                        .extend(accepted.into_iter().map(|name| BoundServerName {
                            server: *server_id,
                            name,
                        }));
                }
            }
        }
    }

    fn finish_occurrence(
        &mut self,
        occurrence: OccurrenceId,
        outcome: Option<(DiagnosticCode, &'static str)>,
    ) {
        if let Some((code, message)) = outcome {
            self.block(occurrence, code, message);
        } else {
            self.resolved(occurrence);
        }
    }

    fn block_subtree(&mut self, directive: &ExpandedDirective, message: &'static str) {
        self.block(directive.occurrence, E_UNSUPPORTED_FEATURE, message);
        for child in directive.children.as_deref().unwrap_or_default() {
            self.block_subtree(child, message);
        }
    }

    fn structural_subtree(&mut self, directive: &ExpandedDirective) {
        self.dispositions[directive.occurrence.get()] = Some(OccurrenceDisposition::Structural);
        for child in directive.children.as_deref().unwrap_or_default() {
            self.structural_subtree(child);
        }
    }

    fn classify_remaining(&mut self) {
        for index in 0..self.graph.expanded_occurrences.len() {
            if self.dispositions[index].is_some() {
                continue;
            }
            let occurrence = &self.graph.expanded_occurrences[index];
            if occurrence.directive.name.value == b"include" {
                if occurrence.directive.arguments.len() != 1
                    || occurrence.directive.children.is_some()
                {
                    self.block(
                        occurrence.id,
                        E_INVALID_VALUE,
                        "include requires one path and a semicolon",
                    );
                } else if has_variable(&occurrence.directive.arguments[0].value) {
                    self.block(
                        occurrence.id,
                        E_UNSUPPORTED_FEATURE,
                        "variables in include paths are unsupported",
                    );
                } else {
                    let failure = self
                        .graph
                        .includes
                        .iter()
                        .find(|edge| edge.occurrence == occurrence.id)
                        .and_then(include_failure);
                    self.dispositions[index] = Some(failure.map_or(
                        OccurrenceDisposition::Structural,
                        OccurrenceDisposition::Blocking,
                    ));
                }
            } else {
                self.block(
                    occurrence.id,
                    E_UNSUPPORTED_FEATURE,
                    "reachable nginx directive was not resolved",
                );
            }
        }
    }

    fn resolved(&mut self, occurrence: OccurrenceId) {
        let disposition = &mut self.dispositions[occurrence.get()];
        if disposition.is_none() {
            *disposition = Some(OccurrenceDisposition::Resolved);
        }
    }

    fn block(&mut self, occurrence: OccurrenceId, code: DiagnosticCode, message: &'static str) {
        if matches!(
            self.dispositions[occurrence.get()],
            Some(OccurrenceDisposition::Blocking(_))
        ) {
            return;
        }
        self.dispositions[occurrence.get()] = Some(OccurrenceDisposition::Blocking(code));
        let expanded = self.occurrence(occurrence);
        self.diagnostics.push(
            Diagnostic::new(code, Severity::Error, DiagnosticStage::Resolve, message)
                .with_primary_span(expanded.directive.span)
                .with_include_stack(
                    expanded
                        .provenance
                        .include_stack
                        .iter()
                        .map(|frame| frame.directive_span),
                ),
        );
    }

    fn block_related(
        &mut self,
        occurrence: OccurrenceId,
        code: DiagnosticCode,
        message: &'static str,
        first: OccurrenceId,
    ) {
        if matches!(
            self.dispositions[occurrence.get()],
            Some(OccurrenceDisposition::Blocking(_))
        ) {
            return;
        }
        self.dispositions[occurrence.get()] = Some(OccurrenceDisposition::Blocking(code));
        let expanded = self.occurrence(occurrence);
        let first = self.occurrence(first);
        self.diagnostics.push(
            Diagnostic::new(code, Severity::Error, DiagnosticStage::Resolve, message)
                .with_primary_span(expanded.directive.span)
                .with_include_stack(
                    expanded
                        .provenance
                        .include_stack
                        .iter()
                        .map(|frame| frame.directive_span),
                )
                .with_related_span(first.directive.span, "first identity declared here"),
        );
    }

    fn warn_related(
        &mut self,
        occurrence: OccurrenceId,
        code: DiagnosticCode,
        message: &'static str,
        related: OccurrenceId,
    ) {
        let expanded = self.occurrence(occurrence);
        let first = self.occurrence(related);
        self.diagnostics.push(
            Diagnostic::new(code, Severity::Warning, DiagnosticStage::Resolve, message)
                .with_primary_span(expanded.directive.span)
                .with_related_span(first.directive.span, "first-loaded declaration is here")
                .with_include_stack(
                    expanded
                        .provenance
                        .include_stack
                        .iter()
                        .map(|frame| frame.directive_span),
                ),
        );
    }

    fn occurrence(&self, id: OccurrenceId) -> &ExpandedOccurrence {
        let occurrence = &self.graph.expanded_occurrences[id.get()];
        debug_assert_eq!(occurrence.id, id);
        occurrence
    }

    fn origin(directive: &ExpandedDirective) -> DirectiveOrigin {
        DirectiveOrigin {
            occurrence: directive.occurrence,
            span: directive.directive.span,
            provenance: directive.provenance.clone(),
        }
    }

    fn value(&self, word: &Word) -> NginxValue {
        let source = self
            .graph
            .source(word.span.source())
            .expect("expanded word source is retained in the graph");
        NginxValue {
            value: word.value.clone(),
            raw: source
                .source
                .slice(word.span.range())
                .expect("expanded word span is within its source")
                .to_vec(),
            span: word.span,
        }
    }

    fn decision(&self, occurrence: &ExpandedOccurrence) -> OccurrenceDecision {
        OccurrenceDecision {
            occurrence: occurrence.id,
            parent: occurrence.parent,
            name: self.value(&occurrence.directive.name),
            arguments: occurrence
                .directive
                .arguments
                .iter()
                .map(|word| self.value(word))
                .collect(),
            span: occurrence.directive.span,
            provenance: occurrence.provenance.clone(),
            disposition: self.dispositions[occurrence.id.get()]
                .expect("every expanded occurrence has a terminal disposition"),
        }
    }
}

fn parse_listen_endpoint(value: &[u8]) -> Option<ListenEndpoint> {
    if let Some(path) = value.strip_prefix(b"unix:") {
        return unix_socket_path(path).map(|path| ListenEndpoint::Unix { path });
    }
    if value.iter().all(u8::is_ascii_digit) {
        return parse_port(value).map(|port| ListenEndpoint::Socket {
            address: b"*".to_vec(),
            port,
        });
    }
    if value.starts_with(b"[") {
        let closing = value.iter().position(|byte| *byte == b']')?;
        let port = match value.get(closing + 1..) {
            Some([]) => 80,
            Some(rest) if rest.first() == Some(&b':') => parse_port(&rest[1..])?,
            _ => return None,
        };
        return Some(ListenEndpoint::Socket {
            address: if &value[..=closing] == b"[::]" {
                b"*".to_vec()
            } else {
                ascii_lowercase(&value[..=closing])
            },
            port,
        });
    }
    let Some(colon) = value.iter().rposition(|byte| *byte == b':') else {
        return Some(ListenEndpoint::Socket {
            address: match value {
                b"*" | b"0.0.0.0" => b"*".to_vec(),
                address if !address.is_empty() => ascii_lowercase(address),
                _ => return None,
            },
            port: 80,
        });
    };
    if value[..colon].contains(&b':') {
        return None;
    }
    let address = match &value[..colon] {
        b"" | b"*" | b"0.0.0.0" => b"*".to_vec(),
        address => ascii_lowercase(address),
    };
    Some(ListenEndpoint::Socket {
        address,
        port: parse_port(&value[colon + 1..])?,
    })
}

fn listen_protocols(listen: &EffectiveListen) -> (bool, bool) {
    (
        listen.options.iter().any(|option| option.value == b"ssl"),
        listen.options.iter().any(|option| option.value == b"http2"),
    )
}

fn listen_endpoints_overlap(first: &ListenEndpoint, second: &ListenEndpoint) -> bool {
    let (
        ListenEndpoint::Socket {
            address: first_address,
            port: first_port,
        },
        ListenEndpoint::Socket {
            address: second_address,
            port: second_port,
        },
    ) = (first, second)
    else {
        return false;
    };
    if first == second || first_port != second_port {
        return false;
    }
    let Some(first_ip) = listen_endpoint_ip(first_address) else {
        return false;
    };
    let Some(second_ip) = listen_endpoint_ip(second_address) else {
        return false;
    };
    let first_ip = canonical_ip(first_ip);
    let second_ip = canonical_ip(second_ip);
    first_ip == second_ip || first_ip.is_unspecified() || second_ip.is_unspecified()
}

fn listen_endpoint_ip(address: &[u8]) -> Option<IpAddr> {
    if address == b"*" {
        return Some(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
    }
    let address = address
        .strip_prefix(b"[")
        .and_then(|address| address.strip_suffix(b"]"))
        .unwrap_or(address);
    std::str::from_utf8(address).ok()?.parse().ok()
}

fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ipv6) => ipv6.to_ipv4_mapped().map_or(IpAddr::V6(ipv6), IpAddr::V4),
        IpAddr::V4(_) => ip,
    }
}

fn include_failure(edge: &super::IncludeEdge) -> Option<DiagnosticCode> {
    edge.failure.or_else(|| {
        edge.candidates
            .iter()
            .find_map(|candidate| match candidate.status {
                IncludeCandidateStatus::Expanded(_) => None,
                IncludeCandidateStatus::Cycle(_) => Some(E_INCLUDE_CYCLE),
                IncludeCandidateStatus::ExpansionLimit(_)
                | IncludeCandidateStatus::SourceSizeLimit
                | IncludeCandidateStatus::SourceFileLimit
                | IncludeCandidateStatus::AggregateSourceLimit => Some(E_SOURCE_LIMIT),
                IncludeCandidateStatus::CanonicalizeFailed
                | IncludeCandidateStatus::SourceChanged => Some(E_SOURCE_CHANGED),
                IncludeCandidateStatus::SourceIo => Some(E_SOURCE_IO),
            })
    })
}

fn parse_static_endpoint(value: &[u8], default_port: u16) -> Option<StaticEndpoint> {
    if value.is_empty() {
        return None;
    }
    if let Some(path) = value.strip_prefix(b"unix:") {
        return unix_socket_path(path).map(|path| StaticEndpoint::Unix { path });
    }
    let (host, port) = if value.starts_with(b"[") {
        let closing = value.iter().position(|byte| *byte == b']')?;
        let port = match value.get(closing + 1..) {
            Some([]) => default_port,
            Some(rest) if rest.first() == Some(&b':') => parse_port(&rest[1..])?,
            _ => return None,
        };
        (&value[..=closing], port)
    } else if let Some(colon) = value.iter().rposition(|byte| *byte == b':') {
        if value[..colon].contains(&b':') || colon == 0 {
            return None;
        }
        (&value[..colon], parse_port(&value[colon + 1..])?)
    } else {
        (value, default_port)
    };
    if let Some(ip) = ip_address(host) {
        return Some(StaticEndpoint::Socket {
            address: SocketAddr::new(ip, port),
        });
    }
    dns_name(host).map(|host| StaticEndpoint::Dns { host, port })
}

fn parse_port(value: &[u8]) -> Option<u16> {
    let port = std::str::from_utf8(value).ok()?.parse::<u16>().ok()?;
    (port != 0).then_some(port)
}

fn server_name_kind(value: &[u8]) -> ServerNameKind {
    if has_variable(value) {
        return ServerNameKind::Variable;
    }
    if value.starts_with(b"~") {
        return ServerNameKind::Regex;
    }
    if value.starts_with(b"*.") && value.len() > 2 && !value[2..].contains(&b'*') {
        ServerNameKind::LeadingWildcard
    } else if value.starts_with(b".") && value.len() > 1 && !value.contains(&b'*') {
        ServerNameKind::LeadingWildcardAndExact
    } else if value.ends_with(b".*") && value.len() > 2 && !value[..value.len() - 2].contains(&b'*')
    {
        ServerNameKind::TrailingWildcard
    } else if !value.contains(&b'*') {
        ServerNameKind::Exact
    } else {
        ServerNameKind::Invalid
    }
}

fn server_name_claims(name: &EffectiveServerName) -> Vec<EffectiveServerName> {
    if name.kind != ServerNameKind::LeadingWildcardAndExact {
        return vec![name.clone()];
    }
    let suffix = name
        .normalized
        .strip_prefix(b".")
        .expect("leading-dot name has a dot prefix");
    let mut exact = name.clone();
    exact.kind = ServerNameKind::Exact;
    exact.normalized = suffix.to_vec();
    let mut wildcard = name.clone();
    wildcard.kind = ServerNameKind::LeadingWildcard;
    wildcard.normalized = [b"*.".as_slice(), suffix].concat();
    vec![exact, wildcard]
}

const fn is_supported_server_name(kind: ServerNameKind) -> bool {
    matches!(
        kind,
        ServerNameKind::Exact
            | ServerNameKind::LeadingWildcard
            | ServerNameKind::LeadingWildcardAndExact
            | ServerNameKind::TrailingWildcard
    )
}

const fn is_supported_location(kind: LocationKind) -> bool {
    matches!(kind, LocationKind::Exact | LocationKind::Prefix)
}

fn is_http_policy(name: &[u8]) -> bool {
    is_location_policy(name)
        || matches!(
            name,
            b"ssl_certificate"
                | b"ssl_certificate_key"
                | b"ssl_protocols"
                | b"ssl_ciphers"
                | b"ssl_dhparam"
                | b"ssl_prefer_server_ciphers"
                | b"ssl_session_cache"
                | b"ssl_session_tickets"
                | b"ssl_session_timeout"
                | b"http2"
                | b"access_log"
                | b"error_log"
                | b"log_format"
                | b"gzip"
                | b"gzip_comp_level"
                | b"gzip_http_version"
                | b"gzip_min_length"
                | b"gzip_proxied"
                | b"gzip_types"
                | b"gzip_vary"
                | b"keepalive_timeout"
                | b"large_client_header_buffers"
                | b"limit_conn_zone"
                | b"limit_req_zone"
                | b"send_timeout"
                | b"sendfile"
                | b"types_hash_max_size"
        )
}

fn is_server_policy(name: &[u8]) -> bool {
    is_http_policy(name)
}

fn is_location_policy(name: &[u8]) -> bool {
    matches!(
        name,
        b"client_max_body_size"
            | b"proxy_connect_timeout"
            | b"proxy_read_timeout"
            | b"proxy_send_timeout"
            | b"proxy_http_version"
            | b"proxy_buffering"
            | b"proxy_request_buffering"
            | b"proxy_next_upstream"
            | b"proxy_next_upstream_tries"
            | b"proxy_set_header"
            | b"proxy_hide_header"
            | b"proxy_pass_header"
            | b"proxy_ignore_headers"
            | b"proxy_cookie_path"
            | b"root"
            | b"index"
            | b"return"
            | b"auth_basic"
            | b"auth_basic_user_file"
            | b"add_header"
            | b"alias"
            | b"autoindex"
            | b"autoindex_exact_size"
            | b"autoindex_localtime"
            | b"default_type"
            | b"error_page"
            | b"etag"
            | b"expires"
            | b"proxy_cache"
            | b"try_files"
    )
}

fn is_scalar_policy(name: &[u8]) -> bool {
    matches!(
        name,
        b"client_max_body_size"
            | b"proxy_connect_timeout"
            | b"proxy_read_timeout"
            | b"proxy_send_timeout"
            | b"proxy_http_version"
            | b"proxy_buffering"
            | b"proxy_request_buffering"
            | b"proxy_next_upstream"
            | b"proxy_next_upstream_tries"
            | b"root"
            | b"return"
            | b"auth_basic"
            | b"auth_basic_user_file"
            | b"etag"
            | b"ssl_protocols"
            | b"http2"
            | b"gzip"
            | b"gzip_comp_level"
            | b"gzip_http_version"
            | b"gzip_min_length"
            | b"gzip_proxied"
            | b"gzip_types"
            | b"gzip_vary"
    )
}

fn policy_argument_count_valid(name: &[u8], count: usize) -> bool {
    match name {
        b"ssl_protocols"
        | b"index"
        | b"proxy_next_upstream"
        | b"proxy_ignore_headers"
        | b"gzip_proxied"
        | b"gzip_types"
        | b"log_format"
        | b"try_files"
        | b"error_page" => count > 0,
        b"proxy_set_header"
        | b"proxy_cookie_path"
        | b"large_client_header_buffers"
        | b"limit_conn_zone"
        | b"limit_req_zone" => count >= 2,
        b"add_header" => matches!(count, 2 | 3),
        b"access_log" | b"error_log" | b"keepalive_timeout" => matches!(count, 1 | 2),
        b"return" => matches!(count, 1 | 2),
        _ => count == 1,
    }
}

fn policy_allows_variables(name: &[u8]) -> bool {
    matches!(
        name,
        b"proxy_set_header"
            | b"proxy_cookie_path"
            | b"return"
            | b"limit_conn_zone"
            | b"limit_req_zone"
            | b"log_format"
            | b"try_files"
    )
}

fn parse_proxy_pass(value: &[u8]) -> Option<(ProxyPassScheme, Vec<u8>, Option<Vec<u8>>)> {
    let (scheme, rest) = if let Some(rest) = value.strip_prefix(b"http://") {
        (ProxyPassScheme::Http, rest)
    } else if let Some(rest) = value.strip_prefix(b"https://") {
        (ProxyPassScheme::Https, rest)
    } else if let Some(rest) = value.strip_prefix(b"$scheme://") {
        (ProxyPassScheme::Downstream, rest)
    } else {
        let separator = value.windows(3).position(|window| window == b"://")?;
        (
            ProxyPassScheme::Unsupported(value[..separator].to_vec()),
            &value[separator + 3..],
        )
    };
    let uri_start = rest
        .iter()
        .position(|byte| matches!(*byte, b'/' | b'?' | b'#'));
    let (authority, replacement_uri) = uri_start.map_or_else(
        || (rest.to_vec(), None),
        |index| (rest[..index].to_vec(), Some(rest[index..].to_vec())),
    );
    if authority.is_empty() {
        return None;
    }
    Some((scheme, authority, replacement_uri))
}

fn has_variable(value: &[u8]) -> bool {
    value.iter().enumerate().any(|(index, byte)| {
        *byte == b'$'
            && value[..index]
                .iter()
                .rev()
                .take_while(|candidate| **candidate == b'\\')
                .count()
                % 2
                == 0
    })
}

fn ascii_lowercase(value: &[u8]) -> Vec<u8> {
    value.iter().map(u8::to_ascii_lowercase).collect()
}
