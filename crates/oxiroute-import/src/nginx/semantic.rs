use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, SocketAddr},
    path::PathBuf,
};

use oxiroute_config::canonical_ip;

use crate::canonical::{dns_name, ip_address, unix_socket_path};
use crate::{
    Diagnostic, DiagnosticCode, DiagnosticStage, E_DUPLICATE_IDENTITY, E_INCLUDE_CYCLE,
    E_INVALID_VALUE, E_SOURCE_CHANGED, E_SOURCE_IO, E_SOURCE_LIMIT, E_UNRESOLVED_REFERENCE,
    E_UNSUPPORTED_FEATURE, Report, Severity, Span,
};

use super::{
    Directive, ExpandedDirective, ExpandedOccurrence, IncludeCandidateStatus, OccurrenceId,
    Provenance, SourceGraph, Word,
};

mod binds;
mod driver;
mod helpers;
mod locations;
mod model;
mod servers;

pub(super) use helpers::certbot_host_condition;
use helpers::{
    ascii_lowercase, has_variable, include_failure, is_http_policy, is_location_policy,
    is_scalar_policy, is_server_policy, is_supported_location, is_supported_server_name,
    listen_endpoints_overlap, listen_protocols, parse_listen_endpoint, parse_proxy_pass,
    parse_static_endpoint, policy_allows_variables, policy_argument_count_valid,
    server_name_claims, server_name_kind,
};
pub use model::*;

/// Resolves an expanded nginx fragment whose root contains only an `http` block.
#[must_use]
pub fn resolve_http_fragment(loaded: Report<SourceGraph>) -> Report<HttpResolution> {
    let (graph, mut diagnostics) = loaded.into_parts();
    let (resolution, resolve_diagnostics) = resolve_http_graph(&graph).into_parts();
    diagnostics.extend(resolve_diagnostics);
    Report::new(resolution, diagnostics)
}

#[must_use]
pub(super) fn resolve_http_graph(graph: &SourceGraph) -> Report<HttpResolution> {
    Resolver::new(graph, false).run()
}

pub(super) fn resolve_http_root_graph(graph: &SourceGraph) -> Report<HttpResolution> {
    Resolver::new(graph, true).run()
}

struct Resolver<'a> {
    graph: &'a SourceGraph,
    dispositions: Vec<Option<OccurrenceDisposition>>,
    diagnostics: Vec<Diagnostic>,
    complete_root: bool,
}
