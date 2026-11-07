use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, SocketAddr},
};

use crate::canonical::{dns_name, ip_address, unix_socket_path};
use crate::{
    Diagnostic, DiagnosticCode, DiagnosticStage, E_DUPLICATE_IDENTITY, E_INCLUDE_CYCLE,
    E_INVALID_VALUE, E_SOURCE_CHANGED, E_SOURCE_IO, E_SOURCE_LIMIT, E_UNRESOLVED_REFERENCE,
    E_UNSUPPORTED_FEATURE, Report, Severity,
};

use super::{DirectiveOrigin, ListenEndpoint, NginxValue, StaticEndpoint};
use super::{
    ExpandedDirective, ExpandedOccurrence, IncludeCandidateStatus, IncludeEdge, OccurrenceDecision,
    OccurrenceDisposition, OccurrenceId, SourceGraph, Word,
};

mod directives;
mod listeners;
mod model;
mod parse;
mod support;

pub use model::*;
use model::{NGINX_DEFAULT_PROXY_CONNECT_TIMEOUT_MS, NGINX_DEFAULT_PROXY_TIMEOUT_MS};
use parse::{
    ascii_lowercase, has_variable, include_failure, is_direct_endpoint_value,
    listen_endpoints_overlap, parse_duration_ms, parse_static_endpoint,
    parse_stream_listen_endpoint,
};

#[must_use]
pub fn resolve_stream_fragment(loaded: Report<SourceGraph>) -> Report<StreamResolution> {
    let (graph, mut diagnostics) = loaded.into_parts();
    let (resolution, resolve_diagnostics) = resolve_stream_graph(&graph).into_parts();
    diagnostics.extend(resolve_diagnostics);
    Report::new(resolution, diagnostics)
}

pub(super) fn resolve_stream_graph(graph: &SourceGraph) -> Report<StreamResolution> {
    Resolver::new(graph, false).run()
}

pub(super) fn resolve_stream_root_graph(graph: &SourceGraph) -> Report<StreamResolution> {
    Resolver::new(graph, true).run()
}

struct Resolver<'a> {
    graph: &'a SourceGraph,
    dispositions: Vec<Option<OccurrenceDisposition>>,
    diagnostics: Vec<Diagnostic>,
    complete_root: bool,
}
