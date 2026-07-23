use std::cmp::Ordering;

use crate::Span;

/// A native value is not valid for its directive.
pub const E_INVALID_VALUE: DiagnosticCode = DiagnosticCode::new("E_INVALID_VALUE");

/// Two native objects claim the same effective identity.
pub const E_DUPLICATE_IDENTITY: DiagnosticCode = DiagnosticCode::new("E_DUPLICATE_IDENTITY");

/// A native reference has no unique declaration in the imported graph.
pub const E_UNRESOLVED_REFERENCE: DiagnosticCode = DiagnosticCode::new("E_UNRESOLVED_REFERENCE");

/// A reachable native feature is outside the supported import subset.
pub const E_UNSUPPORTED_FEATURE: DiagnosticCode = DiagnosticCode::new("E_UNSUPPORTED_FEATURE");

/// Native behavior cannot be translated without changing its semantics.
pub const E_SEMANTICS_NOT_REPRESENTABLE: DiagnosticCode =
    DiagnosticCode::new("E_SEMANTICS_NOT_REPRESENTABLE");

/// Stable machine-readable identifier for one diagnostic class.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DiagnosticCode(&'static str);

impl DiagnosticCode {
    #[must_use]
    pub const fn new(code: &'static str) -> Self {
        Self(code)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DiagnosticStage {
    Source,
    Lex,
    Parse,
    Resolve,
    Lower,
    Validate,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RelatedSpan {
    span: Span,
    message: String,
}

impl RelatedSpan {
    #[must_use]
    pub fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn span(&self) -> Span {
        self.span
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    code: DiagnosticCode,
    severity: Severity,
    stage: DiagnosticStage,
    message: String,
    primary_span: Option<Span>,
    include_stack: Vec<Span>,
    related_spans: Vec<RelatedSpan>,
    help: Option<String>,
}

impl Diagnostic {
    #[must_use]
    pub fn new(
        code: DiagnosticCode,
        severity: Severity,
        stage: DiagnosticStage,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            severity,
            stage,
            message: message.into(),
            primary_span: None,
            include_stack: Vec::new(),
            related_spans: Vec::new(),
            help: None,
        }
    }

    #[must_use]
    pub fn with_primary_span(mut self, span: Span) -> Self {
        self.primary_span = Some(span);
        self
    }

    #[must_use]
    pub fn with_related_span(mut self, span: Span, message: impl Into<String>) -> Self {
        self.related_spans.push(RelatedSpan::new(span, message));
        self
    }

    #[must_use]
    pub fn with_include_stack(mut self, stack: impl IntoIterator<Item = Span>) -> Self {
        self.include_stack = stack.into_iter().collect();
        self
    }

    #[must_use]
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    #[must_use]
    pub const fn code(&self) -> DiagnosticCode {
        self.code
    }

    #[must_use]
    pub const fn severity(&self) -> Severity {
        self.severity
    }

    #[must_use]
    pub const fn stage(&self) -> DiagnosticStage {
        self.stage
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub const fn primary_span(&self) -> Option<Span> {
        self.primary_span
    }

    #[must_use]
    pub fn include_stack(&self) -> &[Span] {
        &self.include_stack
    }

    #[must_use]
    pub fn related_spans(&self) -> &[RelatedSpan] {
        &self.related_spans
    }

    #[must_use]
    pub fn help(&self) -> Option<&str> {
        self.help.as_deref()
    }
}

/// A partial or complete value and its deterministically ordered diagnostics.
///
/// Located diagnostics sort by source range. At one location, errors precede warnings, followed
/// by code, stage, message, include stack, related spans, and help. Unlocated diagnostics sort
/// last.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Report<T> {
    value: T,
    diagnostics: Vec<Diagnostic>,
}

impl<T> Report<T> {
    #[must_use]
    pub fn new(value: T, mut diagnostics: Vec<Diagnostic>) -> Self {
        diagnostics.sort_by(compare_diagnostics);
        Self { value, diagnostics }
    }

    #[must_use]
    pub const fn value(&self) -> &T {
        &self.value
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
    }

    #[must_use]
    pub fn into_parts(self) -> (T, Vec<Diagnostic>) {
        (self.value, self.diagnostics)
    }
}

fn compare_diagnostics(left: &Diagnostic, right: &Diagnostic) -> Ordering {
    compare_optional_spans(left.primary_span, right.primary_span)
        .then_with(|| left.severity.cmp(&right.severity))
        .then_with(|| left.code.cmp(&right.code))
        .then_with(|| left.stage.cmp(&right.stage))
        .then_with(|| left.message.cmp(&right.message))
        .then_with(|| left.include_stack.cmp(&right.include_stack))
        .then_with(|| left.related_spans.cmp(&right.related_spans))
        .then_with(|| left.help.cmp(&right.help))
}

fn compare_optional_spans(left: Option<Span>, right: Option<Span>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}
