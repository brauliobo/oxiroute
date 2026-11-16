use std::collections::HashMap;

use oxiroute_config::{ConfigDraft, HttpRoute, UpstreamPool};

use crate::{
    ActivationRequirement, CanonicalCandidate as SharedCanonicalCandidate, DeploymentRequirement,
    DeploymentRequirementKind, Diagnostic, DiagnosticStage, E_INVALID_VALUE,
    OperationalOverlayRequirement, ProvenanceRole, Report, Severity, SourceImportMetadata,
    candidate::{
        CanonicalCandidateState, CanonicalProvenanceLedger, EmptyOriginPolicy, empty_config,
        finalize_candidate,
    },
};

use super::{ApacheResolution, DirectiveOrigin, OccurrenceDecision, SourceGraph};

mod listener;
mod pool;
mod tls;
mod virtual_host;

use listener::socket_addresses_overlap;

pub use super::semantic::ApacheProvenance;

pub type CanonicalCandidate = SharedCanonicalCandidate<ApacheProvenance>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockedVirtualHost {
    pub address: std::net::SocketAddr,
    pub origin: ApacheProvenance,
    pub diagnostic_codes: Vec<crate::DiagnosticCode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApacheImportReport {
    pub source_graph: SourceGraph,
    pub occurrence_ledger: Vec<OccurrenceDecision>,
    pub diagnostics: Vec<Diagnostic>,
    pub blocked_virtual_hosts: Vec<BlockedVirtualHost>,
    pub candidate: CanonicalCandidate,
}

impl ApacheImportReport {
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity() == Severity::Error)
    }
}

pub(super) fn lower(loaded: Report<SourceGraph>) -> ApacheImportReport {
    let (graph, load_diagnostics) = loaded.into_parts();
    let resolved = super::semantic::resolve(Report::new(graph.clone(), load_diagnostics));
    let (resolution, diagnostics) = resolved.into_parts();
    Lowerer::new(graph, resolution, diagnostics).run()
}

struct Lowerer {
    graph: SourceGraph,
    resolution: ApacheResolution,
    diagnostics: Vec<Diagnostic>,
    draft: ConfigDraft,
    provenance: CanonicalProvenanceLedger<ApacheProvenance>,
    deployment_requirements: Vec<DeploymentRequirement<ApacheProvenance>>,
    activation_requirements: Vec<ActivationRequirement<ApacheProvenance>>,
    operational_overlays: Vec<OperationalOverlayRequirement<ApacheProvenance>>,
    pool_names: HashMap<String, String>,
    pool_definitions: HashMap<String, LoweredPool>,
    next_pool_number: usize,
    blocked_virtual_hosts: Vec<BlockedVirtualHost>,
}

struct LoweredPool {
    pool: UpstreamPool,
    origins: Vec<ApacheProvenance>,
}

struct LoweredVirtualHost {
    routes: Vec<HttpRoute>,
    route_origins: Vec<Vec<ApacheProvenance>>,
    origins: Vec<ApacheProvenance>,
}

impl Lowerer {
    fn new(graph: SourceGraph, resolution: ApacheResolution, diagnostics: Vec<Diagnostic>) -> Self {
        Self {
            graph,
            resolution,
            diagnostics,
            draft: empty_config(),
            provenance: CanonicalProvenanceLedger::new(EmptyOriginPolicy::Preserve),
            deployment_requirements: Vec::new(),
            activation_requirements: Vec::new(),
            operational_overlays: Vec::new(),
            pool_names: HashMap::new(),
            pool_definitions: HashMap::new(),
            next_pool_number: 1,
            blocked_virtual_hosts: Vec::new(),
        }
    }

