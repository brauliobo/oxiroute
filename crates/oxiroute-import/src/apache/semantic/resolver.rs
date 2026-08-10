#[must_use]
pub fn resolve(loaded: Report<SourceGraph>) -> Report<ApacheResolution> {
    let (graph, mut diagnostics) = loaded.into_parts();
    let (resolution, semantic_diagnostics) = Resolver::new(&graph).run().into_parts();
    diagnostics.extend(semantic_diagnostics);
    Report::new(resolution, diagnostics)
}

struct Resolver<'a> {
    graph: &'a SourceGraph,
    defaults: ApacheDefaults,
    dispositions: HashMap<OccurrenceId, OccurrenceDisposition>,
    diagnostics: Vec<Diagnostic>,
    listens: Vec<EffectiveListen>,
    virtual_hosts: Vec<EffectiveVirtualHost>,
    balancers: Vec<EffectiveBalancer>,
    module_loads: Vec<EffectiveModuleLoad>,
}

#[derive(Clone, Debug, Default)]
struct ApacheDefaults {
    server_name: Option<DefaultServerName>,
    proxy_passes: Vec<EffectiveProxyPass>,
    tls: EffectiveTls,
    preserve_host: bool,
    preserve_host_origin: Option<DirectiveOrigin>,
    rewrite_engine: Option<(bool, DirectiveOrigin)>,
}

#[derive(Clone, Debug)]
struct DefaultServerName {
    value: String,
    origin: DirectiveOrigin,
}

