#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn sorted_diagnostics(diagnostics: &[Diagnostic]) -> Vec<Diagnostic> {
    Report::new((), diagnostics.to_vec()).into_parts().1
}

pub(super) fn diagnostic_evidence(diagnostic: &Diagnostic) -> DiagnosticEvidence {
    DiagnosticEvidence {
        code: diagnostic.code().as_str().into(),
        severity: snake_case(&format!("{:?}", diagnostic.severity())),
        stage: snake_case(&format!("{:?}", diagnostic.stage())),
        message: diagnostic.message().to_owned(),
        primary_span: diagnostic.primary_span().map(span_evidence),
        include_stack: diagnostic
            .include_stack()
            .iter()
            .copied()
            .map(span_evidence)
            .collect(),
        related_spans: diagnostic
            .related_spans()
            .iter()
            .map(|related| RelatedSpanEvidence {
                span: span_evidence(related.span()),
                message: related.message().to_owned(),
            })
            .collect(),
        help: diagnostic.help().map(str::to_owned),
    }
}

pub(super) fn diagnostic_blockers(diagnostics: &[Diagnostic]) -> Vec<ImportBlocker> {
    diagnostics
        .iter()
        .enumerate()
        .filter(|(_, diagnostic)| diagnostic.severity() == Severity::Error)
        .map(|(index, diagnostic)| {
            let mut origins = diagnostic
                .primary_span()
                .into_iter()
                .map(span_evidence)
                .collect::<Vec<_>>();
            origins.extend(
                diagnostic
                    .include_stack()
                    .iter()
                    .copied()
                    .map(span_evidence),
            );
            ImportBlocker {
                id: format!("diagnostic-{index:04}"),
                kind: "diagnostic".into(),
                code: diagnostic.code().as_str().into(),
                message: diagnostic.message().to_owned(),
                scope: None,
                occurrence_ids: Vec::new(),
                origins,
            }
        })
        .collect()
}

fn typed_blocker(
    id: String,
    kind: impl Into<String>,
    code: crate::DiagnosticCode,
    message: String,
    scope: Option<String>,
    occurrence_ids: Vec<String>,
    origins: Vec<SpanEvidence>,
) -> ImportBlocker {
    ImportBlocker {
        id,
        kind: kind.into(),
        code: code.as_str().into(),
        message,
        scope,
        occurrence_ids,
        origins,
    }
}

pub(super) fn varnish_blockers(report: &crate::varnish::ImportReport) -> Vec<ImportBlocker> {
    let crate::varnish::LoweringStatus::Blocked(blocker) = report.lowering else {
        return Vec::new();
    };
    let (code, message) = match blocker {
        crate::varnish::LoweringBlocker::NoCanonicalGraph => (
            crate::varnish::E_VCL_LOWERING_BLOCKED,
            "Varnish source contains no canonical HTTP lowering graph",
        ),
        crate::varnish::LoweringBlocker::InvalidSource => (
            crate::varnish::E_VCL_LOWERING_BLOCKED,
            "Varnish source is invalid for exact canonical lowering",
        ),
        crate::varnish::LoweringBlocker::UnsupportedBehavior => (
            crate::varnish::E_VCL_LOWERING_BLOCKED,
            "Varnish behavior is outside the exact canonical lowering subset",
        ),
        crate::varnish::LoweringBlocker::UnsupportedSubroutine => (
            crate::varnish::E_VCL_UNSUPPORTED_SUBROUTINE,
            "Varnish subroutine behavior is outside the exact canonical lowering graph",
        ),
        crate::varnish::LoweringBlocker::SemanticMismatch => (
            crate::varnish::E_VCL_SEMANTIC_MISMATCH,
            "Varnish behavior has no semantics-preserving canonical representation",
        ),
        crate::varnish::LoweringBlocker::Invocation => (
            crate::varnish::E_VCL_LOWERING_BLOCKED,
            "Varnish invocation facts are insufficient for exact canonical lowering",
        ),
        crate::varnish::LoweringBlocker::Validation => (
            crate::E_INVALID_VALUE,
            "lowered Varnish configuration failed canonical validation",
        ),
    };
    let origins = report
        .source_graph
        .root
        .and_then(|root| report.source_graph.source(root))
        .map(|source| span_evidence(source.source.full_span()))
        .into_iter()
        .collect();
    vec![typed_blocker(
        "varnish-lowering".into(),
        "lowering",
        code,
        message.into(),
        None,
        Vec::new(),
        origins,
    )]
}

fn message_for_code(diagnostics: &[Diagnostic], code: crate::DiagnosticCode) -> String {
    diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code() == code)
        .map_or_else(
            || format!("import blocked by diagnostic {}", code.as_str()),
            |diagnostic| diagnostic.message().to_owned(),
        )
}

