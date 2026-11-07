#[allow(clippy::wildcard_imports)]
use super::*;

/// Resolves parsed `HAProxy` sources in their existing occurrence order.
#[must_use]
pub(in super::super) fn resolve(configuration: &Configuration) -> Report<EffectiveConfiguration> {
    Resolver::new(configuration).run()
}
/// Resolves a parsed report without discarding source, lexing, or parsing diagnostics.
#[must_use]
pub(in super::super) fn resolve_report(
    parsed: Report<Configuration>,
) -> Report<EffectiveConfiguration> {
    let (configuration, mut diagnostics) = parsed.into_parts();
    let (effective, resolve_diagnostics) = resolve(&configuration).into_parts();
    for diagnostic in resolve_diagnostics {
        if !diagnostics.contains(&diagnostic) {
            diagnostics.push(diagnostic);
        }
    }
    Report::new(effective, diagnostics)
}

#[derive(Clone)]
pub(in super::super) struct ParsedHeader {
    pub(in super::super) name: Option<(Vec<u8>, Span)>,
    pub(in super::super) from: Option<(Vec<u8>, Span)>,
}

#[derive(Clone)]
pub(in super::super) struct SectionMeta {
    pub(in super::super) id: SectionId,
    pub(in super::super) section: Section,
    header: Option<ParsedHeader>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum DefaultsResolutionState {
    Unvisited,
    Visiting,
    Resolved,
}

struct PendingDecision {
    occurrence: OccurrenceId,
    section: Option<SectionId>,
    keyword: Vec<u8>,
    span: Span,
    outcome: Option<DecisionOutcome>,
}

#[derive(Default)]
struct SectionState {
    settings: ProxySettings,
    binds: Vec<EffectiveBind>,
    servers: Vec<EffectiveServer>,
    server_defaults: Option<EffectiveServer>,
    acls: Vec<EffectiveValue<AclDefinition>>,
    pending_http_request_rules: Vec<PendingHttpRequestRule>,
    pending_use_backends: Vec<PendingUseBackend>,
    use_backends: Vec<EffectiveValue<UseBackend>>,
    stats: StatsSettings,
}

struct PendingHttpRequestRule {
    occurrence: OccurrenceId,
    span: Span,
    rule: HttpRequestRule,
    condition: Option<PendingAclCondition>,
}

pub(in super::super) struct PendingAclCondition {
    pub(in super::super) name: Vec<u8>,
    pub(in super::super) span: Span,
    pub(in super::super) polarity: ConditionPolarity,
    pub(in super::super) negated: bool,
}

struct PendingUseBackend {
    occurrence: OccurrenceId,
    span: Span,
    backend_name: Vec<u8>,
    backend_span: Span,
    acl_conditions: Vec<PendingAclCondition>,
    polarity: ConditionPolarity,
    condition_negated: bool,
}

struct Resolver {
    preamble: Vec<(OccurrenceId, Directive)>,
    sections: Vec<SectionMeta>,
    decisions: Vec<PendingDecision>,
    decision_indices: HashMap<OccurrenceId, usize>,
    diagnostics: Vec<Diagnostic>,
    defaults_by_name: HashMap<Vec<u8>, Vec<usize>>,
    backends_by_name: HashMap<Vec<u8>, Vec<usize>>,
    defaults_state: Vec<DefaultsResolutionState>,
    resolved_defaults: Vec<Option<EffectiveDefaults>>,
    effective: EffectiveConfiguration,
}

mod decisions;
mod driver;
mod rules;
mod sections;
mod settings;

use decisions::{push_pending_decision, semantic_blocker, set_idempotent_value, set_value};
