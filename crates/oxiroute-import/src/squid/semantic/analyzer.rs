use std::collections::HashMap;

use crate::canonical::dns_name;
use crate::{
    Diagnostic, DiagnosticStage, E_DUPLICATE_IDENTITY, E_UNRESOLVED_REFERENCE,
    E_UNSUPPORTED_FEATURE, Report, Severity,
};

use super::super::{
    E_UNCONSUMED_DIRECTIVE, E_UNKNOWN_DIRECTIVE, E_UNSUPPORTED_FORM, ExpandedDirective,
    OccurrenceId, SourceGraph, Word, bytes,
};
use super::model::{
    AccessAction, AccessListKind, AccessPolicy, AccessRule, AclDefinition, AclMatcher,
    AclReferenceResolution, AclTerm, AclType, Activation, AuthenticationHelper,
    AuthenticationParameter, AuthenticationRealm, AuthenticationScheme, AuthenticationSetting,
    AuthenticationValue, BuiltinAcl, CacheDirective, CachePeer, CachePeerType, Decision,
    DecisionLedger, DecisionOutcome, DirectiveFamily, DirectiveOrigin, DirectiveResolution,
    DirectiveSemantics, DnsNameservers, EffectiveAcl, EffectiveConfiguration, ForwardedForMode,
    LogDestination, LoggingDirective, NativeValue, OpaqueDirective, PeerOption, PortDirective,
    PortKind, PortOption, PrivacyDirective, ProcessDirective, ProxyAuthMatcher, RefreshOption,
    RefreshPattern, SecretFact, SecretKind, SemanticBlockerKind, StorageDirective,
};

#[must_use]
pub fn analyze(graph: &SourceGraph) -> Report<EffectiveConfiguration> {
    Analyzer::new(graph).run()
}

/// Resolves a loaded graph while preserving every source, lexical, and parse diagnostic.
#[must_use]
pub fn analyze_loaded(loaded: Report<SourceGraph>) -> Report<EffectiveConfiguration> {
    let (graph, mut diagnostics) = loaded.into_parts();
    let (effective, semantic_diagnostics) = analyze(&graph).into_parts();
    diagnostics.extend(semantic_diagnostics);
    Report::new(effective, diagnostics)
}

