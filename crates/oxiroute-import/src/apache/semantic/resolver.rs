#[allow(clippy::wildcard_imports)]
use super::*;

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

mod balancer;
mod checks;
mod global;
mod helpers;
mod virtual_host;

use helpers::{
    absolute_path, canonical_host, has_dynamic, lower_name, origin, parse_explicit_socket,
    parse_member_target, parse_proxy_pass, parse_server_name, parse_server_name_text,
    parse_virtual_host_sockets, selector_key, socket_addresses_overlap, split_host_port,
    supported_module, word_text,
};
