#[allow(clippy::wildcard_imports)]
use super::*;

impl<'a> Resolver<'a> {
    pub(super) fn new(graph: &'a SourceGraph, complete_root: bool) -> Self {
        Self {
            graph,
            dispositions: vec![None; graph.expanded_occurrences.len()],
            diagnostics: Vec::new(),
            complete_root,
        }
    }

    pub(super) fn run(mut self) -> Report<StreamResolution> {
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

    #[expect(
        clippy::too_many_lines,
        reason = "stream declarations and representability blockers resolve in source order"
    )]
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
                    if child.directive.arguments.len() == 1
                        && child.children.is_some()
                        && let Some(name) = &upstream.name
                    {
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
}
