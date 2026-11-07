//! `oxiroute check`: one line and a Nagios exit code (0 OK, 1 WARNING, 2 CRITICAL, 3 UNKNOWN) for
//! monitoring systems and cron. It is the one command that does not use the 2..9 exit categories.

use std::time::Instant;

use crate::{
    status_check::{Finding, Thresholds, Verdict, plugin_line},
    status_summary::{Severity, StatusSummary},
};

#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) const EXIT_UNKNOWN: u8 = 3;

#[cfg(test)]
mod tests;

/// Whether the arguments name `check`, even when they are otherwise invalid.
pub(super) fn is_invocation(arguments: &[OsString]) -> bool {
    use clap::CommandFactory;

    <Cli as CommandFactory>::command()
        .ignore_errors(true)
        .try_get_matches_from(arguments)
        .is_ok_and(|matches| matches.subcommand_name() == Some("check"))
}

#[derive(Args)]
pub struct CheckArgs {
    /// Judge only this pool; repeat for several. Default: every pool.
    #[arg(long = "pool", value_parser = nonempty_target)]
    pools: Vec<String>,
    /// WARNING when a pool has fewer available servers than this. Default: any server missing.
    #[arg(long, value_name = "N")]
    warn_available_below: Option<u16>,
    /// CRITICAL when a pool has fewer available servers than this.
    #[arg(long, value_name = "N", default_value_t = 1, value_parser = clap::value_parser!(u16).range(1..))]
    crit_available_below: u16,
    /// WARNING when the oldest queued request has waited longer, for example 5s.
    #[arg(long, value_name = "AGE", default_value = "5s", value_parser = parse_age_ms)]
    max_queue_wait: u64,
    /// WARNING when a pool holds a request older than this, for example 60s. Default: not judged.
    #[arg(long, value_name = "AGE", value_parser = parse_age_ms)]
    max_inflight_age: Option<u64>,
    /// WARNING when a retired generation still drains after this long, for example 1h.
    #[arg(long, value_name = "AGE", default_value = "1h", value_parser = parse_age_ms)]
    max_retired_age: u64,
    /// Print one JSON object instead of the plugin line.
    #[arg(long)]
    json: bool,
    /// Also request `--url` the way a client would and require `--expect-status`.
    #[arg(long, requires = "url")]
    canary: bool,
    /// Plain `http://host:port/path` to request for `--canary`.
    #[arg(long, value_name = "URL", requires = "canary")]
    url: Option<String>,
    /// Host header for `--canary`; default: the authority of `--url`.
    #[arg(long, value_name = "HOST", requires = "canary")]
    host: Option<String>,
    /// Status `--canary` must answer.
    #[arg(
        long,
        value_name = "STATUS",
        default_value_t = 200,
        requires = "canary"
    )]
    expect_status: u16,
    /// WARNING when the `--canary` answer takes longer, for example 2s. Default: not judged.
    #[arg(long, value_name = "AGE", value_parser = parse_age_ms, requires = "canary")]
    canary_max_latency: Option<u64>,
}

impl CheckArgs {
    fn thresholds(&self) -> Thresholds {
        Thresholds {
            warn_available_below: self.warn_available_below.map(usize::from),
            crit_available_below: usize::from(self.crit_available_below),
            max_queue_wait_ms: self.max_queue_wait,
            max_inflight_age_ms: self.max_inflight_age,
            max_retired_age_seconds: self.max_retired_age / 1_000,
        }
    }
}

#[derive(Debug)]
pub(super) struct Report {
    pub(super) code: u8,
    pub(super) line: String,
    pub(super) json: Value,
}

impl Report {
    fn unknown(reason: &str) -> Self {
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

pub(super) fn run(cli: &Cli, args: &CheckArgs) -> ExitCode {
    let report = collect(cli, args).unwrap_or_else(|reason| Report::unknown(&reason));
    if !cli.quiet {
        if args.json || matches!(cli.output, Output::Json) {
            println!("{}", report.json);
        } else {
            println!("{}", report.line);
        }
    }
    ExitCode::from(report.code)
}

fn collect(cli: &Cli, args: &CheckArgs) -> Result<Report, String> {
    let endpoint = match &cli.socket {
        Some(path) => Endpoint::unix(&path.to_string_lossy()),
        None => Endpoint::parse(&cli.endpoint),
    }
    .map_err(|error| error.to_string())?;
    let document = Client {
        endpoint,
        token: None,
    }
    .get("/status.json", false)
    .and_then(|response| response.json())
    .map_err(|error| format!("status page unavailable: {error}"))?;
    let canary = args.canary.then(|| canary(args)).transpose()?;
    assess(document, args, canary)
}

/// Requests `--url` as a client would; a wrong answer is a finding, a bad URL is a usage error.
fn canary(args: &CheckArgs) -> Result<Finding, String> {
    let url = args.url.as_deref().unwrap_or_default();
    let rest = url
        .strip_prefix("http://")
        .ok_or("--url must start with http://")?;
    let (authority, path) = rest.find('/').map_or((rest, "/"), |at| rest.split_at(at));
    let host = args.host.as_deref().unwrap_or(authority);
    let endpoint = Endpoint::parse(&format!("http://{authority}")).map_err(|e| e.to_string())?;
    let started = Instant::now();
    let answer = endpoint.get_unchecked(path, host);
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