struct Analyzer<'a> {
    graph: &'a SourceGraph,
    effective: EffectiveConfiguration,
    decisions: Vec<Option<Decision>>,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Analyzer<'a> {
    fn new(graph: &'a SourceGraph) -> Self {
        Self {
            graph,
            effective: EffectiveConfiguration::default(),
            decisions: vec![None; graph.expanded_directives.len()],
            diagnostics: Vec::new(),
        }
    }

    fn run(mut self) -> Report<EffectiveConfiguration> {
        for expanded in &self.graph.expanded_directives {
            self.classify(expanded);
        }
        self.merge_acls();
        self.resolve_access();
        self.resolve_direct_access();
        self.resolve_authentication();
        self.terminal_accounting();
        Report::new(self.effective, self.diagnostics)
    }

    fn classify(&mut self, expanded: &ExpandedDirective) {
        let name = expanded.directive.name.value.as_slice();
        match name {
            b"include" => self.include(expanded),
            b"acl" => self.acl(expanded),
            b"request_header_access" | b"reply_header_access" => self.header_access(expanded),
            name if access_kind(name).is_some() => self.access(expanded),
            b"http_port" => self.port(expanded, PortKind::Http),
            b"https_port" => self.port(expanded, PortKind::Https),
            b"icp_port" => self.port(expanded, PortKind::Icp),
            b"htcp_port" => self.port(expanded, PortKind::Htcp),
            b"cache_peer" => self.cache_peer(expanded),
            b"refresh_pattern" => self.refresh(expanded),
            name if is_cache_policy(name) => self.cache(expanded),
            name if is_storage(name) => self.storage(expanded),
            b"auth_param" => self.auth_param(expanded),
            name if is_authentication(name) => self.authentication_control(expanded),
            b"access_log" => self.access_log(expanded),
            name if is_logging(name) => self.logging_control(expanded),
            b"dns_nameservers" => self.dns_nameservers(expanded),
            name if is_dns(name) => self.dns_control(expanded),
            b"forwarded_for" => self.forwarded_for(expanded),
            b"via" => self.via(expanded),
            b"request_header_replace" | b"reply_header_replace" => self.header_replace(expanded),
            name if is_process(name) => self.process(expanded),
            _ => self.unknown(expanded),
        }
    }

    fn include(&mut self, expanded: &ExpandedDirective) {
        let activation = self
            .graph
            .includes
            .iter()
            .find(|edge| edge.occurrence == expanded.occurrence)
            .filter(|edge| edge.failure.is_some())
            .map_or(Activation::Structural, |_| {
                Activation::Blocked(SemanticBlockerKind::IncludeExpansion)
            });
        self.record(
            expanded,
            DirectiveFamily::Include,
            DirectiveSemantics::Include,
            activation,
        );
    }

    fn acl(&mut self, expanded: &ExpandedDirective) {
        let [name, acl_type, values @ ..] = expanded.directive.arguments.as_slice() else {
            self.invalid(
                expanded,
                DirectiveFamily::Acl,
                DirectiveSemantics::AclUnsupported,
                "Squid acl requires a name, type, and value",
            );
            return;
        };
        let (acl_type, matchers, semantics, blocker) = match acl_type.value.as_slice() {
            b"src" => (
                AclType::Source,
                values
                    .iter()
                    .map(|value| bytes::ip_network(&value.value).map(AclMatcher::Source))
                    .collect::<Option<Vec<_>>>(),
                DirectiveSemantics::AclSource,
                SemanticBlockerKind::SourceAddressAcl,
            ),
            b"port" => (
                AclType::Port,
                values
                    .iter()
                    .map(|value| bytes::port_range(&value.value).map(AclMatcher::Port))
                    .collect::<Option<Vec<_>>>(),
                DirectiveSemantics::AclPort,
                SemanticBlockerKind::DestinationPortAcl,
            ),
            b"proxy_auth" => (
                AclType::ProxyAuth,
                Some(values.iter().map(proxy_auth_matcher).collect()),
                DirectiveSemantics::AclProxyAuth,
                SemanticBlockerKind::ProxyAuthenticationAcl,
            ),
            _ => {
                self.diagnostics.push(Self::diagnostic(
                    expanded,
                    E_UNSUPPORTED_FEATURE,
                    "Squid ACL type is not registered by the strict importer",
                ));
                self.record(
                    expanded,
                    DirectiveFamily::Acl,
                    DirectiveSemantics::AclUnsupported,
                    Activation::Blocked(SemanticBlockerKind::UnsupportedAclType),
                );
                return;
            }
        };
        let Some(matchers) = matchers.filter(|matchers| !matchers.is_empty()) else {
            self.invalid(
                expanded,
                DirectiveFamily::Acl,
                semantics,
                "Squid acl contains no valid matcher",
            );
            return;
        };
        self.effective.acl_definitions.push(AclDefinition {
            origin: origin(expanded),
            name: name.into(),
            acl_type,
            matchers,
        });
        self.record(
            expanded,
            DirectiveFamily::Acl,
            semantics,
            Activation::Blocked(blocker),
        );
    }

    fn access(&mut self, expanded: &ExpandedDirective) {
        let kind = access_kind(&expanded.directive.name.value)
            .expect("classifier only calls access for registered access directives");
        self.parse_access(expanded, kind, None, &expanded.directive.arguments);
    }

    fn header_access(&mut self, expanded: &ExpandedDirective) {
        let [selector, arguments @ ..] = expanded.directive.arguments.as_slice() else {
            self.invalid(
                expanded,
                DirectiveFamily::Access,
                DirectiveSemantics::HeaderAccess,
                "Squid header access requires a header selector and action",
            );
            return;
        };
        let kind = if expanded.directive.name.value == b"request_header_access" {
            AccessListKind::RequestHeader
        } else {
            AccessListKind::ReplyHeader
        };
        self.parse_access(expanded, kind, Some(selector.into()), arguments);
    }

    fn parse_access(
        &mut self,
        expanded: &ExpandedDirective,
        kind: AccessListKind,
        selector: Option<NativeValue>,
        arguments: &[Word],
    ) {
        let [action, terms @ ..] = arguments else {
            self.invalid(
                expanded,
                DirectiveFamily::Access,
                access_semantics(kind),
                "Squid access rule requires an allow or deny action",
            );
            return;
        };
        let action = match action.value.as_slice() {
            b"allow" => AccessAction::Allow,
            b"deny" => AccessAction::Deny,
            _ => {
                self.invalid(
                    expanded,
                    DirectiveFamily::Access,
                    access_semantics(kind),
                    "Squid access rule action must be allow or deny",
                );
                return;
            }
        };
        if terms.is_empty() {
            self.invalid(
                expanded,
                DirectiveFamily::Access,
                access_semantics(kind),
                "Squid access rule requires at least one ACL term",
            );
            return;
        }
        let terms = terms
            .iter()
            .map(|term| {
                let (negated, value) = term.value.strip_prefix(b"!").map_or_else(
                    || (false, term.value.clone()),
                    |value| (true, value.to_vec()),
                );
                AclTerm {
                    negated,
                    name: NativeValue {
                        value,
                        span: term.span,
                    },
                    resolution: AclReferenceResolution::Unresolved,
                }
            })
            .collect();
        self.effective.access_rules.push(AccessRule {
            origin: origin(expanded),
            kind,
            selector,
            action,
            terms,
            order: self.effective.access_rules.len(),
        });
        self.record(
            expanded,
            DirectiveFamily::Access,
            access_semantics(kind),
            Activation::Blocked(access_blocker(kind)),
        );
    }

    fn port(&mut self, expanded: &ExpandedDirective, kind: PortKind) {
        let Some((endpoint, options)) = expanded.directive.arguments.split_first() else {
            self.invalid(
                expanded,
                DirectiveFamily::Port,
                port_semantics(kind),
                "Squid port directive requires an endpoint",
            );
            return;
        };
        let Some(endpoint) = bytes::port_endpoint(&endpoint.value) else {
            self.invalid(
                expanded,
                DirectiveFamily::Port,
                port_semantics(kind),
                "Squid port endpoint is invalid",
            );
            return;
        };
        let options = options.iter().map(parse_port_option).collect::<Vec<_>>();
        let blocker = if options.is_empty() {
            SemanticBlockerKind::ForwardProxyListener
        } else {
            self.diagnostics.push(Self::diagnostic(
                expanded,
                E_UNSUPPORTED_FEATURE,
                "Squid port contains an unsupported option",
            ));
            SemanticBlockerKind::UnsupportedPortOption
        };
        self.effective.ports.push(PortDirective {
            origin: origin(expanded),
            kind,
            endpoint,
            options,
        });
        self.record(
            expanded,
            DirectiveFamily::Port,
            port_semantics(kind),
            Activation::Blocked(blocker),
        );
    }

    fn cache_peer(&mut self, expanded: &ExpandedDirective) {
        let [host, peer_type, http_port, icp_port, options @ ..] =
            expanded.directive.arguments.as_slice()
        else {
            self.invalid(
                expanded,
                DirectiveFamily::CachePeer,
                DirectiveSemantics::CachePeer,
                "Squid cache_peer requires host, type, HTTP port, and ICP port",
            );
            return;
        };
        let peer_type = match peer_type.value.as_slice() {
            b"parent" => CachePeerType::Parent,
            b"sibling" => CachePeerType::Sibling,
            b"multicast" => CachePeerType::Multicast,
            _ => {
                self.invalid(
                    expanded,
                    DirectiveFamily::CachePeer,
                    DirectiveSemantics::CachePeer,
                    "Squid cache_peer type is invalid",
                );
                return;
            }
        };
        let (Some(http_port), Some(icp_port)) = (
            bytes::unsigned(&http_port.value),
            bytes::unsigned(&icp_port.value),
        ) else {
            self.invalid(
                expanded,
                DirectiveFamily::CachePeer,
                DirectiveSemantics::CachePeer,
                "Squid cache_peer ports are invalid",
            );
            return;
        };
        let peer = CachePeer {
            origin: origin(expanded),
            host: host.into(),
            peer_type,
            http_port,
            icp_port,
            options: options.iter().map(parse_peer_option).collect(),
        };
        let activation = if is_static_parent_peer(&peer) {
            Activation::Structural
        } else {
            Activation::Blocked(SemanticBlockerKind::CachePeerHierarchy)
        };
        self.effective.cache_peers.push(peer);
        self.record(
            expanded,
            DirectiveFamily::CachePeer,
            DirectiveSemantics::CachePeer,
            activation,
        );
    }

    fn refresh(&mut self, expanded: &ExpandedDirective) {
        let (case_insensitive, arguments) = expanded
            .directive
            .arguments
            .first()
            .filter(|argument| argument.value == b"-i")
            .map_or((false, expanded.directive.arguments.as_slice()), |_| {
                (true, &expanded.directive.arguments[1..])
            });
        let [pattern, minimum, percent, maximum, options @ ..] = arguments else {
            self.invalid(
                expanded,
                DirectiveFamily::Refresh,
                DirectiveSemantics::RefreshPattern,
                "Squid refresh_pattern requires pattern, minimum, percent, and maximum",
            );
            return;
        };
        let (Some(minimum), Some(percent), Some(maximum), Some(options)) = (
            bytes::minutes(&minimum.value),
            bytes::percent(&percent.value),
            bytes::minutes(&maximum.value),
            options
                .iter()
                .map(parse_refresh_option)
                .collect::<Option<Vec<_>>>(),
        ) else {
            self.invalid(
                expanded,
                DirectiveFamily::Refresh,
                DirectiveSemantics::RefreshPattern,
                "Squid refresh_pattern contains an invalid numeric value or option",
            );
            return;
        };
        self.effective.refresh_policy.patterns.push(RefreshPattern {
            origin: origin(expanded),
            case_insensitive,
            pattern: pattern.into(),
            minimum,
            percent,
            maximum,
            options,
        });
        self.record(
            expanded,
            DirectiveFamily::Refresh,
            DirectiveSemantics::RefreshPattern,
            Activation::Externalized,
        );
    }

    fn auth_param(&mut self, expanded: &ExpandedDirective) {
        let [scheme, setting, values @ ..] = expanded.directive.arguments.as_slice() else {
            self.invalid(
                expanded,
                DirectiveFamily::Authentication,
                DirectiveSemantics::AuthenticationSetting,
                "Squid auth_param requires a scheme and setting",
            );
            return;
        };
        let Some((setting, semantics, value)) = self.parse_auth_value(expanded, setting, values)
        else {
            return;
        };
        self.effective.authentication.push(AuthenticationParameter {
            origin: origin(expanded),
            scheme: scheme.into(),
            setting,
            value,
        });
        self.record(
            expanded,
            DirectiveFamily::Authentication,
            semantics,
            Activation::Blocked(SemanticBlockerKind::ProxyAuthentication),
        );
    }

    fn parse_auth_value(
        &mut self,
        expanded: &ExpandedDirective,
        setting: &Word,
        values: &[Word],
    ) -> Option<(
        AuthenticationSetting,
        DirectiveSemantics,
        AuthenticationValue,
    )> {
        Some(match setting.value.as_slice() {
            b"program" if !values.is_empty() => (
                AuthenticationSetting::Program,
                DirectiveSemantics::AuthenticationHelper,
                AuthenticationValue::Helper(SecretFact {
                    kind: SecretKind::AuthenticationHelper,
                    span: bytes::words_span(values).expect("program values are nonempty"),
                }),
            ),
            b"realm" if !values.is_empty() => (
                AuthenticationSetting::Realm,
                DirectiveSemantics::AuthenticationRealm,
                AuthenticationValue::Realm(SecretFact {
                    kind: SecretKind::AuthenticationRealm,
                    span: bytes::words_span(values).expect("realm values are nonempty"),
                }),
            ),
            b"credentialsttl" => {
                let [value, unit] = values else {
                    self.invalid(
                        expanded,
                        DirectiveFamily::Authentication,
                        DirectiveSemantics::AuthenticationCredentialTtl,
                        "Squid auth credentialsttl requires a value and unit",
                    );
                    return None;
                };
                let Some(duration) = bytes::duration(&value.value, &unit.value) else {
                    self.invalid(
                        expanded,
                        DirectiveFamily::Authentication,
                        DirectiveSemantics::AuthenticationCredentialTtl,
                        "Squid auth credentialsttl is invalid",
                    );
                    return None;
                };
                (
                    AuthenticationSetting::CredentialTtl,
                    DirectiveSemantics::AuthenticationCredentialTtl,
                    AuthenticationValue::Duration(duration),
                )
            }
            b"children" | b"concurrency" if values.len() == 1 => {
                let Some(count) = bytes::unsigned(&values[0].value) else {
                    self.invalid(
                        expanded,
                        DirectiveFamily::Authentication,
                        DirectiveSemantics::AuthenticationSetting,
                        "Squid auth count is invalid",
                    );
                    return None;
                };
                let setting = if setting.value == b"children" {
                    AuthenticationSetting::Children
                } else {
                    AuthenticationSetting::Concurrency
                };
                (
                    setting,
                    DirectiveSemantics::AuthenticationSetting,
                    AuthenticationValue::Count(count),
                )
            }
            b"casesensitive" if values.len() == 1 => {
                let Some(value) = bytes::boolean(&values[0].value) else {
                    self.invalid(
                        expanded,
                        DirectiveFamily::Authentication,
                        DirectiveSemantics::AuthenticationSetting,
                        "Squid auth casesensitive value is invalid",
                    );
                    return None;
                };
                (
                    AuthenticationSetting::CaseSensitive,
                    DirectiveSemantics::AuthenticationSetting,
                    AuthenticationValue::Boolean(value),
                )
            }
            _ => (
                AuthenticationSetting::Other,
                DirectiveSemantics::AuthenticationSetting,
                AuthenticationValue::Opaque {
                    argument_count: values.len(),
                },
            ),
        })
    }

    fn access_log(&mut self, expanded: &ExpandedDirective) {
        let Some((destination, rest)) = expanded.directive.arguments.split_first() else {
            self.invalid(
                expanded,
                DirectiveFamily::Logging,
                DirectiveSemantics::AccessLogging,
                "Squid access_log requires a destination",
            );
            return;
        };
        let destination = parse_log_destination(destination);
        self.effective.logging.push(LoggingDirective {
            origin: origin(expanded),
            destination,
            format: rest.first().map(Into::into),
        });
        self.record(
            expanded,
            DirectiveFamily::Logging,
            DirectiveSemantics::AccessLogging,
            Activation::Blocked(SemanticBlockerKind::AccessLoggingPolicy),
        );
    }

    fn dns_nameservers(&mut self, expanded: &ExpandedDirective) {
        let addresses = expanded
            .directive
            .arguments
            .iter()
            .map(|value| std::str::from_utf8(&value.value).ok()?.parse().ok())
            .collect::<Option<Vec<_>>>();
        let Some(addresses) = addresses.filter(|addresses| !addresses.is_empty()) else {
            self.invalid(
                expanded,
                DirectiveFamily::Dns,
                DirectiveSemantics::DnsNameservers,
                "Squid dns_nameservers requires valid IP addresses",
            );
            return;
        };
        self.effective.dns_nameservers.push(DnsNameservers {
            origin: origin(expanded),
            addresses,
        });
        self.record(
            expanded,
            DirectiveFamily::Dns,
            DirectiveSemantics::DnsNameservers,
            Activation::Blocked(SemanticBlockerKind::ResolverPolicy),
        );
    }

    fn forwarded_for(&mut self, expanded: &ExpandedDirective) {
        let [value] = expanded.directive.arguments.as_slice() else {
            self.invalid(
                expanded,
                DirectiveFamily::Privacy,
                DirectiveSemantics::ForwardedFor,
                "Squid forwarded_for requires one mode",
            );
            return;
        };
        let mode = match value.value.as_slice() {
            b"on" => ForwardedForMode::On,
            b"off" => ForwardedForMode::Off,
            b"transparent" => ForwardedForMode::Transparent,
            b"delete" => ForwardedForMode::Delete,
            b"truncate" => ForwardedForMode::Truncate,
            _ => {
                self.invalid(
                    expanded,
                    DirectiveFamily::Privacy,
                    DirectiveSemantics::ForwardedFor,
                    "Squid forwarded_for mode is invalid",
                );
                return;
            }
        };
        self.effective.privacy.push(PrivacyDirective::ForwardedFor {
            origin: origin(expanded),
            mode,
        });
        self.record(
            expanded,
            DirectiveFamily::Privacy,
            DirectiveSemantics::ForwardedFor,
            Activation::Blocked(SemanticBlockerKind::ForwardedForPolicy),
        );
    }

    fn via(&mut self, expanded: &ExpandedDirective) {
        let [value] = expanded.directive.arguments.as_slice() else {
            self.invalid(
                expanded,
                DirectiveFamily::Privacy,
                DirectiveSemantics::Via,
                "Squid via requires on or off",
            );
            return;
        };
        let Some(enabled) = bytes::boolean(&value.value) else {
            self.invalid(
                expanded,
                DirectiveFamily::Privacy,
                DirectiveSemantics::Via,
                "Squid via requires on or off",
            );
            return;
        };
        self.effective.privacy.push(PrivacyDirective::Via {
            origin: origin(expanded),
            enabled,
        });
        self.record(
            expanded,
            DirectiveFamily::Privacy,
            DirectiveSemantics::Via,
            Activation::Blocked(SemanticBlockerKind::ViaPolicy),
        );
    }

    fn header_replace(&mut self, expanded: &ExpandedDirective) {
        let [name, replacement @ ..] = expanded.directive.arguments.as_slice() else {
            self.invalid(
                expanded,
                DirectiveFamily::Privacy,
                DirectiveSemantics::HeaderPrivacy,
                "Squid header replacement requires a header name",
            );
            return;
        };
        self.effective
            .privacy
            .push(PrivacyDirective::HeaderReplace {
                origin: origin(expanded),
                request: expanded.directive.name.value == b"request_header_replace",
                name: name.into(),
                replacement: replacement.iter().map(Into::into).collect(),
            });
        self.record(
            expanded,
            DirectiveFamily::Privacy,
            DirectiveSemantics::HeaderPrivacy,
            Activation::Blocked(SemanticBlockerKind::HeaderPrivacyPolicy),
        );
    }

    fn cache(&mut self, expanded: &ExpandedDirective) {
        let directive = if expanded.directive.name.value == b"cache_mem" {
            let [value, unit] = expanded.directive.arguments.as_slice() else {
                self.invalid(
                    expanded,
                    DirectiveFamily::CachePolicy,
                    DirectiveSemantics::CacheSetting,
                    "Squid cache_mem requires a value and unit",
                );
                return;
            };
            let Some(bytes) = bytes::byte_size(&value.value, &unit.value) else {
                self.invalid(
                    expanded,
                    DirectiveFamily::CachePolicy,
                    DirectiveSemantics::CacheSetting,
                    "Squid cache_mem value is invalid",
                );
                return;
            };
            CacheDirective::MemoryBytes {
                origin: origin(expanded),
                bytes,
            }
        } else if let [value] = expanded.directive.arguments.as_slice() {
            if let Some(enabled) = bytes::boolean(&value.value) {
                CacheDirective::Toggle {
                    origin: origin(expanded),
                    name: expanded.directive.name.value.clone(),
                    enabled,
                }
            } else {
                CacheDirective::Scalar {
                    origin: origin(expanded),
                    name: expanded.directive.name.value.clone(),
                    values: vec![value.into()],
                }
            }
        } else {
            CacheDirective::Scalar {
                origin: origin(expanded),
                name: expanded.directive.name.value.clone(),
                values: expanded
                    .directive
                    .arguments
                    .iter()
                    .map(Into::into)
                    .collect(),
            }
        };
        self.effective.cache_policy.push(directive);
        self.record(
            expanded,
            DirectiveFamily::CachePolicy,
            DirectiveSemantics::CacheSetting,
            Activation::Blocked(SemanticBlockerKind::CachePolicy),
        );
    }

    fn storage(&mut self, expanded: &ExpandedDirective) {
        let directive = if expanded.directive.name.value == b"cache_dir" {
            let [storage_type, path, size, level_one, level_two, options @ ..] =
                expanded.directive.arguments.as_slice()
            else {
                self.invalid(
                    expanded,
                    DirectiveFamily::Storage,
                    DirectiveSemantics::StorageSetting,
                    "Squid cache_dir requires type, path, size, and directory levels",
                );
                return;
            };
            let (Some(size_mib), Some(level_one), Some(level_two)) = (
                bytes::unsigned(&size.value),
                bytes::unsigned(&level_one.value),
                bytes::unsigned(&level_two.value),
            ) else {
                self.invalid(
                    expanded,
                    DirectiveFamily::Storage,
                    DirectiveSemantics::StorageSetting,
                    "Squid cache_dir numeric value is invalid",
                );
                return;
            };
            StorageDirective::CacheDir {
                origin: origin(expanded),
                storage_type: storage_type.into(),
                path: path.into(),
                size_mib,
                level_one,
                level_two,
                options: options.iter().map(Into::into).collect(),
            }
        } else {
            StorageDirective::Opaque {
                origin: origin(expanded),
                name: expanded.directive.name.value.clone(),
                argument_count: expanded.directive.arguments.len(),
            }
        };
        self.effective.storage.push(directive);
        self.record(
            expanded,
            DirectiveFamily::Storage,
            DirectiveSemantics::StorageSetting,
            Activation::Blocked(SemanticBlockerKind::StoragePolicy),
        );
    }

    fn process(&mut self, expanded: &ExpandedDirective) {
        let directive = if expanded.directive.name.value == b"coredump_dir" {
            let [path] = expanded.directive.arguments.as_slice() else {
                self.invalid(
                    expanded,
                    DirectiveFamily::Process,
                    DirectiveSemantics::CoreDumpDirectory,
                    "Squid coredump_dir requires one path",
                );
                return;
            };
            ProcessDirective::CoreDumpDirectory {
                origin: origin(expanded),
                path: path.into(),
            }
        } else {
            ProcessDirective::Opaque {
                origin: origin(expanded),
                name: expanded.directive.name.value.clone(),
                argument_count: expanded.directive.arguments.len(),
            }
        };
        let semantics = if expanded.directive.name.value == b"coredump_dir" {
            DirectiveSemantics::CoreDumpDirectory
        } else {
            DirectiveSemantics::ProcessSetting
        };
        self.effective.process.push(directive);
        self.record(
            expanded,
            DirectiveFamily::Process,
            semantics,
            Activation::Externalized,
        );
    }

    fn authentication_control(&mut self, expanded: &ExpandedDirective) {
        self.effective
            .authentication_controls
            .push(OpaqueDirective {
                origin: origin(expanded),
                name: expanded.directive.name.value.clone(),
                argument_count: expanded.directive.arguments.len(),
                secret: Some(SecretFact {
                    kind: SecretKind::AuthenticationHelper,
                    span: bytes::words_span(&expanded.directive.arguments)
                        .unwrap_or(expanded.directive.span),
                }),
            });
        self.record(
            expanded,
            DirectiveFamily::Authentication,
            DirectiveSemantics::AuthenticationSetting,
            Activation::Blocked(SemanticBlockerKind::ProxyAuthentication),
        );
    }

    fn logging_control(&mut self, expanded: &ExpandedDirective) {
        self.record(
            expanded,
            DirectiveFamily::Logging,
            DirectiveSemantics::LoggingSetting,
            Activation::Blocked(SemanticBlockerKind::LoggingPolicy),
        );
    }

    fn dns_control(&mut self, expanded: &ExpandedDirective) {
        self.effective.dns_controls.push(OpaqueDirective {
            origin: origin(expanded),
            name: expanded.directive.name.value.clone(),
            argument_count: expanded.directive.arguments.len(),
            secret: None,
        });
        self.record(
            expanded,
            DirectiveFamily::Dns,
            DirectiveSemantics::DnsSetting,
            Activation::Blocked(SemanticBlockerKind::ResolverPolicy),
        );
    }

    fn merge_acls(&mut self) {
        let mut indexes = HashMap::<Vec<u8>, usize>::new();
        let mut conflicts = Vec::new();
        for definition in self.effective.acl_definitions.clone() {
            if let Some(index) = indexes.get(&definition.name.value).copied() {
                let effective = &mut self.effective.acls[index];
                if effective.acl_type == definition.acl_type {
                    effective.definitions.push(definition.origin.occurrence);
                    effective.matchers.extend(definition.matchers.clone());
                } else {
                    conflicts.push(definition.origin.occurrence);
                    self.diagnostics.push(
                        Diagnostic::new(
                            E_DUPLICATE_IDENTITY,
                            Severity::Error,
                            DiagnosticStage::Resolve,
                            "same-name Squid ACL declarations use conflicting types",
                        )
                        .with_primary_span(definition.origin.directive_span)
                        .with_include_stack(
                            definition
                                .origin
                                .provenance
                                .include_stack
                                .iter()
                                .map(|frame| frame.directive_span),
                        ),
                    );
                }
            } else {
                indexes.insert(definition.name.value.clone(), self.effective.acls.len());
                self.effective.acls.push(EffectiveAcl {
                    name: definition.name.value.clone(),
                    acl_type: definition.acl_type,
                    definitions: vec![definition.origin.occurrence],
                    matchers: definition.matchers.clone(),
                });
            }
        }
        for occurrence in conflicts {
            self.block_occurrence(occurrence, SemanticBlockerKind::ConflictingAclType);
        }
    }

    fn resolve_access(&mut self) {
        let definitions = self
            .effective
            .acls
            .iter()
            .map(|acl| (acl.name.clone(), acl.definitions.clone()))
            .collect::<HashMap<_, _>>();
        let mut unresolved = Vec::new();
        for rule in &mut self.effective.access_rules {
            for term in &mut rule.terms {
                term.resolution = if let Some(builtin) = builtin_acl(&term.name.value) {
                    AclReferenceResolution::Builtin(builtin)
                } else if let Some(definitions) = definitions.get(&term.name.value) {
                    AclReferenceResolution::Defined(definitions.clone())
                } else {
                    unresolved.push(rule.origin.occurrence);
                    AclReferenceResolution::Unresolved
                };
            }
        }
        unresolved.sort_unstable();
        unresolved.dedup();
        for occurrence in unresolved {
            let expanded = &self.graph.expanded_directives[occurrence.get()];
            self.diagnostics.push(Self::diagnostic(
                expanded,
                E_UNRESOLVED_REFERENCE,
                "Squid access rule references an unresolved ACL",
            ));
            self.block_occurrence(occurrence, SemanticBlockerKind::UnresolvedAclReference);
        }

        let mut keys = Vec::new();
        for rule in &self.effective.access_rules {
            let key = (
                rule.kind,
                rule.selector
                    .as_ref()
                    .map(|selector| selector.value.clone()),
            );
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        for (kind, selector) in keys {
            let rules = self
                .effective
                .access_rules
                .iter()
                .filter(|rule| {
                    rule.kind == kind
                        && rule.selector.as_ref().map(|value| value.value.as_slice())
                            == selector.as_deref()
                })
                .cloned()
                .collect::<Vec<_>>();
            let default_action = rules
                .last()
                .map_or(AccessAction::Deny, |rule| rule.action.opposite());
            self.effective.access_policies.push(AccessPolicy {
                kind,
                selector,
                rules,
                default_action,
            });
        }
    }

    fn resolve_direct_access(&mut self) {
        let updates = self
            .effective
            .access_policies
            .iter()
            .filter(|policy| {
                matches!(
                    policy.kind,
                    AccessListKind::AlwaysDirect | AccessListKind::NeverDirect
                ) && policy.selector.is_none()
            })
            .flat_map(|policy| {
                let supported = policy.rules.len() == 1
                    && policy.rules[0].terms.len() == 1
                    && !policy.rules[0].terms[0].negated
                    && matches!(
                        policy.rules[0].terms[0].resolution,
                        AclReferenceResolution::Builtin(BuiltinAcl::All)
                    );
                policy
                    .rules
                    .iter()
                    .map(move |rule| (rule.origin.occurrence, supported))
            })
            .collect::<Vec<_>>();
        for (occurrence, supported) in updates {
            if supported {
                self.activate_occurrence(occurrence);
            } else {
                self.block_occurrence(occurrence, SemanticBlockerKind::DirectRoutingPolicy);
            }
        }
    }

    fn resolve_authentication(&mut self) {
        let mut indexes = HashMap::<Vec<u8>, usize>::new();
        for parameter in &self.effective.authentication {
            let index = *indexes
                .entry(parameter.scheme.value.clone())
                .or_insert_with(|| {
                    self.effective
                        .authentication_schemes
                        .push(AuthenticationScheme {
                            scheme: parameter.scheme.value.clone(),
                            parameters: Vec::new(),
                            program: None,
                            realm: None,
                            basic_program: None,
                            realm_value: None,
                            credential_ttl: None,
                            case_sensitive: None,
                            unsupported_settings: false,
                        });
                    self.effective.authentication_schemes.len() - 1
                });
            let scheme = &mut self.effective.authentication_schemes[index];
            scheme.parameters.push(parameter.origin.occurrence);
            match parameter.value {
                AuthenticationValue::Helper(secret) => {
                    scheme.program = Some(secret);
                    if parameter.setting == AuthenticationSetting::Program {
                        let arguments = self
                            .graph
                            .expanded_directives
                            .get(parameter.origin.occurrence.get())
                            .map(|expanded| {
                                expanded
                                    .directive
                                    .arguments
                                    .iter()
                                    .skip(2)
                                    .map(|word| word.value.clone())
                                    .collect()
                            })
                            .unwrap_or_default();
                        scheme.basic_program = Some(AuthenticationHelper { secret, arguments });
                    }
                }
                AuthenticationValue::Realm(secret) => {
                    scheme.realm = Some(secret);
                    let value = self
                        .graph
                        .expanded_directives
                        .get(parameter.origin.occurrence.get())
                        .map(|expanded| {
                            expanded
                                .directive
                                .arguments
                                .iter()
                                .skip(2)
                                .map(|word| word.value.as_slice())
                                .collect::<Vec<_>>()
                                .join(&b' ')
                        })
                        .unwrap_or_default();
                    scheme.realm_value = Some(AuthenticationRealm { secret, value });
                }
                AuthenticationValue::Duration(duration)
                    if parameter.setting == AuthenticationSetting::CredentialTtl =>
                {
                    scheme.credential_ttl = Some(duration);
                }
                AuthenticationValue::Boolean(value)
                    if parameter.setting == AuthenticationSetting::CaseSensitive =>
                {
                    scheme.case_sensitive = Some(value);
                }
                _ => scheme.unsupported_settings = true,
            }
        }
    }

    fn terminal_accounting(&mut self) {
        for expanded in &self.graph.expanded_directives {
            if self.decisions[expanded.occurrence.get()].is_none() {
                self.diagnostics.push(Self::diagnostic(
                    expanded,
                    E_UNCONSUMED_DIRECTIVE,
                    "Squid directive escaped terminal semantic accounting",
                ));
                self.record(
                    expanded,
                    DirectiveFamily::Unknown,
                    DirectiveSemantics::Unknown,
                    Activation::Blocked(SemanticBlockerKind::UnknownDirective),
                );
            }
        }
        self.effective.ledger = DecisionLedger {
            decisions: self
                .decisions
                .drain(..)
                .map(|decision| decision.expect("terminal accounting fills every occurrence"))
                .collect(),
        };
    }

    fn unknown(&mut self, expanded: &ExpandedDirective) {
        self.diagnostics.push(Self::diagnostic(
            expanded,
            E_UNKNOWN_DIRECTIVE,
            "Squid directive is not registered by the strict importer",
        ));
        self.record(
            expanded,
            DirectiveFamily::Unknown,
            DirectiveSemantics::Unknown,
            Activation::Blocked(SemanticBlockerKind::UnknownDirective),
        );
    }

    fn invalid(
        &mut self,
        expanded: &ExpandedDirective,
        family: DirectiveFamily,
        semantics: DirectiveSemantics,
        message: &'static str,
    ) {
        self.diagnostics
            .push(Self::diagnostic(expanded, E_UNSUPPORTED_FORM, message));
        self.record(
            expanded,
            family,
            semantics,
            Activation::Blocked(SemanticBlockerKind::InvalidForm),
        );
    }

    fn block_occurrence(&mut self, occurrence: OccurrenceId, blocker: SemanticBlockerKind) {
        let Some(decision) = self
            .decisions
            .get_mut(occurrence.get())
            .and_then(Option::as_mut)
        else {
            return;
        };
        let DecisionOutcome::Classified {
            family,
            semantics,
            resolution,
            ..
        } = decision.outcome;
        decision.outcome = DecisionOutcome::Classified {
            family,
            semantics,
            resolution,
            activation: Activation::Blocked(blocker),
        };
    }

    fn activate_occurrence(&mut self, occurrence: OccurrenceId) {
        let Some(decision) = self
            .decisions
            .get_mut(occurrence.get())
            .and_then(Option::as_mut)
        else {
            return;
        };
        let DecisionOutcome::Classified {
            family,
            semantics,
            resolution,
            ..
        } = decision.outcome;
        decision.outcome = DecisionOutcome::Classified {
            family,
            semantics,
            resolution,
            activation: Activation::Structural,
        };
    }

    fn record(
        &mut self,
        expanded: &ExpandedDirective,
        family: DirectiveFamily,
        semantics: DirectiveSemantics,
        activation: Activation,
    ) {
        self.decisions[expanded.occurrence.get()] = Some(Decision {
            origin: origin(expanded),
            name: expanded.directive.name.value.clone(),
            outcome: DecisionOutcome::Classified {
                family,
                semantics,
                resolution: resolution_for(semantics, activation),
                activation,
            },
        });
    }

    fn diagnostic(
        expanded: &ExpandedDirective,
        code: crate::DiagnosticCode,
        message: &'static str,
    ) -> Diagnostic {
        Diagnostic::new(code, Severity::Error, DiagnosticStage::Resolve, message)
            .with_primary_span(expanded.directive.span)
            .with_include_stack(
                expanded
                    .provenance
                    .include_stack
                    .iter()
                    .map(|frame| frame.directive_span),
            )
    }
}

