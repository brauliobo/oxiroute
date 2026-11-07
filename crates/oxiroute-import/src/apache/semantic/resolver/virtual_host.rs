#[allow(clippy::wildcard_imports)]
use super::*;

impl Resolver<'_> {
    #[allow(clippy::too_many_lines)]
    pub(super) fn resolve_virtual_host(&mut self, directive: &ExpandedDirective) {
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
}