pub(super) fn nginx_blockers(report: &crate::nginx::NginxImportReport) -> Vec<ImportBlocker> {
    let mut blockers = Vec::new();
    for blocked in &report.blocked_http_services {
        let origins = blocked
            .servers
            .iter()
            .filter_map(|occurrence| nginx_occurrence_span(report, *occurrence))
            .map(span_evidence)
            .collect::<Vec<_>>();
        for code in &blocked.diagnostic_codes {
            blockers.push(typed_blocker(
                format!("nginx-http:{}:{}", blocked.path, code.as_str()),
                "service",
                *code,
                message_for_code(&report.diagnostics, *code),
                Some(blocked.path.clone()),
                blocked
                    .servers
                    .iter()
                    .map(|occurrence| occurrence.get().to_string())
                    .collect(),
                origins.clone(),
            ));
        }
    }
    for blocked in &report.blocked_rtmp_services {
        let origins: Vec<SpanEvidence> = nginx_occurrence_span(report, blocked.server)
            .into_iter()
            .map(span_evidence)
            .collect();
        for code in &blocked.diagnostic_codes {
            blockers.push(typed_blocker(
                format!("nginx-rtmp:{}:{}", blocked.path, code.as_str()),
                "rtmp_service",
                *code,
                message_for_code(&report.diagnostics, *code),
                Some(blocked.path.clone()),
                vec![blocked.server.get().to_string()],
                origins.clone(),
            ));
        }
    }
    for blocked in &report.blocked_stream_services {
        let origins: Vec<SpanEvidence> = nginx_occurrence_span(report, blocked.server)
            .into_iter()
            .map(span_evidence)
            .collect();
        for code in &blocked.diagnostic_codes {
            blockers.push(typed_blocker(
                format!("nginx-stream:{}:{}", blocked.path, code.as_str()),
                "stream_service",
                *code,
                message_for_code(&report.diagnostics, *code),
                Some(blocked.path.clone()),
                vec![blocked.server.get().to_string()],
                origins.clone(),
            ));
        }
    }
    blockers
}

fn nginx_occurrence_span(
    report: &crate::nginx::NginxImportReport,
    occurrence: crate::nginx::OccurrenceId,
) -> Option<Span> {
    report
        .http_occurrence_ledger
        .iter()
        .find(|decision| decision.occurrence == occurrence)
        .map(|decision| decision.span)
        .or_else(|| {
            report
                .rtmp_occurrence_ledger
                .iter()
                .find(|decision| decision.occurrence == occurrence)
                .map(|decision| decision.span)
        })
        .or_else(|| {
            report
                .stream_occurrence_ledger
                .iter()
                .find(|decision| decision.occurrence == occurrence)
                .map(|decision| decision.span)
        })
}

pub(super) fn apache_blockers(report: &crate::apache::ApacheImportReport) -> Vec<ImportBlocker> {
    report
        .blocked_virtual_hosts
        .iter()
        .flat_map(|blocked| {
            blocked.diagnostic_codes.iter().map(|code| {
                let mut origins = vec![span_evidence(blocked.origin.span)];
                origins.extend(
                    blocked
                        .origin
                        .include_stack
                        .iter()
                        .map(|frame| span_evidence(frame.directive_span)),
                );
                typed_blocker(
                    format!("apache-vhost:{}:{}", blocked.address, code.as_str()),
                    "virtual_host",
                    *code,
                    message_for_code(&report.diagnostics, *code),
                    Some(blocked.address.to_string()),
                    Vec::new(),
                    origins,
                )
            })
        })
        .collect()
}

#[cfg(unix)]
pub(super) fn squid_blockers(report: &crate::squid::ImportReport) -> Vec<ImportBlocker> {
    report
        .blocked_capabilities
        .iter()
        .flat_map(|blocked| {
            let scope = snake_case(&format!("{:?}", blocked.kind));
            let origins = blocked
                .occurrences
                .iter()
                .filter_map(|occurrence| {
                    report
                        .effective
                        .ledger
                        .decision(*occurrence)
                        .map(|decision| span_evidence(decision.origin.directive_span))
                })
                .collect::<Vec<_>>();
            blocked.occurrences.iter().map(move |occurrence| {
                typed_blocker(
                    format!("squid-{}:{}", scope, occurrence.get()),
                    "capability",
                    blocked.diagnostic_code,
                    message_for_code(&report.diagnostics, blocked.diagnostic_code),
                    Some(scope.clone()),
                    vec![occurrence.get().to_string()],
                    origins.clone(),
                )
            })
        })
        .collect()
}

pub(super) fn snake_case(value: &str) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    let mut result = String::with_capacity(value.len() + 4);
    for (index, character) in chars.iter().copied().enumerate() {
        if character.is_ascii_uppercase() && index > 0 {
            let previous = chars[index - 1];
            let next_is_lowercase = chars.get(index + 1).is_some_and(char::is_ascii_lowercase);
            if previous.is_ascii_lowercase()
                || previous.is_ascii_digit()
                || (previous.is_ascii_uppercase() && next_is_lowercase)
            {
                result.push('_');
            }
        }
        result.push(character.to_ascii_lowercase());
    }
    result
}