const fn resolution_for(
    semantics: DirectiveSemantics,
    activation: Activation,
) -> DirectiveResolution {
    if matches!(activation, Activation::Externalized) {
        return DirectiveResolution::Externalized;
    }
    match semantics {
        DirectiveSemantics::Include => {
            if matches!(activation, Activation::Structural) {
                DirectiveResolution::Structural
            } else {
                DirectiveResolution::Blocked
            }
        }
        DirectiveSemantics::AclSource
        | DirectiveSemantics::AclPort
        | DirectiveSemantics::AclProxyAuth => DirectiveResolution::MergeSameName,
        DirectiveSemantics::HttpAccess
        | DirectiveSemantics::HeaderAccess
        | DirectiveSemantics::DirectAccess
        | DirectiveSemantics::CacheAccess
        | DirectiveSemantics::RefreshPattern => DirectiveResolution::OrderedFirstMatch,
        DirectiveSemantics::HttpPort
        | DirectiveSemantics::HttpsPort
        | DirectiveSemantics::IcpPort
        | DirectiveSemantics::HtcpPort
        | DirectiveSemantics::CachePeer
        | DirectiveSemantics::AccessLogging
        | DirectiveSemantics::DnsNameservers
        | DirectiveSemantics::StorageSetting => DirectiveResolution::Append,
        DirectiveSemantics::AuthenticationHelper
        | DirectiveSemantics::AuthenticationRealm
        | DirectiveSemantics::AuthenticationCredentialTtl
        | DirectiveSemantics::AuthenticationSetting
        | DirectiveSemantics::LoggingSetting
        | DirectiveSemantics::DnsSetting
        | DirectiveSemantics::ForwardedFor
        | DirectiveSemantics::Via
        | DirectiveSemantics::HeaderPrivacy
        | DirectiveSemantics::CacheSetting
        | DirectiveSemantics::CoreDumpDirectory
        | DirectiveSemantics::ProcessSetting => DirectiveResolution::LastWins,
        DirectiveSemantics::AclUnsupported | DirectiveSemantics::Unknown => {
            DirectiveResolution::Blocked
        }
    }
}

