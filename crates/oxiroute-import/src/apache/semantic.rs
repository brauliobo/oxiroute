use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, SocketAddr},
    path::PathBuf,
};

use oxiroute_config::{HttpHostSelector, UpstreamEndpoint};

use crate::{
    Diagnostic, DiagnosticCode, DiagnosticStage, E_DUPLICATE_IDENTITY, E_INVALID_VALUE,
    E_SEMANTICS_NOT_REPRESENTABLE, E_UNRESOLVED_REFERENCE, E_UNSUPPORTED_FEATURE, Report, Severity,
    Span, canonical::dns_name,
};

use super::{
    E_AMBIGUOUS_VHOST, E_DIRECTORY_MERGE, E_DYNAMIC_BALANCER_MANAGER, E_DYNAMIC_PROXY_PASS,
    E_REWRITE_UNSUPPORTED, E_UNSUPPORTED_DIRECTIVE, E_UNSUPPORTED_MODULE, ExpandedDirective,
    IncludeFrame, OccurrenceId, Provenance, SourceGraph, Word,
};

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
    pub names: Vec<EffectiveServerName>,
    pub proxy_passes: Vec<EffectiveProxyPass>,
    pub tls: EffectiveTls,
    pub preserve_host: bool,
    pub blocked: Vec<DiagnosticCode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveServerName {
    pub origin: DirectiveOrigin,
    pub selector: HttpHostSelector,
    pub certificate_name: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EffectiveTls {
    pub engine_on: bool,
    pub certificate_chain: Option<EffectivePath>,
    pub private_key: Option<EffectivePath>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectivePath {
    pub path: PathBuf,
    pub origin: DirectiveOrigin,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveProxyPass {
    pub origin: DirectiveOrigin,
    pub path: String,
    pub target: ProxyTarget,
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

#[must_use]
pub fn resolve(loaded: Report<SourceGraph>) -> Report<ApacheResolution> {
    let (graph, mut diagnostics) = loaded.into_parts();
    let (resolution, semantic_diagnostics) = Resolver::new(&graph).run().into_parts();
    diagnostics.extend(semantic_diagnostics);
    Report::new(resolution, diagnostics)
}

struct Resolver<'a> {
    graph: &'a SourceGraph,
    dispositions: HashMap<OccurrenceId, OccurrenceDisposition>,
    diagnostics: Vec<Diagnostic>,
    listens: Vec<EffectiveListen>,
    virtual_hosts: Vec<EffectiveVirtualHost>,
    balancers: Vec<EffectiveBalancer>,
    module_loads: Vec<EffectiveModuleLoad>,
}

impl<'a> Resolver<'a> {
    fn new(graph: &'a SourceGraph) -> Self {
        Self {
            graph,
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
            match lower_name(&directive.directive.name.value).as_str() {
                "listen" => self.resolve_listen(&directive),
                "virtualhost" => self.resolve_virtual_host(&directive),
                "proxy" => self.resolve_balancer(&directive),
                "loadmodule" => self.resolve_module_load(&directive),
                "include" | "includeoptional" | "namevirtualhost" => {
                    self.structural(directive.occurrence);
                }
                "balancerpersist" | "balancergrowth" | "balancerinherit" => self.block(
                    &directive,
                    E_DYNAMIC_BALANCER_MANAGER,
                    "Apache balancer state is dynamic and cannot be imported from static source",
                ),
                _ => self.block_subtree(
                    &directive,
                    E_UNSUPPORTED_DIRECTIVE,
                    "Apache directive is outside the static importer subset",
                ),
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

    fn resolve_listen(&mut self, directive: &ExpandedDirective) {
        let Some(address) = parse_explicit_socket(&directive.directive.arguments) else {
            self.block(
                directive,
                E_INVALID_VALUE,
                "Apache Listen requires one explicit IP address and port",
            );
            return;
        };
        if self.listens.iter().any(|listen| listen.address == address) {
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
        let Some(address) = parse_explicit_socket(&directive.directive.arguments) else {
            self.block(
                directive,
                E_INVALID_VALUE,
                "VirtualHost requires exactly one explicit IP address and port",
            );
            return;
        };
        let Some(children) = directive.children.as_deref() else {
            self.block(directive, E_INVALID_VALUE, "VirtualHost must be a block");
            return;
        };
        let mut names = Vec::new();
        let mut server_name_count = 0;
        let mut proxy_passes = Vec::new();
        let mut tls = EffectiveTls::default();
        let mut preserve_host = false;
        let mut preserve_host_set = false;

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
                            }),
                            Err((code, message)) => self.block(child, code, message),
                        }
                    }
                    self.resolved(child.occurrence);
                }
                "sslengine" => match child.directive.arguments.as_slice() {
                    [value] if value.value.eq_ignore_ascii_case(b"on") => {
                        tls.engine_on = true;
                        self.resolved(child.occurrence);
                    }
                    [value] if value.value.eq_ignore_ascii_case(b"off") => {
                        tls.engine_on = false;
                        self.resolved(child.occurrence);
                    }
                    _ => self.block(child, E_INVALID_VALUE, "SSLEngine must be `on` or `off`"),
                },
                "sslcertificatefile" => {
                    if child.directive.arguments.len() != 1 || tls.certificate_chain.is_some() {
                        self.block(
                            child,
                            E_SEMANTICS_NOT_REPRESENTABLE,
                            "Apache TLS requires one unambiguous SSLCertificateFile",
                        );
                    } else if let Some(path) = absolute_path(&child.directive.arguments[0]) {
                        tls.certificate_chain = Some(EffectivePath {
                            path,
                            origin: origin(child),
                        });
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
                    if child.directive.arguments.len() != 1 || tls.private_key.is_some() {
                        self.block(
                            child,
                            E_SEMANTICS_NOT_REPRESENTABLE,
                            "Apache TLS requires one unambiguous SSLCertificateKeyFile",
                        );
                    } else if let Some(path) = absolute_path(&child.directive.arguments[0]) {
                        tls.private_key = Some(EffectivePath {
                            path,
                            origin: origin(child),
                        });
                        self.resolved(child.occurrence);
                    } else {
                        self.block(
                            child,
                            E_INVALID_VALUE,
                            "SSLCertificateKeyFile must be a canonical absolute UTF-8 path",
                        );
                    }
                }
                "proxypass" => match parse_proxy_pass(child, address.port()) {
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
                        preserve_host_set = true;
                        self.resolved(child.occurrence);
                    }
                    [value] if value.value.eq_ignore_ascii_case(b"off") && !preserve_host_set => {
                        preserve_host_set = true;
                        self.resolved(child.occurrence);
                    }
                    _ => self.block(
                        child,
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "duplicate or invalid ProxyPreserveHost policy is not representable",
                    ),
                },
                "rewriteengine"
                    if child.directive.arguments.len() == 1
                        && child.directive.arguments[0]
                            .value
                            .eq_ignore_ascii_case(b"off") =>
                {
                    self.resolved(child.occurrence);
                }
                "rewriteengine" | "rewriterule" | "rewritecond" | "rewritemap" => self.block(
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

        if server_name_count != 1 {
            self.block(
                directive,
                E_UNRESOLVED_REFERENCE,
                "each Apache VirtualHost requires exactly one ServerName",
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
        if !tls.engine_on && (tls.certificate_chain.is_some() || tls.private_key.is_some()) {
            self.block(
                directive,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "Apache certificate references are present while SSLEngine is off",
            );
        }
        if let (Some(chain), Some(key)) = (&tls.certificate_chain, &tls.private_key) {
            if chain.path == key.path {
                self.block(
                    directive,
                    E_INVALID_VALUE,
                    "Apache certificate and private-key paths must differ",
                );
            }
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
            names,
            proxy_passes,
            tls,
            preserve_host,
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
                            word_text(option).as_deref() != Some("loadfactor=1")
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
        let listen_addresses = self
            .listens
            .iter()
            .map(|listen| listen.address)
            .collect::<HashSet<_>>();
        for virtual_host in &mut self.virtual_hosts {
            if !listen_addresses.contains(&virtual_host.address) {
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
            if !self
                .virtual_hosts
                .iter()
                .any(|virtual_host| virtual_host.address == listen.address)
            {
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
        let mut seen = HashMap::<(SocketAddr, String), usize>::new();
        for index in 0..self.virtual_hosts.len() {
            let address = self.virtual_hosts[index].address;
            let names = self.virtual_hosts[index].names.clone();
            for name in names {
                let key = (address, selector_key(&name.selector));
                if let Some(first) = seen.insert(key, index) {
                    let first_origin = self.virtual_hosts[first].origin.clone();
                    let origin = name.origin.clone();
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
                    self.virtual_hosts[index].blocked.push(E_AMBIGUOUS_VHOST);
                    self.virtual_hosts[first].blocked.push(E_AMBIGUOUS_VHOST);
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
    if value.contains('*') || value.contains('$') {
        return None;
    }
    let address = value.parse::<SocketAddr>().ok()?;
    (address.port() != 0).then_some(address)
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
    if value.is_empty() || has_dynamic(&value) || value.contains('*') {
        return Err((
            E_UNSUPPORTED_FEATURE,
            "Apache ServerName/ServerAlias must be an exact static host name".into(),
        ));
    }
    let (host, port) = split_host_port(&value).ok_or_else(|| {
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
    listener_port: u16,
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
    let target = parse_proxy_target(&arguments[1], listener_port)?;
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
    })
}

fn parse_proxy_target(
    word: &Word,
    listener_port: u16,
) -> Result<ProxyTarget, (DiagnosticCode, String)> {
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
    if port == 0 || port == listener_port && host.is_empty() {
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
