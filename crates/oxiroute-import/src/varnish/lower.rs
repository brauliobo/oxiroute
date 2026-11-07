use std::{
    collections::BTreeMap,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
};

use http::{HeaderName, HeaderValue};
use oxiroute_config::{
    CacheKeyComponent, CacheStore, ConfigDraft, DnsResolutionPolicy, DownstreamTimeoutPolicy,
    HealthCheck, HealthCheckType, HealthStartup, HttpCachePolicy, HttpProxyPolicy,
    HttpRequestHeaderMutation, HttpRequestHeaderValue, HttpResponseHeaderMutation, HttpRoute,
    HttpRouteAction, HttpRoutePolicy, HttpService, HttpUpstreamHost, Listener, ListenerBind,
    Protocol, UpstreamAlgorithm, UpstreamConnectionReuse, UpstreamEndpoint, UpstreamPool,
    UpstreamServer, canonical_dns_name,
};

use crate::{
    CanonicalCandidate, Diagnostic, DiagnosticStage, E_INVALID_VALUE, E_SOURCE_CHANGED,
    E_SOURCE_LIMIT, E_UNSUPPORTED_FEATURE, Severity, SourceImportMetadata,
    candidate::{
        CanonicalCandidateState, CanonicalProvenanceLedger, EmptyOriginPolicy, empty_config,
        finalize_candidate,
    },
};

use super::{
    Backend, BackendKind, BackendProperty, BackendReference, CacheFlag, CacheLifetimeField,
    CompressionOperation, Director, DirectorKind, E_VCL_LOWERING_BLOCKED, E_VCL_SEMANTIC_MISMATCH,
    E_VCL_UNSUPPORTED_SUBROUTINE, Expression, ExpressionKind, FeatureBehavior, FlowAction,
    HeaderMutation, HeaderOperation, HeaderScope, InvocationFacts, LoweringBlocker, LoweringStatus,
    ModernDirector, Probe, ProbeProperty, ProbeReference, Provenance, SourceGraph,
    StatementClassification, StatementDecision, Subroutine, SubroutineKind, VmodImport, VmodObject,
};

mod backends;
mod driver;
mod graph;
mod helpers;
mod invocation;
mod model;

use helpers::{
    boolean, canonical_storage_path, canonical_unix_path, display, duration_ms, expression_name,
    integer, is_host_name, parse_duration_ms_bytes, parse_listen, parse_size, probe_provenance,
    request_mutation, response_mutation, same_timing, static_port, static_string, static_text,
};
pub use model::*;

/// Lowers a complete semantic Varnish graph, retaining the semantic diagnostics accumulated by
/// parsing and resolution. No runtime code is invoked during this pass.
#[allow(clippy::too_many_arguments)]
pub(super) fn lower(
    graph: &SourceGraph,
    invocation: &InvocationFacts,
    backends: &[Backend],
    directors: &[Director],
    probes: &[Probe],
    modern_directors: &[ModernDirector],
    subroutines: &[Subroutine],
    statements: &[StatementDecision],
    imports: &[VmodImport],
    vmod_objects: &[VmodObject],
    diagnostics: Vec<Diagnostic>,
) -> (VarnishCanonicalCandidate, LoweringStatus, Vec<Diagnostic>) {
    Lowerer::new(
        graph,
        invocation,
        backends,
        directors,
        probes,
        modern_directors,
        subroutines,
        statements,
        imports,
        vmod_objects,
        diagnostics,
    )
    .run()
}