fn origin(expanded: &ExpandedDirective) -> DirectiveOrigin {
    DirectiveOrigin {
        occurrence: expanded.occurrence,
        directive_span: expanded.directive.span,
        name_span: expanded.directive.name.span,
        argument_spans: expanded
            .directive
            .arguments
            .iter()
            .map(|word| word.span)
            .collect(),
        provenance: expanded.provenance.clone(),
    }
}

fn builtin_acl(value: &[u8]) -> Option<BuiltinAcl> {
    match value {
        b"all" => Some(BuiltinAcl::All),
        b"CONNECT" => Some(BuiltinAcl::Connect),
        b"localhost" => Some(BuiltinAcl::Localhost),
        b"manager" => Some(BuiltinAcl::Manager),
        b"to_localhost" => Some(BuiltinAcl::ToLocalhost),
        b"to_linklocal" => Some(BuiltinAcl::ToLinkLocal),
        _ => None,
    }
}

fn proxy_auth_matcher(value: &Word) -> AclMatcher {
    if value.value == b"REQUIRED" {
        AclMatcher::ProxyAuth(ProxyAuthMatcher::Required)
    } else {
        AclMatcher::ProxyAuth(ProxyAuthMatcher::Identity(SecretFact {
            kind: SecretKind::ProxyIdentity,
            span: value.span,
        }))
    }
}

