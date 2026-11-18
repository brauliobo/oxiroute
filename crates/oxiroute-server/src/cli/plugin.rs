//! Commands that report as monitoring plugins: a verdict and a Nagios exit code (0 OK, 1 WARNING, 2 CRITICAL,
//! 3 UNKNOWN) instead of the exit categories 2 to 9 of the other commands.

use std::{ffi::OsString, process::ExitCode};

use clap::{ArgMatches, CommandFactory as _};
use serde::Serialize;
use serde_json::{Value, json};

use super::{Cli, context::Context, global::Output, render};
use crate::observability::status::check::{Finding, Verdict};

pub(super) const EXIT_UNKNOWN: u8 = 3;

const COMMANDS: [&[&str]; 3] = [&["check"], &["tls", "check"], &["doctor"]];

#[cfg(test)]
mod tests;

#[derive(Debug)]
pub(super) struct Report {
    pub(super) code: u8,
    pub(super) line: String,
    pub(super) json: Value,
}

impl Report {
    pub(super) fn unknown(subject: &str, reason: &str) -> Self {
        let line = format!("{subject} UNKNOWN - {reason}");
        Self {
            code: EXIT_UNKNOWN,
            json: json!({ "status": "unknown", "exitCode": EXIT_UNKNOWN, "summary": line, "findings": [] }),
            line,
        }
    }

    pub(super) fn judged(verdict: &Verdict, line: String, details: &impl Serialize) -> Self {
        Self {
            code: verdict.exit_code(),
            json: json!({
                "status": verdict.label().to_lowercase(),
                "exitCode": verdict.exit_code(),
                "summary": line,
                "findings": verdict.findings,
                "details": json!(details),
            }),
            line,
        }
    }
}

pub(super) fn plugin_line(subject: &str, verdict: &Verdict, ok: &str, data: &str) -> String {
    let text = if verdict.findings.is_empty() {
        ok.to_owned()
    } else {
        verdict
            .findings
            .iter()
            .map(|Finding { message, .. }| message.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!("{subject} {} - {text} | {data}", verdict.label())
}

pub(super) fn finish(context: &Context<'_>, json: bool, report: &Report) -> ExitCode {
    if !context.quiet() {
        let text = if json || matches!(context.output(), Output::Json) {
            report.json.to_string()
        } else {
            report.line.clone()
        };
        drop(render::emit(&format!("{text}\n")));
    }
    ExitCode::from(report.code)
}

/// The command words when the arguments name a plugin command, even when they are otherwise invalid.
pub(super) fn invoked(arguments: &[OsString]) -> Option<String> {
    let matches = Cli::command()
        .ignore_errors(true)
        .try_get_matches_from(arguments)
        .ok()?;
    let words = path(&matches);
    COMMANDS
        .contains(&words.as_slice())
        .then(|| words.join(" "))
}

fn path(matches: &ArgMatches) -> Vec<&str> {
    let mut words = Vec::new();
    let mut current = matches;
    while let Some((name, inner)) = current.subcommand() {
        words.push(name);
        current = inner;
    }
    words
}

/// The plugin line of a usage error: the first line of clap's message, since the line is one line.
pub(super) fn usage_line(command: &str, error: &clap::Error) -> String {
    let message = error.render().to_string();
    let reason = message.lines().next().unwrap_or_default();
    let reason = reason.strip_prefix("error: ").unwrap_or(reason);
    format!("OXIROUTE UNKNOWN - {reason} (see `oxiroute {command} --help`)")
}
