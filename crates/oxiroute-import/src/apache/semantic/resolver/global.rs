#[allow(clippy::wildcard_imports)]
use super::*;

impl<'a> Resolver<'a> {
    pub(super) fn new(graph: &'a SourceGraph) -> Self {
        Self {
            graph,
            defaults: ApacheDefaults::default(),
            dispositions: HashMap::new(),
            diagnostics: Vec::new(),
            listens: Vec::new(),
            virtual_hosts: Vec::new(),
            balancers: Vec::new(),
            module_loads: Vec::new(),
        }
    }

    pub(super) fn run(mut self) -> Report<ApacheResolution> {
        for directive in self.graph.expanded_directives.clone() {
            if lower_name(&directive.directive.name.value) != "virtualhost" {
                self.resolve_global(&directive);
            }
        }
        for directive in self.graph.expanded_directives.clone() {
            if lower_name(&directive.directive.name.value) == "virtualhost" {
                self.resolve_virtual_host(&directive);
            }
        }
        self.check_listener_references();
        self.check_virtual_host_identities();
        self.classify_remaining();
        let decisions = self
            .graph
            .expanded_occurrences
            .iter()
            .map(|occurrence| OccurrenceDecision {
                occurrence: occurrence.id,
                parent: occurrence.parent,
                name: occurrence.directive.name.clone(),
                arguments: occurrence.directive.arguments.clone(),
                span: occurrence.directive.span,
                provenance: occurrence.provenance.clone(),
                disposition: self
                    .dispositions
                    .get(&occurrence.id)
                    .copied()
                    .unwrap_or(OccurrenceDisposition::Structural),
            })
            .collect();
        Report::new(
            ApacheResolution {
                listens: self.listens,
                virtual_hosts: self.virtual_hosts,
                balancers: self.balancers,
                module_loads: self.module_loads,
                decisions,
            },
            self.diagnostics,
        )
    }

    fn resolve_global(&mut self, directive: &ExpandedDirective) {
        match lower_name(&directive.directive.name.value).as_str() {
            "listen" => self.resolve_listen(directive),
            "proxy" => self.resolve_balancer(directive),
            "loadmodule" => self.resolve_module_load(directive),
            "servername" => self.resolve_global_server_name(directive),
            "proxypass" => match parse_proxy_pass(directive) {
                Ok(mut proxy) => {
                    proxy.inherited = true;
                    self.defaults.proxy_passes.push(proxy);
                    self.resolved(directive.occurrence);
                }
                Err((code, message)) => self.block(directive, code, message),
            },
            "proxypreservehost" => match directive.directive.arguments.as_slice() {
                [value] if value.value.eq_ignore_ascii_case(b"on") => {
                    self.defaults.preserve_host = true;
                    self.defaults.preserve_host_origin = Some(origin(directive));
                    self.resolved(directive.occurrence);
                }
                [value] if value.value.eq_ignore_ascii_case(b"off") => {
                    self.defaults.preserve_host = false;
                    self.defaults.preserve_host_origin = Some(origin(directive));
                    self.resolved(directive.occurrence);
                }
                _ => self.block(
                    directive,
                    E_INVALID_VALUE,
                    "ProxyPreserveHost requires one explicit on or off policy",
                ),
            },
            "sslengine" => match directive.directive.arguments.as_slice() {
                [value] if value.value.eq_ignore_ascii_case(b"on") => {
                    self.defaults.tls.engine_on = true;
                    self.defaults.tls.engine_origin = Some(origin(directive));
                    self.defaults.tls.engine_inherited = true;
                    self.resolved(directive.occurrence);
                }
                [value] if value.value.eq_ignore_ascii_case(b"off") => {
                    self.defaults.tls.engine_on = false;
                    self.defaults.tls.engine_origin = Some(origin(directive));
                    self.defaults.tls.engine_inherited = true;
                    self.resolved(directive.occurrence);
                }
                _ => self.block(
                    directive,
                    E_INVALID_VALUE,
                    "SSLEngine must be `on` or `off`",
                ),
            },
            "sslcertificatefile" => {
                self.resolve_global_certificate(directive, true);
            }
            "sslcertificatekeyfile" => {
                self.resolve_global_certificate(directive, false);
            }
            "rewriteengine" => match directive.directive.arguments.as_slice() {
                [value] if value.value.eq_ignore_ascii_case(b"on") => {
                    self.defaults.rewrite_engine = Some((true, origin(directive)));
                    self.resolved(directive.occurrence);
                }
                [value] if value.value.eq_ignore_ascii_case(b"off") => {
                    self.defaults.rewrite_engine = Some((false, origin(directive)));
                    self.resolved(directive.occurrence);
                }
                _ => self.block(
                    directive,
                    E_INVALID_VALUE,
                    "RewriteEngine must be `on` or `off`",
                ),
            },
            "rewritecond" | "rewritemap" | "rewriterule" => self.block(
                directive,
                E_REWRITE_UNSUPPORTED,
                "Apache rewrite behavior is outside the static importer subset",
            ),
            "proxypassmatch" => self.block(
                directive,
                E_DYNAMIC_PROXY_PASS,
                "ProxyPassMatch uses regular-expression routing and cannot be lowered",
            ),
            "proxypassreverse" => self.block(
                directive,
                E_UNSUPPORTED_DIRECTIVE,
                "ProxyPassReverse response-location rewriting has no exact canonical field",
            ),
            "balancerpersist" | "balancergrowth" | "balancerinherit" => self.block(
                directive,
                E_DYNAMIC_BALANCER_MANAGER,
                "Apache balancer state is dynamic and cannot be imported from static source",
            ),
            "include" | "includeoptional" | "namevirtualhost" => {
                self.structural(directive.occurrence);
            }
            _ => self.block_subtree(
                directive,
                E_UNSUPPORTED_DIRECTIVE,
                "Apache directive is outside the static importer subset",
            ),
        }
    }

