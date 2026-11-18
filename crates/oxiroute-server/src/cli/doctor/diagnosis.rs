use std::{cmp::Reverse, fmt::Write as _};

use serde::Serialize;
use serde_json::json;

use crate::{cli::plugin::Report, observability::status::summary::Severity};

const SUBJECT: &str = "OXIROUTE DOCTOR";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Check {
    Daemon,
    Config,
    Files,
    Listeners,
    Upstreams,
    Certificates,
    Unit,
}

impl Check {
    const fn name(self) -> &'static str {
        match self {
            Self::Daemon => "daemon",
            Self::Config => "config",
            Self::Files => "files",
            Self::Listeners => "listeners",
            Self::Upstreams => "upstreams",
            Self::Certificates => "certificates",
            Self::Unit => "unit",
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Diagnosis {
    pub(super) severity: Severity,
    pub(super) check: Check,
    pub(super) subject: String,
    pub(super) message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) fix: Option<String>,
}

impl Diagnosis {
    pub(super) fn new(
        severity: Severity,
        check: Check,
        subject: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity,
            check,
            subject: subject.into(),
            message: message.into(),
            fix: None,
        }
    }

    pub(super) fn ok(check: Check, subject: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(Severity::Ok, check, subject, message)
    }

    pub(super) fn warning(
        check: Check,
        subject: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::new(Severity::Warning, check, subject, message)
    }

    pub(super) fn critical(
        check: Check,
        subject: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::new(Severity::Critical, check, subject, message)
    }

    pub(super) fn fixed_by(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }

    fn write(&self, text: &mut String) {
        let label = match self.severity {
            Severity::Ok => "OK",
            Severity::Warning => "WARNING",
            Severity::Critical => "CRITICAL",
        };
        let _ = write!(
            text,
            "\n{label:<8} {:<12} {}: {}",
            self.check.name(),
            self.subject,
            self.message
        );
        if let Some(fix) = &self.fix {
            let _ = write!(text, "\n    fix: {fix}");
        }
    }
}

pub(super) fn report(mut diagnoses: Vec<Diagnosis>) -> Report {
    diagnoses.sort_by_key(|diagnosis| Reverse(diagnosis.severity));
    let count = |severity| {
        diagnoses
            .iter()
            .filter(|diagnosis| diagnosis.severity == severity)
            .count()
    };
    let (critical, warning, ok) = (
        count(Severity::Critical),
        count(Severity::Warning),
        count(Severity::Ok),
    );
    let (code, label) = match (critical, warning) {
        (0, 0) => (0, "OK"),
        (0, _) => (1, "WARNING"),
        _ => (2, "CRITICAL"),
    };
    let summary = format!("{SUBJECT} {label} - {critical} critical, {warning} warning, {ok} ok");
    let mut line = summary.clone();
    for diagnosis in &diagnoses {
        diagnosis.write(&mut line);
    }
    Report {
        code,
        json: json!({
            "status": label.to_lowercase(),
            "exitCode": code,
            "summary": summary,
            "findings": diagnoses,
        }),
        line,
    }
}