fn access_kind(name: &[u8]) -> Option<AccessListKind> {
    match name {
        b"http_access" => Some(AccessListKind::Http),
        b"follow_x_forwarded_for" => Some(AccessListKind::FollowForwardedFor),
        b"always_direct" => Some(AccessListKind::AlwaysDirect),
        b"never_direct" => Some(AccessListKind::NeverDirect),
        b"cache" | b"no_cache" => Some(AccessListKind::Cache),
        b"cache_peer_access" => Some(AccessListKind::CachePeer),
        name if name.ends_with(b"_access") => Some(AccessListKind::Other),
        _ => None,
    }
}

const fn access_semantics(kind: AccessListKind) -> DirectiveSemantics {
    match kind {
        AccessListKind::Http => DirectiveSemantics::HttpAccess,
        AccessListKind::RequestHeader
        | AccessListKind::ReplyHeader
        | AccessListKind::FollowForwardedFor
        | AccessListKind::Other => DirectiveSemantics::HeaderAccess,
        AccessListKind::AlwaysDirect | AccessListKind::NeverDirect => {
            DirectiveSemantics::DirectAccess
        }
        AccessListKind::Cache | AccessListKind::CachePeer => DirectiveSemantics::CacheAccess,
    }
}

const fn access_blocker(kind: AccessListKind) -> SemanticBlockerKind {
    match kind {
        AccessListKind::Http => SemanticBlockerKind::OrderedHttpAccess,
        AccessListKind::RequestHeader
        | AccessListKind::ReplyHeader
        | AccessListKind::FollowForwardedFor
        | AccessListKind::Other => SemanticBlockerKind::HeaderAccessPolicy,
        AccessListKind::AlwaysDirect | AccessListKind::NeverDirect => {
            SemanticBlockerKind::DirectRoutingPolicy
        }
        AccessListKind::Cache | AccessListKind::CachePeer => SemanticBlockerKind::CacheAccessPolicy,
    }
}

