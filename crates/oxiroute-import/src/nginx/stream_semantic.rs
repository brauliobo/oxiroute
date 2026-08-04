use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, SocketAddr},
};

use crate::canonical::{dns_name, ip_address, unix_socket_path};
use crate::{
    Diagnostic, DiagnosticCode, DiagnosticStage, E_DUPLICATE_IDENTITY, E_INCLUDE_CYCLE,
    E_INVALID_VALUE, E_SOURCE_CHANGED, E_SOURCE_IO, E_SOURCE_LIMIT, E_UNRESOLVED_REFERENCE,
    E_UNSUPPORTED_FEATURE, Report, Severity,
};

use super::{DirectiveOrigin, ListenEndpoint, NginxValue, StaticEndpoint};
use super::{
    ExpandedDirective, ExpandedOccurrence, IncludeCandidateStatus, IncludeEdge, OccurrenceDecision,
    OccurrenceDisposition, OccurrenceId, SourceGraph, Word,
};

const NGINX_DEFAULT_PROXY_CONNECT_TIMEOUT_MS: u64 = 60_000;
const NGINX_DEFAULT_PROXY_TIMEOUT_MS: u64 = 600_000;

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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveStreamListen {
    pub origin: DirectiveOrigin,
    pub value: Option<NginxValue>,
    pub endpoint: Option<ListenEndpoint>,
    pub options: Vec<NginxValue>,
    pub default_server: bool,
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

#[must_use]
pub fn resolve_stream_fragment(loaded: Report<SourceGraph>) -> Report<StreamResolution> {
    let (graph, mut diagnostics) = loaded.into_parts();
    let (resolution, resolve_diagnostics) = resolve_stream_graph(&graph).into_parts();
    diagnostics.extend(resolve_diagnostics);
    Report::new(resolution, diagnostics)
}

pub(super) fn resolve_stream_graph(graph: &SourceGraph) -> Report<StreamResolution> {
    Resolver::new(graph, false).run()
}

pub(super) fn resolve_stream_root_graph(graph: &SourceGraph) -> Report<StreamResolution> {
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

    fn run(mut self) -> Report<StreamResolution> {
        let mut stream_blocks = Vec::new();
        let mut first_stream = None;

        for directive in &self.graph.expanded_directives {
            if directive.directive.name.value == b"stream" {
                if let Some(first) = first_stream {
                    self.block_related(
                        directive.occurrence,
                        E_DUPLICATE_IDENTITY,
                        "nginx permits only one effective stream block",
                        first,
                    );
                } else {
                    first_stream = Some(directive.occurrence);
                }
                stream_blocks.push(self.resolve_stream_block(directive));
            } else if self.complete_root {
                self.structural_subtree(directive);
            } else {
                self.block_subtree(
                    directive,
                    "complete nginx configuration is not a stream fragment; expected only a stream block",
                );
            }
        }

        self.reject_overlapping_listens(&stream_blocks);
        self.classify_remaining();
        let decisions = self
            .graph
            .expanded_occurrences
            .iter()
            .map(|occurrence| self.decision(occurrence))
            .collect();

        Report::new(
            StreamResolution {
                stream_blocks,
                decisions,
            },
            self.diagnostics,
        )
    }

    fn resolve_stream_block(&mut self, directive: &ExpandedDirective) -> EffectiveStream {
        if directive.directive.arguments.is_empty() && directive.children.is_some() {
            self.resolved(directive.occurrence);
        } else {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "stream must be an argument-free block",
            );
        }

        let children = directive.children.as_deref().unwrap_or_default();
        let mut upstreams = Vec::new();
        let mut upstream_by_name = HashMap::new();
        let mut declaration_order = Vec::new();
        let mut connect_timeout_ms = NGINX_DEFAULT_PROXY_CONNECT_TIMEOUT_MS;
        let mut connect_timeout_origin = None;
        let mut idle_timeout_ms = NGINX_DEFAULT_PROXY_TIMEOUT_MS;
        let mut idle_timeout_origin = None;
        let mut scalar_policies = HashMap::new();

        for child in children {
            match child.directive.name.value.as_slice() {
                b"upstream" => {
                    declaration_order.push(StreamDeclaration::Upstream(child.occurrence));
                    let upstream = self.resolve_upstream(child);
                    if child.directive.arguments.len() == 1 && child.children.is_some() {
                        if let Some(name) = &upstream.name {
                            let normalized = ascii_lowercase(&name.value);
                            if has_variable(&name.value) {
                                self.block(
                                    child.occurrence,
                                    E_UNSUPPORTED_FEATURE,
                                    "variables in stream upstream names are unsupported",
                                );
                            } else if let Some(first) = upstream_by_name.get(&normalized).copied() {
                                self.block_related(
                                    child.occurrence,
                                    E_DUPLICATE_IDENTITY,
                                    "duplicate nginx stream upstream identity",
                                    first,
                                );
                            } else {
                                upstream_by_name.insert(normalized, child.occurrence);
                            }
                        }
                    }
                    upstreams.push(upstream);
                }
                b"server" => declaration_order.push(StreamDeclaration::Server(child.occurrence)),
                b"proxy_connect_timeout" => {
                    if let Some(value) = self.resolve_timeout(child) {
                        if self.reject_duplicate_scalar(child, &mut scalar_policies) {
                            continue;
                        }
                        connect_timeout_ms = value;
                        connect_timeout_origin = Some(Self::origin(child));
                    }
                }
                b"proxy_timeout" => {
                    if let Some(value) = self.resolve_timeout(child) {
                        if self.reject_duplicate_scalar(child, &mut scalar_policies) {
                            continue;
                        }
                        idle_timeout_ms = value;
                        idle_timeout_origin = Some(Self::origin(child));
                    }
                }
                b"map" | b"split_clients" | b"geo" | b"keyval" | b"js_set" | b"js_var" => {
                    self.block_subtree(
                        child,
                        "dynamic stream routing is outside the bounded static TCP subset",
                    );
                }
                _ => self.block_subtree(
                    child,
                    "directive is unsupported in the nginx stream context",
                ),
            }
        }

        let mut servers = Vec::new();
        for child in children {
            if child.directive.name.value == b"server" {
                servers.push(self.resolve_server(child, &upstream_by_name));
            }
        }
        if servers.is_empty() {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "stream requires at least one server block",
            );
        }

        EffectiveStream {
            origin: Self::origin(directive),
            declaration_order,
            upstreams,
            servers,
            connect_timeout_ms,
            connect_timeout_origin,
            idle_timeout_ms,
            idle_timeout_origin,
        }
    }

    fn resolve_upstream(&mut self, directive: &ExpandedDirective) -> EffectiveStreamUpstream {
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
                "stream upstream requires one name and a block",
            );
        }

        let mut servers = Vec::new();
        let mut endpoint_identities = HashMap::new();
        for child in directive.children.as_deref().unwrap_or_default() {
            if child.directive.name.value != b"server" {
                self.block_subtree(
                    child,
                    "directive is unsupported in an nginx stream upstream",
                );
                continue;
            }
            let server = self.resolve_upstream_server(child);
            if let Some(endpoint) = &server.endpoint {
                if let Some(first) = endpoint_identities.get(endpoint).copied() {
                    self.block_related(
                        child.occurrence,
                        E_DUPLICATE_IDENTITY,
                        "duplicate endpoint identity in nginx stream upstream",
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
                "stream upstream requires at least one static server",
            );
        }

        EffectiveStreamUpstream {
            origin: Self::origin(directive),
            name,
            servers,
        }
    }

    fn resolve_upstream_server(
        &mut self,
        directive: &ExpandedDirective,
    ) -> EffectiveStreamUpstreamServer {
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
        let outcome = if directive.children.is_some() || address.is_none() {
            Some((
                E_INVALID_VALUE,
                "stream upstream server requires a static address and a semicolon",
            ))
        } else if address
            .as_ref()
            .is_some_and(|address| has_variable(&address.value))
        {
            Some((
                E_UNSUPPORTED_FEATURE,
                "variables in stream upstream server addresses are unsupported",
            ))
        } else if endpoint.is_none() {
            Some((
                E_INVALID_VALUE,
                "invalid static stream upstream server address",
            ))
        } else if !parameters.is_empty() {
            Some((
                E_UNSUPPORTED_FEATURE,
                "stream upstream server parameters are unsupported",
            ))
        } else {
            None
        };
        self.finish_occurrence(directive.occurrence, outcome);

        EffectiveStreamUpstreamServer {
            origin: Self::origin(directive),
            address,
            endpoint,
            parameters,
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "stream server declarations and representability blockers resolve in source order"
    )]
    fn resolve_server(
        &mut self,
        directive: &ExpandedDirective,
        upstreams: &HashMap<Vec<u8>, OccurrenceId>,
    ) -> EffectiveStreamServer {
        if directive.directive.arguments.is_empty() && directive.children.is_some() {
            self.resolved(directive.occurrence);
        } else {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "server in the stream context must be an argument-free block",
            );
        }

        let children = directive.children.as_deref().unwrap_or_default();
        let mut listens = Vec::new();
        let mut proxy_pass = None;
        let mut connect_timeout_ms = None;
        let mut connect_timeout_origin = None;
        let mut idle_timeout_ms = None;
        let mut idle_timeout_origin = None;
        let mut scalar_policies = HashMap::new();

        for child in children {
            match child.directive.name.value.as_slice() {
                b"listen" => listens.push(self.resolve_listen(child)),
                b"proxy_pass" => {
                    let candidate = self.resolve_proxy_pass(child, upstreams);
                    if let Some(first) = proxy_pass
                        .as_ref()
                        .map(|proxy: &EffectiveStreamProxyPass| proxy.origin.occurrence)
                    {
                        self.block_related(
                            child.occurrence,
                            E_DUPLICATE_IDENTITY,
                            "duplicate nginx stream proxy_pass directive",
                            first,
                        );
                    } else {
                        proxy_pass = Some(candidate);
                    }
                }
                b"proxy_connect_timeout" => {
                    if let Some(value) = self.resolve_timeout(child) {
                        if self.reject_duplicate_scalar(child, &mut scalar_policies) {
                            continue;
                        }
                        connect_timeout_ms = Some(value);
                        connect_timeout_origin = Some(Self::origin(child));
                    }
                }
                b"proxy_timeout" => {
                    if let Some(value) = self.resolve_timeout(child) {
                        if self.reject_duplicate_scalar(child, &mut scalar_policies) {
                            continue;
                        }
                        idle_timeout_ms = Some(value);
                        idle_timeout_origin = Some(Self::origin(child));
                    }
                }
                b"ssl_preread" => {
                    self.resolve_off_only_flag(child, "nginx stream TLS preread is unsupported");
                }
                b"proxy_protocol" => {
                    self.resolve_off_only_flag(child, "nginx stream PROXY protocol is unsupported");
                }
                b"proxy_next_upstream" => self.resolve_off_only_flag(
                    child,
                    "nginx stream upstream retry policy is unsupported",
                ),
                b"proxy_socket_keepalive" => self.resolve_off_only_flag(
                    child,
                    "nginx stream socket keepalive policy is not represented canonically",
                ),
                b"server_name" => self.block_subtree(
                    child,
                    "nginx stream server_name routing requires unsupported TLS/SNI inspection",
                ),
                b"preread_buffer_size"
                | b"preread_timeout"
                | b"js_preread"
                | b"js_filter"
                | b"proxy_bind"
                | b"proxy_buffer_size"
                | b"proxy_download_rate"
                | b"proxy_upload_rate"
                | b"proxy_half_close"
                | b"proxy_next_upstream_tries"
                | b"proxy_next_upstream_timeout"
                | b"proxy_session_drop"
                | b"proxy_ssl"
                | b"proxy_ssl_certificate"
                | b"proxy_ssl_certificate_key"
                | b"proxy_ssl_protocols" => self.block_subtree(
                    child,
                    "directive is outside the bounded static TCP stream subset",
                ),
                _ => {
                    self.block_subtree(child, "directive is unsupported in an nginx stream server");
                }
            }
        }

        if listens.is_empty() {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "nginx stream server requires an explicit listen",
            );
        }
        if proxy_pass.is_none() {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "nginx stream server requires one proxy_pass",
            );
        }

        EffectiveStreamServer {
            origin: Self::origin(directive),
            listens,
            proxy_pass,
            connect_timeout_ms,
            connect_timeout_origin,
            idle_timeout_ms,
            idle_timeout_origin,
        }
    }

    fn resolve_listen(&mut self, directive: &ExpandedDirective) -> EffectiveStreamListen {
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
            .and_then(|value| parse_stream_listen_endpoint(&value.value));
        let mut outcome = None;
        if directive.children.is_some() || value.is_none() {
            outcome = Some((
                E_INVALID_VALUE,
                "stream listen requires one static address and a semicolon",
            ));
        } else if value
            .as_ref()
            .is_some_and(|value| has_variable(&value.value))
        {
            outcome = Some((
                E_UNSUPPORTED_FEATURE,
                "variables in stream listen addresses are unsupported",
            ));
        } else if endpoint.is_none() {
            outcome = Some((
                E_INVALID_VALUE,
                "stream listen must be a numeric socket or absolute Unix socket",
            ));
        }

        let mut default_server = false;
        let mut seen_options = HashSet::new();
        for option in &options {
            if !seen_options.insert(option.value.clone()) {
                outcome = Some((E_DUPLICATE_IDENTITY, "duplicate nginx stream listen option"));
                continue;
            }
            match option.value.as_slice() {
                b"default_server" => default_server = true,
                b"udp" => {
                    outcome = Some((
                        E_UNSUPPORTED_FEATURE,
                        "nginx stream UDP listeners are outside the TCP importer subset",
                    ));
                }
                b"ssl" | b"proxy_protocol" => {
                    outcome = Some((
                        E_UNSUPPORTED_FEATURE,
                        "nginx stream listener TLS or PROXY protocol options are unsupported",
                    ));
                }
                _ => {
                    outcome = Some((
                        E_UNSUPPORTED_FEATURE,
                        "nginx stream listen options are not represented canonically",
                    ));
                }
            }
        }
        self.finish_occurrence(directive.occurrence, outcome);
        EffectiveStreamListen {
            origin: Self::origin(directive),
            value,
            endpoint,
            options,
            default_server,
        }
    }

    fn resolve_proxy_pass(
        &mut self,
        directive: &ExpandedDirective,
        upstreams: &HashMap<Vec<u8>, OccurrenceId>,
    ) -> EffectiveStreamProxyPass {
        let value = directive.directive.arguments.first().map_or_else(
            || NginxValue {
                value: Vec::new(),
                raw: Vec::new(),
                span: directive.directive.span,
            },
            |word| self.value(word),
        );
        let normalized = ascii_lowercase(&value.value);
        let destination =
            if directive.children.is_some() || directive.directive.arguments.len() != 1 {
                self.block(
                    directive.occurrence,
                    E_INVALID_VALUE,
                    "stream proxy_pass requires one static address and a semicolon",
                );
                StreamDestination::Unresolved
            } else if has_variable(&value.value) {
                self.block(
                    directive.occurrence,
                    E_UNSUPPORTED_FEATURE,
                    "variables in stream proxy_pass addresses are unsupported",
                );
                StreamDestination::Variable
            } else if let Some(upstream) = upstreams.get(&normalized).copied() {
                self.resolved(directive.occurrence);
                StreamDestination::Upstream(upstream)
            } else if is_direct_endpoint_value(&value.value) {
                if let Some(endpoint) = parse_static_endpoint(&value.value, 80) {
                    self.resolved(directive.occurrence);
                    StreamDestination::Direct(endpoint)
                } else {
                    self.block(
                    directive.occurrence,
                    E_INVALID_VALUE,
                    "stream proxy_pass address is not a canonical socket, DNS, or Unix endpoint",
                );
                    StreamDestination::Unresolved
                }
            } else {
                self.block(
                    directive.occurrence,
                    E_UNRESOLVED_REFERENCE,
                    "stream proxy_pass references an unresolved upstream",
                );
                StreamDestination::Unresolved
            };

        EffectiveStreamProxyPass {
            origin: Self::origin(directive),
            value,
            destination,
        }
    }

    fn resolve_timeout(&mut self, directive: &ExpandedDirective) -> Option<u64> {
        let valid_shape = directive.children.is_none() && directive.directive.arguments.len() == 1;
        if !valid_shape {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "stream timeout requires one duration and a semicolon",
            );
            return None;
        }
        let value = &directive.directive.arguments[0].value;
        if has_variable(value) {
            self.block(
                directive.occurrence,
                E_UNSUPPORTED_FEATURE,
                "variables in stream timeouts are unsupported",
            );
            return None;
        }
        if let Some(value) = parse_duration_ms(value) {
            self.resolved(directive.occurrence);
            Some(value)
        } else {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "stream timeout must be a positive whole-millisecond duration",
            );
            None
        }
    }

    fn resolve_off_only_flag(&mut self, directive: &ExpandedDirective, message: &'static str) {
        if directive.children.is_none()
            && directive.directive.arguments.len() == 1
            && directive.directive.arguments[0].value == b"off"
        {
            self.resolved(directive.occurrence);
        } else if directive.children.is_some() || directive.directive.arguments.len() != 1 {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "stream flag requires one literal value and a semicolon",
            );
        } else {
            self.block(directive.occurrence, E_UNSUPPORTED_FEATURE, message);
        }
    }

    fn reject_duplicate_scalar(
        &mut self,
        directive: &ExpandedDirective,
        scalar_policies: &mut HashMap<Vec<u8>, OccurrenceId>,
    ) -> bool {
        if let Some(first) =
            scalar_policies.insert(directive.directive.name.value.clone(), directive.occurrence)
        {
            self.block_related(
                directive.occurrence,
                E_DUPLICATE_IDENTITY,
                "duplicate nginx stream scalar directive",
                first,
            );
            true
        } else {
            false
        }
    }

    fn reject_overlapping_listens(&mut self, streams: &[EffectiveStream]) {
        let listens = streams
            .iter()
            .flat_map(|stream| &stream.servers)
            .flat_map(|server| &server.listens)
            .filter_map(|listen| {
                listen
                    .endpoint
                    .as_ref()
                    .map(|endpoint| (endpoint, listen.origin.occurrence))
            })
            .collect::<Vec<_>>();
        for (index, (first, first_origin)) in listens.iter().enumerate() {
            for (second, second_origin) in &listens[index + 1..] {
                if first == second || listen_endpoints_overlap(first, second) {
                    self.block_related(
                        *second_origin,
                        E_DUPLICATE_IDENTITY,
                        "overlapping nginx stream listen sockets cannot be represented as independent TCP listeners",
                        *first_origin,
                    );
                    self.block_related(
                        *first_origin,
                        E_DUPLICATE_IDENTITY,
                        "overlapping nginx stream listen sockets cannot be represented as independent TCP listeners",
                        *second_origin,
                    );
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
                .with_related_span(first.directive.span, "first identity declared here")
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

fn parse_stream_listen_endpoint(value: &[u8]) -> Option<ListenEndpoint> {
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
        let rest = value.get(closing + 1..)?;
        let port = rest.strip_prefix(b":").and_then(parse_port)?;
        let address = &value[..=closing];
        ip_address(address)?;
        return Some(ListenEndpoint::Socket {
            address: if address == b"[::]" {
                b"*".to_vec()
            } else {
                ascii_lowercase(address)
            },
            port,
        });
    }
    let colon = value.iter().rposition(|byte| *byte == b':')?;
    if value[..colon].contains(&b':') {
        return None;
    }
    let port = parse_port(&value[colon + 1..])?;
    let address = match &value[..colon] {
        b"" | b"*" | b"0.0.0.0" => b"*".to_vec(),
        address => {
            ip_address(address)?;
            ascii_lowercase(address)
        }
    };
    Some(ListenEndpoint::Socket { address, port })
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

fn is_direct_endpoint_value(value: &[u8]) -> bool {
    value.starts_with(b"unix:")
        || value.contains(&b':')
        || ip_address(value).is_some()
        || dns_name(value).is_some()
}

fn parse_duration_ms(value: &[u8]) -> Option<u64> {
    const MAX_CANONICAL_INTEGER: u64 = 9_007_199_254_740_991;

    if value.iter().all(u8::is_ascii_digit) {
        let amount = std::str::from_utf8(value).ok()?.parse::<u64>().ok()?;
        let milliseconds = amount.checked_mul(1_000)?;
        return (milliseconds > 0 && milliseconds <= MAX_CANONICAL_INTEGER).then_some(milliseconds);
    }

    let mut remaining = value;
    let mut previous_rank = None;
    let mut only_bare_value_remains = false;
    let mut total = 0_u64;
    while !remaining.is_empty() {
        let digit_count = remaining
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        if digit_count == 0 {
            return None;
        }
        let amount = std::str::from_utf8(&remaining[..digit_count])
            .ok()?
            .parse::<u64>()
            .ok()?;
        remaining = &remaining[digit_count..];
        if remaining.is_empty() {
            total = total.checked_add(amount.checked_mul(1_000)?)?;
            break;
        }
        if only_bare_value_remains {
            return None;
        }
        let (rank, multiplier, suffix_bytes) = if remaining.starts_with(b"ms") {
            (7_u8, 1_u64, 2_usize)
        } else {
            match remaining[0] {
                b'w' => (2, 7 * 86_400_000, 1),
                b'd' => (3, 86_400_000, 1),
                b'h' => (4, 3_600_000, 1),
                b'm' => (5, 60_000, 1),
                b's' => (6, 1_000, 1),
                _ => return None,
            }
        };
        if previous_rank.is_some_and(|previous| rank <= previous) {
            return None;
        }
        previous_rank = Some(rank);
        remaining = &remaining[suffix_bytes..];
        total = total.checked_add(amount.checked_mul(multiplier)?)?;
        if remaining.first() == Some(&b' ') {
            if rank >= 6 {
                return None;
            }
            remaining = remaining.trim_ascii_start();
            only_bare_value_remains = true;
        }
    }
    (total > 0 && total <= MAX_CANONICAL_INTEGER).then_some(total)
}

fn parse_port(value: &[u8]) -> Option<u16> {
    let port = std::str::from_utf8(value).ok()?.parse::<u16>().ok()?;
    (port != 0).then_some(port)
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
    if first_port != second_port {
        return false;
    }
    let Some(first_ip) = listen_endpoint_ip(first_address) else {
        return false;
    };
    let Some(second_ip) = listen_endpoint_ip(second_address) else {
        return false;
    };
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
    ip_address(address)
}

fn include_failure(edge: &IncludeEdge) -> Option<DiagnosticCode> {
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

fn has_variable(value: &[u8]) -> bool {
    value
        .windows(2)
        .any(|window| window[0] == b'$' && window[1] != b'$')
}

fn ascii_lowercase(value: &[u8]) -> Vec<u8> {
    value.to_ascii_lowercase()
}