    fn run(mut self) -> ApacheImportReport {
        for module in &self.resolution.module_loads {
            let origin = self.origin(&module.origin, ProvenanceRole::Declaration);
            self.deployment_requirements.push(DeploymentRequirement {
                kind: DeploymentRequirementKind::ModuleLoad,
                directive: "LoadModule".into(),
                value: vec![module.module.clone(), module.path.clone()],
                origin,
            });
        }

        let listens = self.resolution.listens.clone();
        for listen in listens {
            let virtual_hosts = self
                .resolution
                .virtual_hosts
                .iter()
                .filter(|virtual_host| {
                    virtual_host
                        .addresses
                        .iter()
                        .any(|address| socket_addresses_overlap(*address, listen.address))
                })
                .cloned()
                .collect::<Vec<_>>();
            if virtual_hosts.is_empty() {
                continue;
            }
            self.lower_listener(&listen, &virtual_hosts);
        }

        let mut pools = self.pool_definitions.drain().collect::<Vec<_>>();
        pools.sort_by(|(left, _), (right, _)| left.cmp(right));
        for (key, pool) in pools {
            let index = self.draft.upstream_pools.len();
            self.draft.upstream_pools.push(pool.pool);
            let path = format!("/upstream_pools/{index}");
            self.record(path.clone(), pool.origins.clone());
            self.record(format!("{path}/name"), pool.origins.clone());
            self.record(format!("{path}/algorithm"), pool.origins.clone());
            self.record(format!("{path}/servers"), pool.origins.clone());
            let server_count = self.draft.upstream_pools[index].servers.len();
            for server_index in 0..server_count {
                let server_path = format!("{path}/servers/{server_index}");
                self.record(server_path.clone(), pool.origins.clone());
                self.record(format!("{server_path}/name"), pool.origins.clone());
                self.record(format!("{server_path}/endpoint"), pool.origins.clone());
                self.record(format!("{server_path}/endpoint/type"), pool.origins.clone());
            }
            if self.draft.upstream_pools[index].tls.is_some() {
                self.record(format!("{path}/tls"), pool.origins.clone());
            }
            let _ = key;
        }

        let draft = std::mem::replace(&mut self.draft, empty_config());
        let state = self.finalize(draft);
        let source_metadata = SourceImportMetadata {
            original_sources: self
                .graph
                .sources
                .iter()
                .map(|source| source.source.clone())
                .collect(),
            ..SourceImportMetadata::default()
        };
        ApacheImportReport {
            source_graph: self.graph,
            occurrence_ledger: self.resolution.decisions,
            diagnostics: Report::new((), self.diagnostics).into_parts().1,
            blocked_virtual_hosts: self.blocked_virtual_hosts,
            candidate: CanonicalCandidate::new(
                state,
                self.provenance.into_entries(),
                self.deployment_requirements,
                self.activation_requirements,
                self.operational_overlays,
                source_metadata,
            ),
        }
    }

    fn finalize(&mut self, draft: ConfigDraft) -> CanonicalCandidateState {
        let eligible = !self
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity() == Severity::Error);
        match finalize_candidate(&draft, eligible) {
            Ok(Some(config)) => CanonicalCandidateState::Validated(config),
            Ok(None) => CanonicalCandidateState::Blocked(draft),
            Err(error) => {
                let mut diagnostic = Diagnostic::new(
                    E_INVALID_VALUE,
                    Severity::Error,
                    DiagnosticStage::Validate,
                    format!("lowered Apache canonical draft is invalid: {error}"),
                );
                if let Some(origin) = self
                    .provenance
                    .first()
                    .and_then(|entry| entry.origins.first())
                {
                    diagnostic = diagnostic
                        .with_primary_span(origin.span)
                        .with_include_stack(
                            origin
                                .include_stack
                                .iter()
                                .map(|frame| frame.directive_span),
                        );
                }
                self.diagnostics.push(diagnostic);
                CanonicalCandidateState::Blocked(draft)
            }
        }
    }

    fn origin(&self, origin: &DirectiveOrigin, role: ProvenanceRole) -> ApacheProvenance {
        apache_origin(&self.graph, origin, role)
    }

    fn record(&mut self, path: String, origins: Vec<ApacheProvenance>) {
        self.provenance.record(path, origins, |origin| {
            (origin.source, origin.span, origin.role as u8)
        });
    }

    fn block_origin(
        &mut self,
        origin: &DirectiveOrigin,
        code: crate::DiagnosticCode,
        message: impl Into<String>,
    ) {
        let source = self.origin(origin, ProvenanceRole::Value);
        self.diagnostics.push(
            Diagnostic::new(code, Severity::Error, DiagnosticStage::Lower, message)
                .with_primary_span(source.span)
                .with_include_stack(
                    source
                        .include_stack
                        .iter()
                        .map(|frame| frame.directive_span),
                ),
        );
    }
}

#[allow(clippy::naive_bytecount)]
fn apache_origin(
    graph: &SourceGraph,
    origin: &DirectiveOrigin,
    role: ProvenanceRole,
) -> ApacheProvenance {
    let source = graph
        .source(origin.provenance.source)
        .expect("Apache provenance source retained in graph");
    let offset = origin.span.range().start().min(source.source.len());
    ApacheProvenance {
        role,
        source: origin.provenance.source,
        path: source.canonical_path.clone(),
        line: source.source.bytes()[..offset]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count()
            + 1,
        span: origin.span,
        include_stack: origin.provenance.include_stack.clone(),
    }
}