    fn resolve_global_server_name(&mut self, directive: &ExpandedDirective) {
        if directive.directive.arguments.len() != 1 {
            self.block(
                directive,
                E_INVALID_VALUE,
                "ServerName requires exactly one exact host name",
            );
            return;
        }
        let Some(value) = word_text(&directive.directive.arguments[0]) else {
            self.block(directive, E_INVALID_VALUE, "Apache host name is not UTF-8");
            return;
        };
        if value.is_empty() || has_dynamic(&value) || value.contains('*') {
            self.block(
                directive,
                E_UNSUPPORTED_FEATURE,
                "Apache ServerName must be an exact static host name",
            );
            return;
        }
        if split_host_port(&value)
            .and_then(|(host, _)| canonical_host(&host))
            .is_none()
        {
            self.block(
                directive,
                E_INVALID_VALUE,
                "Apache ServerName is not a canonical host or authority",
            );
            return;
        }
        if self.defaults.server_name.is_some() {
            self.block(
                directive,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "Apache global ServerName is declared more than once",
            );
            return;
        }
        self.defaults.server_name = Some(DefaultServerName {
            value,
            origin: origin(directive),
        });
        self.resolved(directive.occurrence);
    }

    fn resolve_global_certificate(&mut self, directive: &ExpandedDirective, chain: bool) {
        if directive.directive.arguments.len() != 1 {
            self.block(
                directive,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "Apache TLS requires one unambiguous certificate path",
            );
            return;
        }
        let Some(path) = absolute_path(&directive.directive.arguments[0]) else {
            self.block(
                directive,
                E_INVALID_VALUE,
                "Apache TLS path must be a canonical absolute UTF-8 path",
            );
            return;
        };
        let already_set = if chain {
            self.defaults.tls.certificate_chain.is_some()
        } else {
            self.defaults.tls.private_key.is_some()
        };
        if already_set {
            self.block(
                directive,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "Apache TLS requires one unambiguous certificate path",
            );
            return;
        }
        let effective_path = EffectivePath {
            path,
            origin: origin(directive),
            inherited: true,
        };
        if chain {
            self.defaults.tls.certificate_chain = Some(effective_path);
        } else {
            self.defaults.tls.private_key = Some(effective_path);
        }
        self.resolved(directive.occurrence);
    }

    fn resolve_listen(&mut self, directive: &ExpandedDirective) {
        let Some(address) = parse_explicit_socket(&directive.directive.arguments) else {
            self.block(
                directive,
                E_INVALID_VALUE,
                "Apache Listen requires one explicit IP address and port",
            );
            return;
        };
        if self
            .listens
            .iter()
            .any(|listen| socket_addresses_overlap(listen.address, address))
        {
            self.block(
                directive,
                E_DUPLICATE_IDENTITY,
                "duplicate Apache Listen endpoint",
            );
            return;
        }
        self.resolved(directive.occurrence);
        self.listens.push(EffectiveListen {
            origin: origin(directive),
            address,
        });
    }

    fn resolve_module_load(&mut self, directive: &ExpandedDirective) {
        let arguments = &directive.directive.arguments;
        let Some(module) = arguments.first().and_then(word_text) else {
            self.block(
                directive,
                E_INVALID_VALUE,
                "LoadModule requires a module name and library path",
            );
            return;
        };
        if arguments.len() != 2 {
            self.block(
                directive,
                E_INVALID_VALUE,
                "LoadModule requires exactly a module name and library path",
            );
            return;
        }
        if !supported_module(&module) {
            self.block(
                directive,
                E_UNSUPPORTED_MODULE,
                format!("Apache module `{module}` is outside the supported capability profile"),
            );
            return;
        }
        let path = word_text(&arguments[1]).unwrap_or_default();
        self.resolved(directive.occurrence);
        self.module_loads.push(EffectiveModuleLoad {
            origin: origin(directive),
            module: module.clone(),
            path,
        });
    }
}
