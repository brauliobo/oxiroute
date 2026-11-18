use std::ffi::OsString;

use clap::CommandFactory as _;

use super::{Report, invoked, usage_line};
use crate::cli::Cli;

fn words(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

#[test]
fn unknown_reports_use_exit_code_three_and_a_plugin_line() {
    let report = Report::unknown(
        "OXIROUTE",
        "status page unavailable: endpoint connection failed",
    );
    assert_eq!(report.code, 3);
    assert_eq!(
        report.line,
        "OXIROUTE UNKNOWN - status page unavailable: endpoint connection failed"
    );
    assert_eq!(report.json["status"], "unknown");
}

#[test]
fn plugin_commands_are_recognised_even_when_their_arguments_are_invalid() {
    let named = |arguments: &[&str]| invoked(&words(arguments));
    assert_eq!(
        named(&["oxiroute", "check", "--bogus"]).as_deref(),
        Some("check")
    );
    assert_eq!(
        named(&["oxiroute", "--output", "json", "check", "--pool"]).as_deref(),
        Some("check")
    );
    assert_eq!(
        named(&["oxiroute", "tls", "check", "--warn"]).as_deref(),
        Some("tls check")
    );
    assert_eq!(named(&["oxiroute", "tls", "list", "--bogus"]), None);
    assert_eq!(named(&["oxiroute", "server", "list", "--bogus"]), None);
}

#[test]
fn a_usage_error_names_the_command_to_ask_for_help() {
    let error = Cli::command()
        .try_get_matches_from(["oxiroute", "check", "--bogus"])
        .expect_err("usage error");
    let line = usage_line("check", &error);
    assert!(line.starts_with("OXIROUTE UNKNOWN - "), "{line}");
    assert!(line.ends_with("(see `oxiroute check --help`)"), "{line}");
}