impl<'a> Resolver<'a> {
    fn new(graph: &'a SourceGraph) -> Self {
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

    fn run(mut self) -> Report<ApacheResolution> {
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

    #[allow(clippy::too_many_lines)]
    fn resolve_virtual_host(&mut self, directive: &ExpandedDirective) {
        let Some(addresses) = parse_virtual_host_sockets(&directive.directive.arguments) else {
            self.block(
                directive,
                E_INVALID_VALUE,
                "VirtualHost requires one or more explicit IP addresses and ports",
            );
            return;
        };
        let address = addresses[0];
        let Some(children) = directive.children.as_deref() else {
            self.block(directive, E_INVALID_VALUE, "VirtualHost must be a block");
            return;
        };
        let mut names = Vec::new();
        let mut server_name_count = 0;
        let mut proxy_passes = self.defaults.proxy_passes.clone();
        let mut tls = self.defaults.tls.clone();
        let mut preserve_host = self.defaults.preserve_host;
        let mut preserve_host_origin = self.defaults.preserve_host_origin.clone();
        let mut preserve_host_inherited = preserve_host_origin.is_some();
        let mut preserve_host_set = false;
        let mut ssl_engine_set = false;
        let mut certificate_chain_set = false;
        let mut private_key_set = false;
        let mut rewrite_engine = self.defaults.rewrite_engine.clone();
        let mut local_rewrite_engine_set = false;

        self.resolved(directive.occurrence);
        for child in children {
            let name = lower_name(&child.directive.name.value);
            match name.as_str() {
                "servername" => {
                    server_name_count += 1;
                    if child.directive.arguments.len() != 1 {
                        self.block(
                            child,
                            E_INVALID_VALUE,
                            "ServerName requires exactly one exact host name",
                        );
                        continue;
                    }
                    match parse_server_name(&child.directive.arguments[0], address.port()) {
                        Ok(name) => names.push(EffectiveServerName {
                            origin: origin(child),
                            selector: name.selector,
                            certificate_name: name.certificate_name,
                            inherited: false,
                        }),
                        Err((code, message)) => self.block(child, code, message),
                    }
                }
                "serveralias" => {
                    if child.directive.arguments.is_empty() {
                        self.block(
                            child,
                            E_INVALID_VALUE,
                            "ServerAlias requires at least one exact host name",
                        );
                        continue;
                    }
                    for argument in &child.directive.arguments {
                        match parse_server_name(argument, address.port()) {
                            Ok(name) => names.push(EffectiveServerName {
                                origin: origin(child),
                                selector: name.selector,
                                certificate_name: name.certificate_name,
                                inherited: false,
                            }),
                            Err((code, message)) => self.block(child, code, message),
                        }
                    }
                    self.resolved(child.occurrence);
                }
                "sslengine" => match child.directive.arguments.as_slice() {
                    [value] if value.value.eq_ignore_ascii_case(b"on") && !ssl_engine_set => {
                        tls.engine_on = true;
                        tls.engine_origin = Some(origin(child));
                        tls.engine_inherited = false;
                        ssl_engine_set = true;
                        self.resolved(child.occurrence);
                    }
                    [value] if value.value.eq_ignore_ascii_case(b"off") && !ssl_engine_set => {
                        tls.engine_on = false;
                        tls.engine_origin = Some(origin(child));
                        tls.engine_inherited = false;
                        ssl_engine_set = true;
                        self.resolved(child.occurrence);
                    }
                    [..] if ssl_engine_set => self.block(
                        child,
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "duplicate SSLEngine policy is not representable",
                    ),
                    _ => self.block(child, E_INVALID_VALUE, "SSLEngine must be `on` or `off`"),
                },
                "sslcertificatefile" => {
                    if child.directive.arguments.len() != 1 || certificate_chain_set {
                        self.block(
                            child,
                            E_SEMANTICS_NOT_REPRESENTABLE,
                            "Apache TLS requires one unambiguous SSLCertificateFile",
                        );
                    } else if let Some(path) = absolute_path(&child.directive.arguments[0]) {
                        tls.certificate_chain = Some(EffectivePath {
                            path,
                            origin: origin(child),
                            inherited: false,
                        });
                        certificate_chain_set = true;
                        self.resolved(child.occurrence);
                    } else {
                        self.block(
                            child,
                            E_INVALID_VALUE,
                            "SSLCertificateFile must be a canonical absolute UTF-8 path",
                        );
                    }
                }
                "sslcertificatekeyfile" => {
                    if child.directive.arguments.len() != 1 || private_key_set {
                        self.block(
                            child,
                            E_SEMANTICS_NOT_REPRESENTABLE,
                            "Apache TLS requires one unambiguous SSLCertificateKeyFile",
                        );
                    } else if let Some(path) = absolute_path(&child.directive.arguments[0]) {
                        tls.private_key = Some(EffectivePath {
                            path,
                            origin: origin(child),
                            inherited: false,
                        });
                        private_key_set = true;
                        self.resolved(child.occurrence);
                    } else {
                        self.block(
                            child,
                            E_INVALID_VALUE,
                            "SSLCertificateKeyFile must be a canonical absolute UTF-8 path",
                        );
                    }
                }
                "proxypass" => match parse_proxy_pass(child) {
                    Ok(proxy) => {
                        proxy_passes.push(proxy);
                        self.resolved(child.occurrence);
                    }
                    Err((code, message)) => self.block(child, code, message),
                },
                "proxypassmatch" => self.block(
                    child,
                    E_DYNAMIC_PROXY_PASS,
                    "ProxyPassMatch uses regular-expression routing and cannot be lowered",
                ),
                "proxypassreverse" => self.block(
                    child,
                    E_UNSUPPORTED_DIRECTIVE,
                    "ProxyPassReverse response-location rewriting has no exact canonical field",
                ),
                "proxypreservehost" => match child.directive.arguments.as_slice() {
                    [value] if value.value.eq_ignore_ascii_case(b"on") && !preserve_host_set => {
                        preserve_host = true;
                        preserve_host_origin = Some(origin(child));
                        preserve_host_inherited = false;
                        preserve_host_set = true;
                        self.resolved(child.occurrence);
                    }
                    [value] if value.value.eq_ignore_ascii_case(b"off") && !preserve_host_set => {
                        preserve_host = false;
                        preserve_host_origin = Some(origin(child));
                        preserve_host_inherited = false;
                        preserve_host_set = true;
                        self.resolved(child.occurrence);
                    }
                    _ => self.block(
                        child,
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "duplicate or invalid ProxyPreserveHost policy is not representable",
                    ),
                },
                "rewriteengine" => match child.directive.arguments.as_slice() {
                    [value]
                        if !local_rewrite_engine_set
                            && value.value.eq_ignore_ascii_case(b"off") =>
                    {
                        rewrite_engine = Some((false, origin(child)));
                        local_rewrite_engine_set = true;
                        self.resolved(child.occurrence);
                    }
                    [value]
                        if !local_rewrite_engine_set && value.value.eq_ignore_ascii_case(b"on") =>
                    {
                        rewrite_engine = Some((true, origin(child)));
                        local_rewrite_engine_set = true;
                        self.resolved(child.occurrence);
                    }
                    [..] if local_rewrite_engine_set => self.block(
                        child,
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "duplicate RewriteEngine policy is not representable",
                    ),
                    _ => self.block(
                        child,
                        E_INVALID_VALUE,
                        "RewriteEngine must be `on` or `off`",
                    ),
                },
                "rewriterule" | "rewritecond" | "rewritemap" => self.block(
                    child,
                    E_REWRITE_UNSUPPORTED,
                    "Apache rewrite behavior is outside the static importer subset",
                ),
                "sethandler"
                    if child.directive.arguments.len() == 1
                        && child.directive.arguments[0]
                            .value
                            .eq_ignore_ascii_case(b"balancer-manager") =>
                {
                    self.block(
                        child,
                        E_DYNAMIC_BALANCER_MANAGER,
                        "Apache balancer-manager state is dynamic and cannot be imported",
                    );
                }
                "directory" | "directorymatch" | "location" | "locationmatch" | "files"
                | "filesmatch" => self.block_subtree(
                    child,
                    E_DIRECTORY_MERGE,
                    "Apache directory or location merge changes routing outside the flat subset",
                ),
                "ifmodule" | "ifdefine" | "if" => self.block_subtree(
                    child,
                    E_UNSUPPORTED_MODULE,
                    "conditional Apache module configuration is not statically decidable",
                ),
                "balancerpersist" | "balancergrowth" | "balancerinherit" => self.block(
                    child,
                    E_DYNAMIC_BALANCER_MANAGER,
                    "Apache balancer state is dynamic and cannot be imported from static source",
                ),
                _ => self.block_subtree(
                    child,
                    E_UNSUPPORTED_DIRECTIVE,
                    "Apache virtual-host directive is outside the static importer subset",
                ),
            }
        }

        if server_name_count == 0
            && let Some(default) = &self.defaults.server_name
        {
            match parse_server_name_text(&default.value, address.port()) {
                Ok(name) => names.push(EffectiveServerName {
                    origin: default.origin.clone(),
                    selector: name.selector,
                    certificate_name: name.certificate_name,
                    inherited: true,
                }),
                Err((code, message)) => self.block(
                    directive,
                    code,
                    format!("inherited Apache ServerName is invalid: {message}"),
                ),
            }
        }
        if server_name_count > 1 || names.is_empty() {
            self.block(
                directive,
                E_UNRESOLVED_REFERENCE,
                "each Apache VirtualHost requires exactly one effective ServerName",
            );
        }
        if proxy_passes.is_empty() {
            self.block(
                directive,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "Apache VirtualHost has no lowerable static ProxyPass rule",
            );
        }
        if tls.engine_on && (tls.certificate_chain.is_none() || tls.private_key.is_none()) {
            self.block(
                directive,
                E_UNRESOLVED_REFERENCE,
                "TLS-enabled Apache VirtualHost requires certificate and private-key references",
            );
        }
        if !tls.engine_on {
            let has_local_certificate = tls
                .certificate_chain
                .as_ref()
                .is_some_and(|path| !path.inherited)
                || tls.private_key.as_ref().is_some_and(|path| !path.inherited);
            if has_local_certificate {
                self.block(
                    directive,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "Apache certificate references are present while SSLEngine is off",
                );
            }
        }
        if let (Some(chain), Some(key)) = (&tls.certificate_chain, &tls.private_key)
            && chain.path == key.path
        {
            self.block(
                directive,
                E_INVALID_VALUE,
                "Apache certificate and private-key paths must differ",
            );
        }
        if rewrite_engine.as_ref().is_some_and(|(enabled, _)| *enabled) {
            self.block(
                directive,
                E_REWRITE_UNSUPPORTED,
                "Apache rewrite behavior is enabled by an inherited or local RewriteEngine",
            );
        }
        let blocked = Self::subtree_occurrences(directive)
            .filter_map(|occurrence| {
                self.dispositions.get(&occurrence).and_then(|disposition| {
                    if let OccurrenceDisposition::Blocking(code) = disposition {
                        Some(*code)
                    } else {
                        None
                    }
                })
            })
            .collect::<Vec<_>>();
        self.virtual_hosts.push(EffectiveVirtualHost {
            origin: origin(directive),
            address,
            addresses,
            names,
            proxy_passes,
            tls,
            preserve_host,
            preserve_host_origin,
            preserve_host_inherited,
            blocked,
        });
    }

    #[allow(clippy::too_many_lines)]
    fn resolve_balancer(&mut self, directive: &ExpandedDirective) {
        let Some(argument) = directive.directive.arguments.first() else {
            self.block(
                directive,
                E_INVALID_VALUE,
                "Apache <Proxy> requires a balancer:// name",
            );
            return;
        };
        let Some(value) = word_text(argument) else {
            self.block(
                directive,
                E_INVALID_VALUE,
                "Apache balancer name is not UTF-8",
            );
            return;
        };
        let Some(name) = value.strip_prefix("balancer://") else {
            self.block(
                directive,
                E_UNSUPPORTED_DIRECTIVE,
                "only static balancer:// Proxy blocks are supported",
            );
            return;
        };
        if directive.directive.arguments.len() != 1 || name.is_empty() || has_dynamic(name) {
            self.block(
                directive,
                E_DYNAMIC_PROXY_PASS,
                "dynamic or wildcard balancer names cannot be lowered",
            );
            return;
        }
        if self.balancers.iter().any(|balancer| balancer.name == name) {
            self.block(
                directive,
                E_DUPLICATE_IDENTITY,
                "duplicate Apache balancer identity",
            );
            return;
        }
        let Some(children) = directive.children.as_deref() else {
            self.block(directive, E_INVALID_VALUE, "balancer Proxy must be a block");
            return;
        };
        let mut members = Vec::new();
        let mut identities = HashSet::new();
        self.resolved(directive.occurrence);
        for child in children {
            match lower_name(&child.directive.name.value).as_str() {
                "balancermember" => {
                    let parsed = child
                        .directive
                        .arguments
                        .first()
                        .and_then(|word| parse_member_target(word).ok());
                    let Some(member) = parsed else {
                        self.block(
                            child,
                            E_DYNAMIC_PROXY_PASS,
                            "BalancerMember must use a static HTTP or HTTPS destination",
                        );
                        continue;
                    };
                    if child.directive.arguments.len() > 2
                        || child.directive.arguments.get(1).is_some_and(|option| {
                            !word_text(option)
                                .is_some_and(|value| value.eq_ignore_ascii_case("loadfactor=1"))
                        })
                    {
                        self.block(
                            child,
                            E_SEMANTICS_NOT_REPRESENTABLE,
                            "only equal-weight byrequests BalancerMember options are supported",
                        );
                        continue;
                    }
                    let identity = format!(
                        "{}://{}:{}",
                        member.scheme.as_str(),
                        member.host,
                        member.port
                    );
                    if !identities.insert(identity) {
                        self.block(
                            child,
                            E_DUPLICATE_IDENTITY,
                            "duplicate Apache balancer member",
                        );
                        continue;
                    }
                    self.resolved(child.occurrence);
                    members.push(EffectiveBalancerMember {
                        origin: origin(child),
                        endpoint: member.endpoint,
                        host: member.host,
                        port: member.port,
                        scheme: member.scheme,
                    });
                }
                "proxyset" => {
                    if child.directive.arguments.len() == 1
                        && child.directive.arguments[0]
                            .value
                            .eq_ignore_ascii_case(b"lbmethod=byrequests")
                    {
                        self.resolved(child.occurrence);
                    } else {
                        self.block(
                            child,
                            E_SEMANTICS_NOT_REPRESENTABLE,
                            "Apache balancers require the equal-weight byrequests algorithm",
                        );
                    }
                }
                "balancerpersist" | "balancergrowth" | "balancerinherit" => self.block(
                    child,
                    E_DYNAMIC_BALANCER_MANAGER,
                    "Apache balancer state is dynamic and cannot be imported from static source",
                ),
                _ => self.block_subtree(
                    child,
                    E_UNSUPPORTED_DIRECTIVE,
                    "Apache balancer directive is outside the static importer subset",
                ),
            }
        }
        if members.is_empty() {
            self.block(
                directive,
                E_UNRESOLVED_REFERENCE,
                "Apache balancer requires at least one static member",
            );
        }
        self.balancers.push(EffectiveBalancer {
            origin: origin(directive),
            name: name.to_owned(),
            members,
        });
    }

    fn check_listener_references(&mut self) {
        for virtual_host in &mut self.virtual_hosts {
            if !virtual_host.addresses.iter().all(|address| {
                self.listens
                    .iter()
                    .any(|listen| socket_addresses_overlap(listen.address, *address))
            }) {
                self.diagnostics.push(
                    Diagnostic::new(
                        E_UNRESOLVED_REFERENCE,
                        Severity::Error,
                        DiagnosticStage::Resolve,
                        "Apache VirtualHost has no matching explicit Listen endpoint",
                    )
                    .with_primary_span(virtual_host.origin.span)
                    .with_include_stack(
                        virtual_host
                            .origin
                            .provenance
                            .include_stack
                            .iter()
                            .map(|frame| frame.directive_span),
                    ),
                );
                virtual_host.blocked.push(E_UNRESOLVED_REFERENCE);
            }
        }
        for listen in &self.listens {
            if !self.virtual_hosts.iter().any(|virtual_host| {
                virtual_host
                    .addresses
                    .iter()
                    .any(|address| socket_addresses_overlap(*address, listen.address))
            }) {
                self.diagnostics.push(
                    Diagnostic::new(
                        E_UNRESOLVED_REFERENCE,
                        Severity::Error,
                        DiagnosticStage::Resolve,
                        "Apache Listen endpoint has no imported VirtualHost",
                    )
                    .with_primary_span(listen.origin.span)
                    .with_include_stack(
                        listen
                            .origin
                            .provenance
                            .include_stack
                            .iter()
                            .map(|frame| frame.directive_span),
                    ),
                );
            }
        }
    }

    fn check_virtual_host_identities(&mut self) {
        for first in 0..self.virtual_hosts.len() {
            for second in first + 1..self.virtual_hosts.len() {
                if !self.virtual_hosts[first]
                    .addresses
                    .iter()
                    .any(|first_address| {
                        self.virtual_hosts[second]
                            .addresses
                            .iter()
                            .any(|second_address| {
                                socket_addresses_overlap(*first_address, *second_address)
                            })
                    })
                {
                    continue;
                }
                let first_names = self.virtual_hosts[first].names.clone();
                let second_names = self.virtual_hosts[second].names.clone();
                for first_name in first_names {
                    for second_name in &second_names {
                        if selector_key(&first_name.selector) != selector_key(&second_name.selector)
                        {
                            continue;
                        }
                        let first_origin = self.virtual_hosts[first].origin.clone();
                        let origin = second_name.origin.clone();
                        let diagnostic = Diagnostic::new(
                            E_AMBIGUOUS_VHOST,
                            Severity::Error,
                            DiagnosticStage::Resolve,
                            "Apache virtual hosts have an ambiguous listener and host identity",
                        )
                        .with_primary_span(origin.span)
                        .with_related_span(first_origin.span, "first virtual host claim is here")
                        .with_include_stack(
                            origin
                                .provenance
                                .include_stack
                                .iter()
                                .map(|frame| frame.directive_span),
                        );
                        self.diagnostics.push(diagnostic);
                        self.virtual_hosts[second].blocked.push(E_AMBIGUOUS_VHOST);
                        self.virtual_hosts[first].blocked.push(E_AMBIGUOUS_VHOST);
                    }
                }
            }
        }
    }

    fn classify_remaining(&mut self) {
        for occurrence in &self.graph.expanded_occurrences {
            self.dispositions
                .entry(occurrence.id)
                .or_insert(OccurrenceDisposition::Structural);
        }
    }

    fn subtree_occurrences(
        directive: &ExpandedDirective,
    ) -> Box<dyn Iterator<Item = OccurrenceId> + '_> {
        let mut occurrences = vec![directive.occurrence];
        if let Some(children) = &directive.children {
            occurrences.extend(children.iter().flat_map(|child| {
                let mut values = vec![child.occurrence];
                if let Some(children) = &child.children {
                    values.extend(children.iter().map(|nested| nested.occurrence));
                }
                values
            }));
        }
        Box::new(occurrences.into_iter())
    }

    fn resolved(&mut self, occurrence: OccurrenceId) {
        self.dispositions
            .entry(occurrence)
            .or_insert(OccurrenceDisposition::Resolved);
    }

    fn structural(&mut self, occurrence: OccurrenceId) {
        self.dispositions
            .entry(occurrence)
            .or_insert(OccurrenceDisposition::Structural);
    }

    fn block(
        &mut self,
        directive: &ExpandedDirective,
        code: DiagnosticCode,
        message: impl Into<String>,
    ) {
        let already_blocked = matches!(
            self.dispositions.get(&directive.occurrence),
            Some(OccurrenceDisposition::Blocking(_))
        );
        self.dispositions
            .insert(directive.occurrence, OccurrenceDisposition::Blocking(code));
        if already_blocked {
            return;
        }
        self.diagnostics.push(
            Diagnostic::new(code, Severity::Error, DiagnosticStage::Resolve, message)
                .with_primary_span(directive.directive.span)
                .with_include_stack(
                    directive
                        .provenance
                        .include_stack
                        .iter()
                        .map(|frame| frame.directive_span),
                ),
        );
    }

    fn block_subtree(
        &mut self,
        directive: &ExpandedDirective,
        code: DiagnosticCode,
        message: &'static str,
    ) {
        self.block(directive, code, message);
        if let Some(children) = &directive.children {
            for child in children {
                self.block_subtree(child, code, message);
            }
        }
    }
}

fn origin(directive: &ExpandedDirective) -> DirectiveOrigin {
    DirectiveOrigin {
        occurrence: directive.occurrence,
        span: directive.directive.span,
        provenance: directive.provenance.clone(),
    }
}

fn lower_name(value: &[u8]) -> String {
    String::from_utf8_lossy(value).to_ascii_lowercase()
}

fn word_text(word: &Word) -> Option<String> {
    std::str::from_utf8(&word.value).ok().map(str::to_owned)
}

fn absolute_path(word: &Word) -> Option<PathBuf> {
    let value = std::str::from_utf8(&word.value).ok()?;
    let path = std::path::Path::new(value);
    (path.is_absolute()
        && !value.is_empty()
        && !value.contains("//")
        && !value.ends_with('/')
        && !value
            .strip_prefix('/')
            .unwrap_or(value)
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == ".."))
    .then(|| path.to_path_buf())
}