const fn port_semantics(kind: PortKind) -> DirectiveSemantics {
    match kind {
        PortKind::Http => DirectiveSemantics::HttpPort,
        PortKind::Https => DirectiveSemantics::HttpsPort,
        PortKind::Icp => DirectiveSemantics::IcpPort,
        PortKind::Htcp => DirectiveSemantics::HtcpPort,
    }
}

fn parse_port_option(option: &Word) -> PortOption {
    match option.value.as_slice() {
        b"intercept" => PortOption::Intercept,
        b"tproxy" => PortOption::Tproxy,
        b"accel" => PortOption::Accel,
        b"ssl-bump" => PortOption::SslBump,
        value if bytes::assignment(value, b"name").is_some() => PortOption::Name(option.into()),
        value if bytes::assignment(value, b"defaultsite").is_some() => {
            PortOption::DefaultSite(option.into())
        }
        _ => PortOption::Unsupported(option.into()),
    }
}

fn parse_peer_option(option: &Word) -> PeerOption {
    if let Some(kind) = peer_secret_kind(&option.value) {
        return PeerOption::Secret(SecretFact {
            kind,
            span: option.span,
        });
    }
    match option.value.as_slice() {
        b"no-query" => PeerOption::NoQuery,
        b"proxy-only" => PeerOption::ProxyOnly,
        b"originserver" => PeerOption::OriginServer,
        b"round-robin" => PeerOption::RoundRobin,
        value if bytes::assignment(value, b"weight").is_some() => {
            bytes::assignment(value, b"weight")
                .and_then(bytes::unsigned)
                .map_or_else(
                    || PeerOption::Unsupported(option.into()),
                    PeerOption::Weight,
                )
        }
        value if bytes::assignment(value, b"name").is_some() => PeerOption::Name(option.into()),
        _ => PeerOption::Unsupported(option.into()),
    }
}

