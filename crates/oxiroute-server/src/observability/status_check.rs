//! Judges a status summary. The server stamps its `state` fields with the default thresholds and
//! `oxiroute check` applies the operator's, so both read one implementation.

use std::{cmp::Reverse, fmt::Write as _};

use serde::Serialize;

use crate::{
    observability::status_summary::{
        CanarySummary, GenerationSummary, PoolSummary, RegistrySummary, Severity, StatusSummary,
    },
    routing::CanaryResult,
};

/// A canary that failed this many runs in a row is an outage, not a blip.
const CANARY_CRITICAL_AFTER: u64 = 2;
/// A canary silent for this many of its own intervals is no longer running.
const CANARY_STALLED_INTERVALS: u64 = 3;

#[cfg(test)]
mod tests;

#[derive(Clone, Debug)]
pub(crate) struct Thresholds {
    /// Warn below this many available servers; `None` warns when any server of the pool is missing.
    pub(crate) warn_available_below: Option<usize>,
    pub(crate) crit_available_below: usize,
    pub(crate) max_queue_wait_ms: u64,
    /// Oldest in-flight request a pool may hold; `None` leaves slow requests to the overdue rule.
    pub(crate) max_inflight_age_ms: Option<u64>,
    pub(crate) max_retired_age_seconds: u64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            warn_available_below: None,
            crit_available_below: 1,
            max_queue_wait_ms: 5_000,
            max_inflight_age_ms: None,
            max_retired_age_seconds: 3_600,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Finding {
    pub(crate) severity: Severity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pool: Option<String>,
    pub(crate) message: String,
}

#[derive(Debug)]
pub(crate) struct Verdict {
    pub(crate) severity: Severity,
    pub(crate) findings: Vec<Finding>,
}

impl Thresholds {
    fn warn_below(&self, total: usize) -> usize {
        self.warn_available_below.unwrap_or(total)
    }
}

impl PoolSummary {
    pub(crate) fn judge(&self, limits: &Thresholds) -> Vec<Finding> {
        let at = |severity, message: String| Finding {
            severity,
            pool: Some(self.name.clone()),
            message: format!("pool {} {message}", self.name),
        };
        let mut findings = Vec::new();
        let counts = format!("{}/{} available", self.available, self.total);
        if self.available < limits.crit_available_below {
            findings.push(at(Severity::Critical, counts));
        } else if self.available < limits.warn_below(self.total) {
            findings.push(at(Severity::Warning, counts));
        }
        if self.queued > 0 && self.oldest_queue_wait_ms > limits.max_queue_wait_ms {
            let message = format!(
                "oldest queued request waited {} ms",
                self.oldest_queue_wait_ms
            );
            findings.push(at(Severity::Warning, message));
        }
        if limits
            .max_inflight_age_ms
            .is_some_and(|limit| self.oldest_inflight_ms > limit)
        {
            let message = format!("oldest request is {} ms old", self.oldest_inflight_ms);
            findings.push(at(Severity::Warning, message));
        }
        findings.extend(
            self.canary
                .iter()
                .flat_map(|canary| canary.judge(&self.name)),
        );
        findings
    }
}

impl CanarySummary {
    fn judge(&self, pool: &str) -> Vec<Finding> {
        let at = |severity, message: String| Finding {
            severity,
            pool: Some(pool.to_owned()),
            message: format!("pool {pool} {message}"),
        };
        let mut findings = Vec::new();
        match self.result {
            Some(CanaryResult::Failed) => {
                let severity = if self.consecutive_failures >= CANARY_CRITICAL_AFTER {
                    Severity::Critical
                } else {
                    Severity::Warning
                };
                let reason = self.reason.as_deref().unwrap_or("unknown");
                findings.push(at(severity, format!("canary failed ({reason})")));
            }
            Some(CanaryResult::Slow) => {
                let message = format!("canary answered slowly ({} ms)", self.duration_ms);
                findings.push(at(Severity::Warning, message));
            }
            _ => {}
        }
        let stalled_after = (self.interval_ms / 1_000).saturating_mul(CANARY_STALLED_INTERVALS);
        if let Some(age) = self
            .last_attempt_age_seconds
            .filter(|age| *age > stalled_after)
        {
            findings.push(at(
                Severity::Warning,
                format!("canary has not run for {age} s"),
            ));
        }
        findings
    }
}

fn worst(findings: &[Finding]) -> Severity {
    findings
        .iter()
        .map(|finding| finding.severity)
        .max()
        .unwrap_or(Severity::Ok)
}

impl StatusSummary {
    /// The pool's own severity under the default thresholds.
    pub(crate) fn pool_severity(pool: &PoolSummary) -> Severity {
        worst(&pool.judge(&Thresholds::default()))
    }