fn parse_explicit_socket(arguments: &[Word]) -> Option<SocketAddr> {
    if arguments.len() != 1 {
        return None;
    }
    let value = word_text(&arguments[0])?;
    if value.contains('$') {
        return None;
    }
    let value = value
        .strip_prefix("*:")
        .map_or_else(|| value.clone(), |port| format!("0.0.0.0:{port}"));
    let address = value.parse::<SocketAddr>().ok()?;
    (address.port() != 0).then_some(address)
}

fn parse_virtual_host_sockets(arguments: &[Word]) -> Option<Vec<SocketAddr>> {
    if arguments.is_empty() {
        return None;
    }
    let mut addresses = Vec::with_capacity(arguments.len());
    for argument in arguments {
        let address = parse_explicit_socket(std::slice::from_ref(argument))?;
        if !addresses.contains(&address) {
            addresses.push(address);
        }
    }
    (!addresses.is_empty()).then_some(addresses)
}

fn socket_addresses_overlap(first: SocketAddr, second: SocketAddr) -> bool {
    first.port() == second.port()
        && (first.ip() == second.ip()
            || first.ip().is_unspecified()
            || second.ip().is_unspecified())
}

struct ParsedServerName {
    selector: HttpHostSelector,
    certificate_name: String,
}

fn parse_server_name(
    word: &Word,
    listener_port: u16,
) -> Result<ParsedServerName, (DiagnosticCode, String)> {
    let value =
        word_text(word).ok_or_else(|| (E_INVALID_VALUE, "Apache host name is not UTF-8".into()))?;
    parse_server_name_text(&value, listener_port)
}

