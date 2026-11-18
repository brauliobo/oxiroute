use std::{process::ExitCode, time::Duration};

use clap::Args;
use serde_json::{Value, json};

use super::{
    expiry::Expiry,
    served::{self, Served, Target},
};
use crate::{
    cli::{
        context::Context,
        parse::{nonempty_target, parse_duration},
        plugin::{self, Report},
        units::now_unix_seconds,
    },
    observability::status::{
        check::{Finding, Verdict},
        summary::Severity,
    },
};

const SUBJECT: &str = "OXIROUTE TLS";

#[derive(Args)]
pub(crate) struct CheckArgs {
    /// HOST:PORT of a TLS server to handshake with; default: the certificates of the daemon.
    #[arg(value_name = "HOST:PORT", value_parser = Target::parse)]
    pub(super) target: Option<Target>,
    /// Judge only this certificate of the daemon; repeat for several. Default: every certificate.
    #[arg(long = "certificate", conflicts_with = "target", value_parser = nonempty_target)]
    pub(super) certificates: Vec<String>,
    /// WARNING when a certificate expires within this long, for example 30d.
    #[arg(long, value_name = "DURATION", default_value = "30d", value_parser = parse_duration)]
    pub(super) warn: Duration,
    /// CRITICAL when a certificate expires within this long, or has expired, for example 7d.
    #[arg(long, value_name = "DURATION", default_value = "7d", value_parser = parse_duration)]
    pub(super) crit: Duration,
    /// Server name to ask for with SNI; default: the host of HOST:PORT.
    #[arg(long, value_name = "NAME", requires = "target")]
    pub(super) servername: Option<String>,
    /// CRITICAL when the presented chain or server name is not trusted.
    #[arg(long, requires = "target")]
    pub(super) verify: bool,
    /// Extra trusted root certificates (PEM) for `--verify`.
    #[arg(long, value_name = "FILE", requires = "verify")]
    pub(super) ca_file: Option<std::path::PathBuf>,
    /// Print one JSON object instead of the plugin line.
    #[arg(long)]
    pub(super) json: bool,
}

struct Subject {
    name: String,
    expiry: Option<Expiry>,
    verification: Result<(), String>,
}

pub(in crate::cli) fn run(context: &Context<'_>, args: &CheckArgs) -> ExitCode {
    let report = collect(context, args).unwrap_or_else(|reason| Report::unknown(SUBJECT, &reason));
    plugin::finish(context, args.json, &report)
}

fn collect(context: &Context<'_>, args: &CheckArgs) -> Result<Report, String> {
    if args.crit > args.warn {
        return Err("--crit must not be longer than --warn".into());
    }
    match &args.target {
        Some(target) => Ok(assess(
            &[live(context, args, target)?],
            args,
            now_unix_seconds(),
        )),
        None => assess_inventory(&fetch_inventory(context)?, args, now_unix_seconds()),
    }
}

fn live(context: &Context<'_>, args: &CheckArgs, target: &Target) -> Result<Subject, String> {
    let served: Served = served::fetch(
        target,
        args.servername.as_deref(),
        args.ca_file.as_deref(),
        context.timeouts(),
    )
    .map_err(|error| error.to_string())?;
    Ok(Subject {
        name: target.to_string(),
        expiry: Some(served.not_after),
        verification: if args.verify {
            served.verification
        } else {
            Ok(())
        },
    })
}

fn fetch_inventory(context: &Context<'_>) -> Result<Value, String> {
    context
        .client()
        .and_then(|client| client.get("/api/v1/tls", true))
        .and_then(|response| response.json())
        .map_err(|error| format!("certificate inventory unavailable: {error}"))
}

pub(super) fn assess_inventory(
    document: &Value,
    args: &CheckArgs,
    now: i64,
) -> Result<Report, String> {
    let wanted = &args.certificates;
    let entries = document
        .get("certificates")
        .and_then(Value::as_array)
        .ok_or("the certificate inventory is not understood")?;
    let named = |entry: &Value, name: &String| entry["name"].as_str() == Some(name.as_str());
    if let Some(missing) = wanted
        .iter()
        .find(|name| !entries.iter().any(|entry| named(entry, name)))
    {
        return Err(format!("certificate {missing} does not exist"));
    }
    let subjects = entries
        .iter()
        .filter(|entry| wanted.is_empty() || wanted.iter().any(|name| named(entry, name)))
        .map(|entry| Subject {
            name: entry["name"].as_str().unwrap_or_default().to_owned(),
            expiry: Expiry::of_status(&entry["status"]),
            verification: Ok(()),
        })
        .collect::<Vec<_>>();
    Ok(assess(&subjects, args, now))
}

fn assess(subjects: &[Subject], args: &CheckArgs, now: i64) -> Report {
    let warn = i64::try_from(args.warn.as_secs()).unwrap_or(i64::MAX);
    let crit = i64::try_from(args.crit.as_secs()).unwrap_or(i64::MAX);
    let mut findings = Vec::new();
    for subject in subjects {
        findings.extend(judge(subject, warn, crit, now));
    }
    let verdict = Verdict::from_findings(findings);
    let soonest = subjects
        .iter()
        .filter_map(|subject| Some((subject, subject.expiry?)))
        .min_by_key(|(_, expiry)| expiry.unix_seconds());
    let ok = soonest.map_or_else(
        || "no certificates".to_owned(),
        |(subject, expiry)| {
            format!(
                "{} certificates, soonest {} expires {} ({})",
                subjects.len(),
                subject.name,
                expiry.utc(),
                expiry.relative(now)
            )
        },
    );
    let data = subjects
        .iter()
        .filter_map(|subject| {
            let label = subject.name.replace('\'', "_");
            Some(format!(
                "'{label}'={}s;{warn};{crit};0",
                subject.expiry?.remaining(now)
            ))
        })
        .collect::<Vec<_>>()
        .join(" ");
    let details = json!(
        subjects
            .iter()
            .map(|subject| json!({
                "name": subject.name,
                "expires": subject.expiry.map(Expiry::utc),
                "remainingSeconds": subject.expiry.map(|expiry| expiry.remaining(now)),
            }))
            .collect::<Vec<_>>()
    );
    let line = plugin::plugin_line(SUBJECT, &verdict, &ok, &data);
    Report::judged(&verdict, line, &details)
}

fn judge(subject: &Subject, warn: i64, crit: i64, now: i64) -> Vec<Finding> {
    let mut findings = Vec::new();
    if let Err(reason) = &subject.verification {
        findings.push(Finding::new(
            Severity::Critical,
            format!("{} is not trusted: {reason}", subject.name),
        ));
    }
    let Some(expiry) = subject.expiry else {
        findings.push(Finding::new(
            Severity::Warning,
            format!("{} reports no expiry", subject.name),
        ));
        return findings;
    };
    findings.extend(expiry.finding(&subject.name, warn, crit, now));
    findings
}
