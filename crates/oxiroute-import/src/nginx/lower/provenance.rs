use crate::{Diagnostic, DiagnosticCode, DiagnosticStage, Severity};

use crate::nginx::{
    DirectiveOrigin, ExpandedOccurrence, OccurrenceDecision, OccurrenceDisposition, OccurrenceId,
};

use super::{LowerIssue, Lowerer};

#[derive(Clone)]
pub(super) struct PolicyValue {
    pub(super) arguments: Vec<Vec<u8>>,
    pub(super) origins: Vec<DirectiveOrigin>,
}

pub(super) fn collect_result<T>(
    result: Result<T, LowerIssue>,
    issues: &mut Vec<LowerIssue>,
) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(issue) => {
            issues.push(issue);
            None
        }
    }
}

pub(super) fn issue(
    origin: &DirectiveOrigin,
    code: DiagnosticCode,
    message: impl Into<String>,
) -> LowerIssue {
    LowerIssue {
        origin: origin.clone(),
        code,
        message: message.into(),
        emit: true,
    }
}

pub(super) fn lower_diagnostic(issue: &LowerIssue) -> Diagnostic {
    Diagnostic::new(
        issue.code,
        Severity::Error,
        DiagnosticStage::Lower,
        issue.message.clone(),
    )
    .with_primary_span(issue.origin.span)
    .with_include_stack(
        issue
            .origin
            .provenance
            .include_stack
            .iter()
            .map(|frame| frame.directive_span),
    )
}

pub(super) fn utf8(value: &[u8]) -> Option<&str> {
    std::str::from_utf8(value).ok()
}

impl Lowerer {
    pub(super) fn effective_list_policy(
        &self,
        scope: OccurrenceId,
        inherited_scope: OccurrenceId,
        name: &[u8],
    ) -> Vec<PolicyValue> {
        let direct = self.direct_policies(scope, name);
        if direct.is_empty() {
            self.direct_policies(inherited_scope, name)
        } else {
            direct
        }
    }

    pub(super) fn effective_list_policy_chain(
        &self,
        scope: OccurrenceId,
        name: &[u8],
    ) -> Vec<PolicyValue> {
        let mut current = Some(scope);
        while let Some(occurrence) = current {
            let direct = self.direct_policies(occurrence, name);
            if !direct.is_empty() {
                return direct;
            }
            current = self.occurrence(occurrence).and_then(|item| item.parent);
        }
        Vec::new()
    }

    pub(super) fn effective_policy(&self, scope: OccurrenceId, name: &[u8]) -> Option<PolicyValue> {
        let mut scopes = Vec::new();
        let mut current = Some(scope);
        while let Some(occurrence) = current {
            scopes.push(occurrence);
            current = self.occurrence(occurrence).and_then(|item| item.parent);
        }
        scopes.reverse();

        let mut effective = None;
        for scope in scopes {
            if let Some(value) = self.direct_policies(scope, name).last() {
                effective = Some(value.clone());
            }
        }
        effective
    }

    fn direct_policies(&self, scope: OccurrenceId, name: &[u8]) -> Vec<PolicyValue> {
        self.graph
            .expanded_occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.parent == Some(scope) && occurrence.directive.name.value == name
            })
            .map(|occurrence| PolicyValue {
                arguments: occurrence
                    .directive
                    .arguments
                    .iter()
                    .map(|argument| argument.value.clone())
                    .collect(),
                origins: vec![self.origin(occurrence.id)],
            })
            .collect()
    }

    pub(super) fn blocking_decisions(
        &self,
    ) -> impl Iterator<Item = (&OccurrenceDecision, DiagnosticCode)> {
        self.resolution
            .decisions
            .iter()
            .filter_map(|decision| match decision.disposition {
                OccurrenceDisposition::Blocking(code) => Some((decision, code)),
                OccurrenceDisposition::Resolved | OccurrenceDisposition::Structural => None,
            })
    }

    pub(super) fn blocking_subtree_issues(&self, root: OccurrenceId) -> Vec<LowerIssue> {
        self.blocking_decisions()
            .filter(|(decision, _)| self.is_descendant(decision.occurrence, root))
            .map(|(decision, code)| LowerIssue {
                origin: self.origin(decision.occurrence),
                code,
                message: "blocking nginx upstream directive is reachable".into(),
                emit: false,
            })
            .collect()
    }

    pub(super) fn child_below(
        &self,
        occurrence: OccurrenceId,
        ancestor: OccurrenceId,
    ) -> Option<OccurrenceId> {
        let mut current = occurrence;
        loop {
            let parent = self.occurrence(current)?.parent?;
            if parent == ancestor {
                return Some(current);
            }
            current = parent;
        }
    }

    pub(super) fn is_descendant(&self, occurrence: OccurrenceId, ancestor: OccurrenceId) -> bool {
        occurrence == ancestor || self.child_below(occurrence, ancestor).is_some()
    }

    pub(super) fn occurrence(&self, occurrence: OccurrenceId) -> Option<&ExpandedOccurrence> {
        self.graph
            .expanded_occurrences
            .get(occurrence.get())
            .filter(|item| item.id == occurrence)
    }

    pub(super) fn origin(&self, occurrence: OccurrenceId) -> DirectiveOrigin {
        let expanded = self
            .occurrence(occurrence)
            .expect("resolved occurrence is retained in source graph");
        DirectiveOrigin {
            occurrence,
            span: expanded.directive.span,
            provenance: expanded.provenance.clone(),
        }
    }
}
