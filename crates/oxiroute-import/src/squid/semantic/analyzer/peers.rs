#[allow(clippy::wildcard_imports)]
use super::*;

impl Analyzer<'_> {
    pub(super) fn port(&mut self, expanded: &ExpandedDirective, kind: PortKind) {
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

    pub(super) fn cache_peer(&mut self, expanded: &ExpandedDirective) {
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

    pub(super) fn refresh(&mut self, expanded: &ExpandedDirective) {
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
}