fn is_static_parent_peer(peer: &CachePeer) -> bool {
    peer.peer_type == CachePeerType::Parent
        && peer.http_port != 0
        && peer.icp_port == 0
        && peer.options.is_empty()
        && (std::str::from_utf8(&peer.host.value)
            .ok()
            .and_then(|value| value.parse::<std::net::IpAddr>().ok())
            .is_some()
            || dns_name(&peer.host.value).is_some())
}

fn peer_secret_kind(value: &[u8]) -> Option<SecretKind> {
    if [b"login".as_slice(), b"password", b"passwd"]
        .iter()
        .any(|key| bytes::assignment(value, key).is_some())
    {
        Some(SecretKind::PeerCredentials)
    } else if [b"token".as_slice(), b"bearer"]
        .iter()
        .any(|key| bytes::assignment(value, key).is_some())
    {
        Some(SecretKind::BearerToken)
    } else if [b"key".as_slice(), b"private_key"]
        .iter()
        .any(|key| bytes::assignment(value, key).is_some())
    {
        Some(SecretKind::PrivateKey)
    } else if [b"password_hash".as_slice(), b"passwordhash"]
        .iter()
        .any(|key| bytes::assignment(value, key).is_some())
    {
        Some(SecretKind::PasswordHash)
    } else {
        None
    }
}

