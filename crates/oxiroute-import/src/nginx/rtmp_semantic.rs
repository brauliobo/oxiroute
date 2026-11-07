use std::{collections::HashMap, net::SocketAddr, path::PathBuf};

use crate::{
    Diagnostic, DiagnosticCode, DiagnosticStage, E_DUPLICATE_IDENTITY, E_INCLUDE_CYCLE,
    E_INVALID_VALUE, E_SEMANTICS_NOT_REPRESENTABLE, E_SOURCE_CHANGED, E_SOURCE_IO, E_SOURCE_LIMIT,
    E_UNSUPPORTED_FEATURE, Report, Severity,
};
use oxiroute_config::{
    RtmpAclAction, RtmpExecMode, RtmpExecTrigger, RtmpHlsFragmentNaming, RtmpHlsKeyPolicy,
    RtmpHlsPolicy, RtmpRecordMask,
};

use super::{
    DirectiveContext, DirectiveError, DirectiveOrigin, ExpandedDirective, ExpandedOccurrence,
    IncludeCandidateStatus, NginxValue, OccurrenceDecision, OccurrenceDisposition, OccurrenceId,
    SourceGraph, Word, directive_specs, validate_directive,
};

mod applications;
mod driver;
mod helpers;
mod model;
mod policy;
mod servers;

use helpers::{
    exact_suffix, include_failure, is_root_auto_push_policy, is_supported_policy,
    parse_access_rule, parse_nginx_milliseconds, parse_nginx_size, parse_push_target, parse_record,
    parse_rtmp_socket, parse_u32, parse_u64, secure_auto_push_root, secure_recording_root,
    sockets_overlap, unsupported_rtmp_reason, valid_canonical_name, valid_exec_path,
};
pub use model::*;

#[must_use]
pub fn resolve_rtmp(loaded: Report<SourceGraph>) -> Report<RtmpResolution> {
    let (graph, mut diagnostics) = loaded.into_parts();
    let (resolution, resolve_diagnostics) = resolve_rtmp_graph(&graph).into_parts();
    diagnostics.extend(resolve_diagnostics);
    Report::new(resolution, diagnostics)
}

pub(super) fn resolve_rtmp_graph(graph: &SourceGraph) -> Report<RtmpResolution> {
    Resolver::new(graph, false).run()
}

pub(super) fn resolve_rtmp_root_graph(graph: &SourceGraph) -> Report<RtmpResolution> {
    Resolver::new(graph, true).run()
}

struct Resolver<'a> {
    graph: &'a SourceGraph,
    dispositions: Vec<Option<OccurrenceDisposition>>,
    diagnostics: Vec<Diagnostic>,
    complete_root: bool,
}