fn parse_server_name_text(
    value: &str,
    listener_port: u16,
) -> Result<ParsedServerName, (DiagnosticCode, String)> {
    if value.is_empty() || has_dynamic(value) || value.contains('*') {
        return Err((
            E_UNSUPPORTED_FEATURE,
            "Apache ServerName/ServerAlias must be an exact static host name".into(),
        ));
    }
    let (host, port) = split_host_port(value).ok_or_else(|| {
        (
            E_INVALID_VALUE,
            "Apache ServerName/ServerAlias is not a canonical host or authority".into(),
        )
    })?;
    let host = canonical_host(&host).ok_or_else(|| {
        (
            E_INVALID_VALUE,
            "Apache ServerName/ServerAlias is not a canonical DNS name or IP address".into(),
        )
    })?;
    if let Some(port) = port {
        if port != listener_port {
            return Err((
                E_SEMANTICS_NOT_REPRESENTABLE,
                "Apache host authority port does not match its VirtualHost listener".into(),
            ));
        }
        let authority = format_authority(&host, port);
        Ok(ParsedServerName {
            selector: HttpHostSelector::AsciiCaseInsensitiveExactAuthority { value: authority },
            certificate_name: host,
        })
    } else {
        Ok(ParsedServerName {
            selector: HttpHostSelector::NormalizedHost {
                value: host.clone(),
            },
            certificate_name: host,
        })
    }
}

