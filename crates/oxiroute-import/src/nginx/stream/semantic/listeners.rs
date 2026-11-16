#[allow(clippy::wildcard_imports)]
use super::*;

impl Resolver<'_> {
    #[expect(
        clippy::too_many_lines,
        reason = "stream server declarations and representability blockers resolve in source order"
    )]
    pub(super) fn resolve_server(
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
        let mut proxy_protocol = false;
        let mut proxy_protocol_origin = None;
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
                    if self.resolve_on_or_off_flag(child, "nginx stream PROXY protocol") {
                        proxy_protocol = child
                            .directive
                            .arguments
                            .first()
                            .is_some_and(|value| value.value == b"on");
                        proxy_protocol_origin = Some(Self::origin(child));
                    }
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
        if proxy_protocol && listens.iter().any(|listen| !listen.proxy_protocol) {
            self.block(
                directive.occurrence,
                E_UNSUPPORTED_FEATURE,
                "nginx stream proxy_protocol on requires proxy_protocol on every listen endpoint for exact explicit propagation",
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
            proxy_protocol,
            proxy_protocol_origin,
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
        let mut proxy_protocol = false;
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
                b"ssl" => {
                    outcome = Some((
                        E_UNSUPPORTED_FEATURE,
                        "nginx stream listener TLS options are unsupported",
                    ));
                }
                b"proxy_protocol" => proxy_protocol = true,
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
            proxy_protocol,
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
}
