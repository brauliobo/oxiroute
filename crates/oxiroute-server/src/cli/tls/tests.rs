use clap::Parser as _;
use serde_json::json;

use super::{
    Expiry, TlsCommand,
    check::{CheckArgs, assess_inventory},
    served::Target,
};
use crate::cli::{Cli, Command};

const NOW: i64 = 1_893_456_000;

fn args(extra: &[&str]) -> CheckArgs {
    let mut words = vec!["oxiroute", "tls", "check"];
    words.extend_from_slice(extra);
    match Cli::try_parse_from(words)
        .expect("tls check arguments")
        .command
    {
        Command::Tls {
            command: TlsCommand::Check(args),
        } => args,
        _ => panic!("tls check command"),
    }
}

fn inventory(expires: &[(&str, &str)]) -> serde_json::Value {
    json!({ "certificates": expires.iter().map(|(name, at)| json!({
        "name": name, "status": { "expiresAt": at }
    })).collect::<Vec<_>>() })
}

#[test]
fn openssl_dates_are_read_as_utc() {
    let expiry = Expiry::parse_openssl("Jan  3 00:00:00 2030 GMT").expect("date");
    assert_eq!(expiry.unix_seconds(), NOW + 2 * 86_400);
    assert_eq!(expiry.utc(), "2030-01-03T00:00:00Z");
    assert!(Expiry::parse_openssl("Jan  3 00:00:00 2030 CET").is_none());
    assert!(Expiry::parse_openssl("Foo  3 00:00:00 2030 GMT").is_none());
    assert!(Expiry::parse_openssl("soon").is_none());
}

#[test]
fn the_unix_seconds_of_an_entry_win_over_its_text() {
    let status = json!({ "notAfterUnixSeconds": NOW, "notAfter": "Jan  3 00:00:00 2030 GMT" });
    assert_eq!(Expiry::of_status(&status), Some(Expiry::from_unix(NOW)));
}

#[test]
fn relative_expiry_reads_in_both_directions() {
    let expiry = Expiry::from_unix(NOW);
    assert_eq!(expiry.relative(NOW - 90_000), "in 1d01h");
    assert_eq!(expiry.relative(NOW + 7_500), "expired 2h05m ago");
}

#[test]
fn a_distant_expiry_is_ok_and_reports_the_soonest_certificate() {
    let report = assess_inventory(
        &inventory(&[
            ("www", "Mar  1 00:00:00 2030 GMT"),
            ("api", "Feb  1 00:00:00 2030 GMT"),
        ]),
        &args(&[]),
        NOW,
    )
    .expect("report");
    assert_eq!(report.code, 0);
    assert!(
        report.line.starts_with(
            "OXIROUTE TLS OK - 2 certificates, soonest api expires 2030-02-01T00:00:00Z (in 31d00h) | "
        ),
        "{}",
        report.line
    );
    assert!(
        report.line.contains("'api'=2678400s;2592000;604800;0"),
        "{}",
        report.line
    );
}

#[test]
fn an_expiry_inside_the_warning_window_warns_and_inside_the_critical_window_is_critical() {
    let warn = assess_inventory(
        &inventory(&[("www", "Jan 20 00:00:00 2030 GMT")]),
        &args(&[]),
        NOW,
    )
    .expect("report");
    assert_eq!(warn.code, 1);
    assert!(
        warn.line
            .contains("www expires 2030-01-20T00:00:00Z (in 19d00h)"),
        "{}",
        warn.line
    );
    let critical = assess_inventory(
        &inventory(&[("www", "Jan  5 00:00:00 2030 GMT")]),
        &args(&[]),
        NOW,
    )
    .expect("report");
    assert_eq!(critical.code, 2);
    assert_eq!(critical.json["status"], "critical");
}

#[test]
fn an_expired_certificate_is_critical_and_says_so() {
    let report = assess_inventory(
        &inventory(&[("www", "Dec 30 00:00:00 2029 GMT")]),
        &args(&[]),
        NOW,
    )
    .expect("report");
    assert_eq!(report.code, 2);
    assert!(
        report
            .line
            .contains("www expired 2029-12-30T00:00:00Z (expired 2d00h ago)"),
        "{}",
        report.line
    );
}

#[test]
fn a_certificate_without_an_expiry_warns() {
    let report = assess_inventory(
        &json!({ "certificates": [{ "name": "www" }] }),
        &args(&[]),
        NOW,
    )
    .expect("report");
    assert_eq!(report.code, 1);
    assert!(
        report.line.contains("www reports no expiry"),
        "{}",
        report.line
    );
}

#[test]
fn a_wanted_certificate_the_daemon_does_not_have_is_unknown() {
    let error = assess_inventory(
        &inventory(&[("www", "Mar  1 00:00:00 2030 GMT")]),
        &args(&["--certificate", "api"]),
        NOW,
    )
    .expect_err("unknown certificate");
    assert_eq!(error, "certificate api does not exist");
}

#[test]
fn targets_need_a_host_and_a_numeric_port() {
    assert_eq!(
        Target::parse("example.test:443")
            .expect("target")
            .to_string(),
        "example.test:443"
    );
    assert_eq!(
        Target::parse("[::1]:8443").expect("target").to_string(),
        "[::1]:8443"
    );
    assert!(Target::parse("example.test").is_err());
    assert!(Target::parse("example.test:https").is_err());
    assert!(Target::parse(":443").is_err());
}

#[test]
fn the_thresholds_and_the_target_options_are_checked_by_the_parser() {
    assert!(Cli::try_parse_from(["oxiroute", "tls", "check", "--verify"]).is_err());
    assert!(
        Cli::try_parse_from(["oxiroute", "tls", "check", "a:1", "--certificate", "x"]).is_err()
    );
    assert!(Cli::try_parse_from(["oxiroute", "tls", "check", "--warn", "soon"]).is_err());
    assert!(Cli::try_parse_from(["oxiroute", "tls", "revoke", "www"]).is_err());
    assert!(Cli::try_parse_from(["oxiroute", "tls", "delete", "www"]).is_err());
    assert!(Cli::try_parse_from(["oxiroute", "tls", "revoke", "www", "--yes"]).is_ok());
}
