use std::{process::ExitCode, time::Instant};

use serde_json::{Value, json};

use super::{CheckArgs, EXIT_UNKNOWN};
use crate::{
    cli::{
        context::Context,
        global::Output,
        net::{Endpoint, Timeouts},
        render,
    },
    observability::status::{
        check::{Finding, Verdict, plugin_line},
        summary::{Severity, StatusSummary},
    },
};

#[derive(Debug)]
pub(super) struct Report {
    pub(super) code: u8,
    pub(super) line: String,
    pub(super) json: Value,
}

impl Report {
    pub(super) fn unknown(reason: &str) -> Self {
        let line = format!("OXIROUTE UNKNOWN - {reason}");
        Self {
            code: EXIT_UNKNOWN,
            json: json!({ "status": "unknown", "exitCode": EXIT_UNKNOWN, "summary": line, "findings": [] }),
            line,
        }
    }
}

/// Judges one status document against the operator's limits.
pub(super) fn assess(
    document: Value,
    args: &CheckArgs,
    canary: Option<Finding>,
) -> Result<Report, String> {
    let mut summary = serde_json::from_value::<StatusSummary>(document)
        .map_err(|error| format!("the status document is not understood: {error}"))?;
    if let Some(missing) = args
        .pools
        .iter()
        .find(|name| !summary.pools.iter().any(|pool| &pool.name == *name))
    {
        return Err(format!("pool {missing} does not exist"));
    }
    if !args.pools.is_empty() {
        summary.pools.retain(|pool| args.pools.contains(&pool.name));
    }
    let limits = args.thresholds();
    let mut verdict = summary.judge(&limits);
    verdict.extend(canary);
    let line = plugin_line(&summary, &verdict, &limits);
    let json = json!({
        "status": verdict.label().to_lowercase(),
        "exitCode": verdict.exit_code(),
        "summary": line,
        "findings": verdict.findings,
        "details": summary,
    });
    Ok(Report {
        code: verdict.exit_code(),
        line,
        json,
    })
}

pub(in crate::cli) fn run(context: &Context<'_>, args: &CheckArgs) -> ExitCode {
    let report = collect(context, args).unwrap_or_else(|reason| Report::unknown(&reason));
    if !context.quiet() {
        let text = if args.json || matches!(context.output(), Output::Json) {
            report.json.to_string()
        } else {
            report.line.clone()
        };
        drop(render::emit(&format!("{text}\n")));
    }
    ExitCode::from(report.code)
}

fn collect(context: &Context<'_>, args: &CheckArgs) -> Result<Report, String> {
    let client = context.public_client().map_err(|error| error.to_string())?;
    let document = client
        .get("/status.json", false)
        .and_then(|response| response.json())
        .map_err(|error| format!("status page unavailable: {error}"))?;
    let canary = args
        .canary
        .then(|| {
            let timeouts = context.timeouts().map_err(|error| error.to_string())?;
            canary(args, timeouts)
        })
        .transpose()?;
    assess(document, args, canary)
}

/// Requests `--url` as a client would; a wrong answer is a finding, a bad URL is a usage error.
fn canary(args: &CheckArgs, timeouts: Timeouts) -> Result<Finding, String> {
    let url = args.url.as_deref().unwrap_or_default();
    let rest = url
        .strip_prefix("http://")
        .ok_or("--url must start with http://")?;
    let (authority, path) = rest.find('/').map_or((rest, "/"), |at| rest.split_at(at));
    let host = args.host.as_deref().unwrap_or(authority);
    let endpoint = Endpoint::parse(&format!("http://{authority}")).map_err(|e| e.to_string())?;
    let started = Instant::now();
    let answer = endpoint.get_unchecked(path, host, timeouts);
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let at = |severity, message: String| Finding {
        severity,
        pool: None,
        message: format!("canary {url} {message}"),
    };
    Ok(match answer {
        Err(error) => at(Severity::Critical, format!("failed: {error}")),
        Ok(response) if response.status != args.expect_status => at(
            Severity::Critical,
            format!(
                "answered {} instead of {}",
                response.status, args.expect_status
            ),
        ),
        Ok(_)
            if args
                .canary_max_latency
                .is_some_and(|limit| elapsed_ms > limit) =>
        {
            at(Severity::Warning, format!("answered after {elapsed_ms} ms"))
        }
        Ok(_) => at(Severity::Ok, format!("answered in {elapsed_ms} ms")),
    })
}

impl Verdict {
    fn extend(&mut self, finding: Option<Finding>) {
        let Some(finding) = finding else { return };
        self.severity = self.severity.max(finding.severity);
        if finding.severity > Severity::Ok {
            self.findings.push(finding);
            self.findings
                .sort_by_key(|finding| std::cmp::Reverse(finding.severity));
        }
    }
}
