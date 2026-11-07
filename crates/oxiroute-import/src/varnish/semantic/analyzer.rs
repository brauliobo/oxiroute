#[allow(clippy::wildcard_imports)]
use super::*;

#[must_use]
pub fn analyze(root: &SourceFile, included_sources: &[SourceFile]) -> ImportReport {
    analyze_graph(
        load_memory(root, included_sources),
        InvocationFacts::default(),
    )
}

#[cfg(unix)]
#[must_use]
pub fn import(root: &Path, invocation: &VarnishdInvocation) -> ImportReport {
    analyze_graph(super::super::loader::load(root), invocation.facts())
}

#[must_use]
pub fn analyze_graph(loaded: Report<SourceGraph>, invocation: InvocationFacts) -> ImportReport {
    let (graph, diagnostics) = loaded.into_parts();
    Builder::new(graph, invocation, diagnostics).build()
}

struct Builder {
    graph: SourceGraph,
    invocation: InvocationFacts,
    declarations: Vec<DeclarationDecision>,
    imports: Vec<VmodImport>,
    vmod_objects: Vec<VmodObject>,
    acls: Vec<Acl>,
    probes: Vec<Probe>,
    backends: Vec<Backend>,
    directors: Vec<Director>,
    modern_directors: Vec<ModernDirector>,
    subroutines: Vec<Subroutine>,
    statements: Vec<StatementDecision>,
    import_aliases: BTreeMap<Vec<u8>, Vec<u8>>,
    vmod_object_names: BTreeMap<Vec<u8>, Vec<usize>>,
    acl_names: BTreeMap<Vec<u8>, Vec<usize>>,
    probe_names: BTreeMap<Vec<u8>, Vec<usize>>,
    backend_names: BTreeMap<Vec<u8>, Vec<usize>>,
    director_names: BTreeMap<Vec<u8>, Vec<usize>>,
    modern_director_names: BTreeMap<Vec<u8>, Vec<usize>>,
    subroutine_names: BTreeMap<Vec<u8>, Vec<usize>>,
    call_edges: Vec<CallEdge>,
    call_truncated: bool,
    diagnostics: Vec<Diagnostic>,
}

mod conditions;
mod driver;
mod helpers;
mod signatures;
mod statements;
mod symbols;

use helpers::{
    backend_kind, cache_lifetime_field, classify_backend_property, classify_probe_property,
    collect_duplicate_spans, condition_is_non_static, condition_operator, detect_cycles,
    director_kind, expression_call, expression_name, first_call, header_condition,
    invalid_expression, is_cookie, literal_status, object_method_call, parse_header, split_method,
    unique_index,
};
pub use signatures::*;