fn parse_refresh_option(option: &Word) -> Option<RefreshOption> {
    match option.value.as_slice() {
        b"override-expire" => Some(RefreshOption::OverrideExpire),
        b"override-lastmod" => Some(RefreshOption::OverrideLastModified),
        b"reload-into-ims" => Some(RefreshOption::ReloadIntoIms),
        b"ignore-reload" => Some(RefreshOption::IgnoreReload),
        b"ignore-no-store" => Some(RefreshOption::IgnoreNoStore),
        b"ignore-private" => Some(RefreshOption::IgnorePrivate),
        b"refresh-ims" => Some(RefreshOption::RefreshIms),
        b"store-stale" => Some(RefreshOption::StoreStale),
        value => bytes::assignment(value, b"max-stale")
            .and_then(bytes::unsigned)
            .map(RefreshOption::MaxStale),
    }
}

fn parse_log_destination(word: &Word) -> LogDestination {
    match word.value.as_slice() {
        b"none" => LogDestination::Disabled,
        value if value.starts_with(b"stdio:") => LogDestination::Stdio(word.into()),
        value if value.starts_with(b"daemon:") => LogDestination::Daemon(word.into()),
        b"syslog" => LogDestination::Syslog(None),
        value if value.starts_with(b"syslog:") => LogDestination::Syslog(Some(word.into())),
        _ => LogDestination::File(word.into()),
    }
}

fn is_cache_policy(name: &[u8]) -> bool {
    matches!(
        name,
        b"cache_mem"
            | b"maximum_object_size"
            | b"minimum_object_size"
            | b"maximum_object_size_in_memory"
            | b"cache_replacement_policy"
            | b"memory_replacement_policy"
            | b"offline_mode"
            | b"collapsed_forwarding"
            | b"cache_swap_low"
            | b"cache_swap_high"
            | b"store_avg_object_size"
            | b"quick_abort_min"
            | b"quick_abort_max"
            | b"quick_abort_pct"
            | b"read_ahead_gap"
            | b"range_offset_limit"
    )
}

fn is_storage(name: &[u8]) -> bool {
    matches!(
        name,
        b"cache_dir"
            | b"store_dir_select_algorithm"
            | b"cache_swap_state"
            | b"unlinkd_program"
            | b"store_id_program"
            | b"store_id_children"
    )
}

fn is_authentication(name: &[u8]) -> bool {
    matches!(
        name,
        b"authenticate_cache_garbage_interval"
            | b"authenticate_ttl"
            | b"authenticate_ip_ttl"
            | b"external_acl_type"
            | b"url_rewrite_program"
            | b"sslpassword_program"
    )
}

fn is_logging(name: &[u8]) -> bool {
    matches!(
        name,
        b"cache_log"
            | b"cache_store_log"
            | b"logfile_daemon"
            | b"logformat"
            | b"debug_options"
            | b"log_icp_queries"
            | b"buffered_logs"
            | b"strip_query_terms"
    )
}

fn is_dns(name: &[u8]) -> bool {
    matches!(
        name,
        b"dns_v4_first"
            | b"dns_timeout"
            | b"positive_dns_ttl"
            | b"negative_dns_ttl"
            | b"ipcache_size"
            | b"fqdncache_size"
            | b"hosts_file"
    )
}

fn is_process(name: &[u8]) -> bool {
    matches!(
        name,
        b"coredump_dir"
            | b"pid_filename"
            | b"workers"
            | b"max_filedescriptors"
            | b"cache_effective_user"
            | b"cache_effective_group"
            | b"visible_hostname"
            | b"unique_hostname"
    )
}