    pub(crate) fn judge(&self, limits: &Thresholds) -> Verdict {
        let global = |severity, message: String| Finding {
            severity,
            pool: None,
            message,
        };
        let mut findings = Vec::new();
        if !self.ready {
            findings.push(global(Severity::Critical, "not ready".into()));
        }
        findings.extend(self.pools.iter().flat_map(|pool| pool.judge(limits)));
        if self.overdue_requests > 0 {
            let message = format!("{} overdue requests", self.overdue_requests);
            findings.push(global(Severity::Critical, message));
        }
        if self.stuck_requests > 0 {
            let message = format!("{} stuck requests", self.stuck_requests);
            findings.push(global(Severity::Warning, message));
        }
        findings.extend(self.generation.judge(limits));
        findings.extend(self.registry.iter().flat_map(RegistrySummary::judge));
        findings.sort_by_key(|finding| Reverse(finding.severity));
        Verdict {
            severity: worst(&findings),
            findings,
        }
    }
}

impl GenerationSummary {
    fn judge(&self, limits: &Thresholds) -> Vec<Finding> {
        let warning = |message: String| Finding {
            severity: Severity::Warning,
            pool: None,
            message,
        };
        let mut findings = Vec::new();
        if let Some(rejection) = &self.last_rejection {
            findings.push(warning(format!(
                "configuration refused {} s ago, the previous generation keeps serving",
                rejection.age_seconds
            )));
        } else if self.degraded {
            findings.push(warning("generation degraded".into()));
        }
        if self.retired_oldest_age_seconds > limits.max_retired_age_seconds {
            findings.push(warning(format!(
                "retired generation still draining after {} s",
                self.retired_oldest_age_seconds
            )));
        }
        findings
    }
}

impl RegistrySummary {
    fn judge(&self) -> Vec<Finding> {
        let at = |severity, message: String| Finding {
            severity,
            pool: None,
            message,
        };
        let mut findings = Vec::new();
        if self.degraded {
            findings.push(at(Severity::Critical, "registry store degraded".into()));
        }
        for (count, what) in [
            (self.late_leases, "registry leases late for a heartbeat"),
            (
                self.shadowed,
                "registrations shadowed by a configured server",
            ),
            (
                self.orphaned,
                "registrations in pools that no longer accept them",
            ),
        ] {
            if count > 0 {
                findings.push(at(Severity::Warning, format!("{count} {what}")));
            }
        }
        findings
    }
}

impl Verdict {
    /// Nagios exit code of the verdict.
    pub(crate) const fn exit_code(&self) -> u8 {
        match self.severity {
            Severity::Ok => 0,
            Severity::Warning => 1,
            Severity::Critical => 2,
        }
    }

    pub(crate) const fn label(&self) -> &'static str {
        match self.severity {
            Severity::Ok => "OK",
            Severity::Warning => "WARNING",
            Severity::Critical => "CRITICAL",
        }
    }
}

/// The plugin line: `OXIROUTE CRITICAL - pool farm 0/4 available | farm_available=0;4;1`.
pub(crate) fn plugin_line(
    summary: &StatusSummary,
    verdict: &Verdict,
    limits: &Thresholds,
) -> String {
    let text = if verdict.findings.is_empty() {
        let (available, total) = summary
            .pools
            .iter()
            .fold((0, 0), |(a, t), pool| (a + pool.available, t + pool.total));
        format!(
            "{} pools, {available}/{total} servers available",
            summary.pools.len()
        )
    } else {
        verdict
            .findings
            .iter()
            .map(|finding| finding.message.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "OXIROUTE {} - {text} | {}",
        verdict.label(),
        performance_data(summary, limits)
    )
}

fn performance_data(summary: &StatusSummary, limits: &Thresholds) -> String {
    let mut data = String::new();
    for pool in &summary.pools {
        let name = &pool.name;
        let _ = write!(
            data,
            "{name}_available={};{};{} {name}_queued={} {name}_oldest_queue_wait_ms={} \
             {name}_inflight={} {name}_oldest_inflight_ms={} ",
            pool.available,
            limits.warn_below(pool.total),
            limits.crit_available_below,
            pool.queued,
            pool.oldest_queue_wait_ms,
            pool.inflight,
            pool.oldest_inflight_ms,
        );
        if let Some(canary) = pool
            .canary
            .as_ref()
            .filter(|canary| canary.result.is_some())
        {
            let _ = write!(data, "{name}_canary_ms={} ", canary.duration_ms);
        }
    }
    let _ = write!(
        data,
        "overdue={} stuck={} generation_age_s={} retired={} retired_oldest_age_s={}",
        summary.overdue_requests,
        summary.stuck_requests,
        summary.generation.age_seconds,
        summary.generation.retired,
        summary.generation.retired_oldest_age_seconds,
    );
    data
}