fn canonical_host(value: &str) -> Option<String> {
    if let Ok(ip) = value.parse::<IpAddr>() {
        return Some(ip.to_string());
    }
    let value = value.strip_suffix('.').unwrap_or(value);
    dns_name(value.as_bytes())
}

fn split_host_port(value: &str) -> Option<(String, Option<u16>)> {
    if let Some(rest) = value.strip_prefix('[') {
        let (host, port) = rest.split_once(']')?;
        let port = if let Some(port) = port.strip_prefix(':') {
            Some(port.parse().ok()?)
        } else {
            None
        };
        return Some((host.to_owned(), port));
    }
    let mut pieces = value.rsplitn(2, ':');
    let last = pieces.next()?;
    let first = pieces.next();
    match first {
        Some(host) if last.bytes().all(|byte| byte.is_ascii_digit()) => {
            Some((host.to_owned(), Some(last.parse().ok()?)))
        }
        Some(_) => None,
        None => Some((last.to_owned(), None)),
    }
}

fn format_authority(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn parse_proxy_pass(
    directive: &ExpandedDirective,
) -> Result<EffectiveProxyPass, (DiagnosticCode, String)> {
    let arguments = &directive.directive.arguments;
    if arguments.len() != 2 {
        return Err((
            E_DYNAMIC_PROXY_PASS,
            "ProxyPass options and interpolation are outside the static importer subset".into(),
        ));
    }
    let path = word_text(&arguments[0])
        .ok_or_else(|| (E_INVALID_VALUE, "ProxyPass path is not UTF-8".into()))?;
    if !path.starts_with('/') || path.contains('?') || path.contains('#') {
        return Err((
            E_DYNAMIC_PROXY_PASS,
            "ProxyPass requires a static absolute request path".into(),
        ));
    }
    let Some(path) = oxiroute_config::canonicalize_http_path(&path)
        .filter(|canonical| canonical.as_ref() == path)
        .map(std::borrow::Cow::into_owned)
    else {
        return Err((
            E_SEMANTICS_NOT_REPRESENTABLE,
            "ProxyPass request path has ambiguous canonical normalization".into(),
        ));
    };
    let target = parse_proxy_target(&arguments[1])?;
    let target_path = match &target {
        ProxyTarget::Direct { target_path, .. } | ProxyTarget::Balancer { target_path, .. } => {
            target_path
        }
    };
    if target_path != &path {
        return Err((
            E_SEMANTICS_NOT_REPRESENTABLE,
            "ProxyPass URI replacement is not represented by the canonical proxy action".into(),
        ));
    }
    Ok(EffectiveProxyPass {
        origin: origin(directive),
        path,
        target,
        inherited: false,
    })
}

fn parse_proxy_target(word: &Word) -> Result<ProxyTarget, (DiagnosticCode, String)> {
    let value = word_text(word)
        .ok_or_else(|| (E_INVALID_VALUE, "ProxyPass destination is not UTF-8".into()))?;
    if has_dynamic(&value) || value.contains('?') || value.contains('#') {
        return Err((
            E_DYNAMIC_PROXY_PASS,
            "ProxyPass destination contains dynamic interpolation or query semantics".into(),
        ));
    }
    if let Some(rest) = value.strip_prefix("balancer://") {
        let (name, path) = split_target_path(rest);
        if name.is_empty() || has_dynamic(name) {
            return Err((
                E_DYNAMIC_PROXY_PASS,
                "ProxyPass balancer name is dynamic".into(),
            ));
        }
        return Ok(ProxyTarget::Balancer {
            name: name.to_owned(),
            target_path: path,
        });
    }
    let (scheme, rest) = if let Some(rest) = value.strip_prefix("http://") {
        (ProxyScheme::Http, rest)
    } else if let Some(rest) = value.strip_prefix("https://") {
        (ProxyScheme::Https, rest)
    } else {
        return Err((
            E_UNSUPPORTED_FEATURE,
            "ProxyPass destination must use static http://, https://, or balancer:// syntax".into(),
        ));
    };
    let (authority, target_path) = split_target_path(rest);
    let (host, explicit_port) = split_host_port(authority).ok_or_else(|| {
        (
            E_INVALID_VALUE,
            "ProxyPass destination authority is invalid".into(),
        )
    })?;
    if host.is_empty() || authority.contains('@') {
        return Err((
            E_INVALID_VALUE,
            "ProxyPass destination authority is invalid".into(),
        ));
    }
    let host = canonical_host(&host).ok_or_else(|| {
        (
            E_INVALID_VALUE,
            "ProxyPass destination host is not a canonical DNS name or IP address".into(),
        )
    })?;
    let port = explicit_port.unwrap_or(match scheme {
        ProxyScheme::Http => 80,
        ProxyScheme::Https => 443,
    });
    if port == 0 {
        return Err((
            E_INVALID_VALUE,
            "ProxyPass destination port is invalid".into(),
        ));
    }
    let endpoint = if let Ok(address) = host.parse::<IpAddr>() {
        UpstreamEndpoint::Socket {
            address: SocketAddr::new(address, port),
        }
    } else {
        UpstreamEndpoint::Dns {
            host: host.clone(),
            port,
        }
    };
    Ok(ProxyTarget::Direct {
        scheme,
        endpoint,
        host,
        port,
        target_path,
    })
}

struct ParsedMemberTarget {
    scheme: ProxyScheme,
    endpoint: UpstreamEndpoint,
    host: String,
    port: u16,
}

fn parse_member_target(word: &Word) -> Result<ParsedMemberTarget, ()> {
    let value = word_text(word).ok_or(())?;
    let (scheme, rest) = if let Some(rest) = value.strip_prefix("http://") {
        (ProxyScheme::Http, rest)
    } else if let Some(rest) = value.strip_prefix("https://") {
        (ProxyScheme::Https, rest)
    } else {
        return Err(());
    };
    let (authority, path) = split_target_path(rest);
    if path != "/" {
        return Err(());
    }
    let (host, explicit_port) = split_host_port(authority).ok_or(())?;
    let host = canonical_host(&host).ok_or(())?;
    let port = explicit_port.unwrap_or(match scheme {
        ProxyScheme::Http => 80,
        ProxyScheme::Https => 443,
    });
    if port == 0 {
        return Err(());
    }
    let endpoint = if let Ok(address) = host.parse::<IpAddr>() {
        UpstreamEndpoint::Socket {
            address: SocketAddr::new(address, port),
        }
    } else {
        UpstreamEndpoint::Dns {
            host: host.clone(),
            port,
        }
    };
    Ok(ParsedMemberTarget {
        scheme,
        endpoint,
        host,
        port,
    })
}

fn split_target_path(value: &str) -> (&str, String) {
    value
        .split_once('/')
        .map_or((value, "/".into()), |(authority, path)| {
            (authority, format!("/{path}"))
        })
}

fn selector_key(selector: &HttpHostSelector) -> String {
    match selector {
        HttpHostSelector::NormalizedHost { value } => format!("host:{value}"),
        HttpHostSelector::AsciiCaseInsensitiveExactAuthority { value } => {
            format!("authority:{}", value.to_ascii_lowercase())
        }
        HttpHostSelector::ExactAuthority { value } => format!("authority:{value}"),
        HttpHostSelector::NginxLeadingWildcard { value }
        | HttpHostSelector::NginxLeadingDot { value } => format!("unsupported:{value}"),
    }
}

fn supported_module(module: &str) -> bool {
    [
        "authz_core_module",
        "lbmethod_byrequests_module",
        "log_config_module",
        "mpm_event_module",
        "mpm_prefork_module",
        "proxy_balancer_module",
        "proxy_http_module",
        "proxy_module",
        "slotmem_shm_module",
        "socache_shmcb_module",
        "ssl_module",
        "unixd_module",
    ]
    .contains(&module.to_ascii_lowercase().as_str())
}

fn has_dynamic(value: &str) -> bool {
    value.contains('$') || value.contains('%') || value.contains("${") || value.contains("#{")
}

impl ProxyScheme {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }
}
