//! `oxiroute tls` against a real daemon that serves a certificate file on its listener.

use oxiroute_config::{
    AlpnProtocol, Certificate, CertificateSource, TlsPolicy, TlsProfile, TlsVersion,
};
use serde_json::Value;
use tempfile::TempDir;

use super::{Daemon, fixture_support, reserve_tcp_address, stderr, stdout};

const HOST: &str = "proxy.example.test";
const EXPIRY: &str = "2051-03-13T22:11:07Z";

async fn daemon_serving_a_certificate(directory: &TempDir) -> Daemon {
    let (certificate, key) = fixture_support::write_test_identity(directory.path(), "edge-key.pem");
    Daemon::start_configured(reserve_tcp_address(), |draft| {
        draft.certificates = vec![Certificate {
            name: "edge".into(),
            dns_names: vec![HOST.into()],
            source: CertificateSource::Files {
                certificate_chain_path: certificate,
                private_key_path: key,
            },
        }];
        draft.tls_profiles = vec![TlsProfile {
            name: "edge".into(),
            certificates: vec!["edge".into()],
            default_certificate: "edge".into(),
            min_version: TlsVersion::Tls12,
            alpn: vec![AlpnProtocol::Http11],
            policy: TlsPolicy::default(),
        }];
        draft.listeners[0].tls_profile = Some("edge".into());
    })
    .await
}

#[tokio::test]
async fn list_and_show_report_the_expiry_in_utc_with_the_time_left() {
    let directory = TempDir::new().expect("directory");
    let daemon = daemon_serving_a_certificate(&directory).await;

    let list = daemon.run(&["tls", "list"]);
    assert!(list.status.success(), "{}", stderr(&list));
    let table = stdout(&list);
    assert!(table.contains("edge") && table.contains(EXPIRY), "{table}");
    assert!(table.contains(" in "), "{table}");

    let shown = daemon.run(&["tls", "show", "edge"]);
    assert!(shown.status.success(), "{}", stderr(&shown));
    assert!(stdout(&shown).contains(EXPIRY), "{}", stdout(&shown));

    let json = daemon.run(&["--output", "json", "tls", "show", "edge"]);
    let document = serde_json::from_slice::<Value>(&json.stdout).expect("JSON");
    assert_eq!(document["certificates"][0]["name"], "edge");

    let missing = daemon.run(&["tls", "show", "nosuch"]);
    assert_eq!(missing.status.code(), Some(6), "{}", stderr(&missing));
}

#[tokio::test]
async fn list_keeps_only_the_certificates_that_expire_within_the_window() {
    let directory = TempDir::new().expect("directory");
    let daemon = daemon_serving_a_certificate(&directory).await;

    let none = daemon.run(&["tls", "list", "--expiring-within", "30d"]);
    assert!(none.status.success(), "{}", stderr(&none));
    assert!(stdout(&none).is_empty(), "{}", stdout(&none));
    assert!(
        stderr(&none).contains("no certificates"),
        "{}",
        stderr(&none)
    );

    let some = daemon.run(&["tls", "list", "--expiring-within", "9500d"]);
    assert!(stdout(&some).contains("edge"), "{}", stdout(&some));
}

#[tokio::test]
async fn check_judges_the_certificates_of_the_daemon_with_monitoring_exit_codes() {
    let directory = TempDir::new().expect("directory");
    let daemon = daemon_serving_a_certificate(&directory).await;
    let verdict = |words: &[&str]| {
        let output = daemon.run(&[&["tls", "check"], words].concat());
        (output.status.code(), stdout(&output))
    };

    let (code, line) = verdict(&[]);
    assert_eq!(code, Some(0), "{line}");
    let ok = format!("OXIROUTE TLS OK - 1 certificates, soonest edge expires {EXPIRY} (in ");
    assert!(line.starts_with(&ok), "{line}");
    assert!(line.contains("'edge'="), "{line}");

    let (code, line) = verdict(&["--warn", "9500d", "--crit", "30d"]);
    assert_eq!(code, Some(1), "{line}");
    assert!(
        line.starts_with("OXIROUTE TLS WARNING - edge expires "),
        "{line}"
    );

    let (code, line) = verdict(&["--warn", "9500d", "--crit", "9500d"]);
    assert_eq!(code, Some(2), "{line}");
    assert!(line.starts_with("OXIROUTE TLS CRITICAL - "), "{line}");

    let (code, line) = verdict(&["--certificate", "nosuch"]);
    assert_eq!(code, Some(3), "{line}");
    assert!(
        line.contains("UNKNOWN - certificate nosuch does not exist"),
        "{line}"
    );

    let (code, line) = verdict(&["--warn", "7d", "--crit", "30d"]);
    assert_eq!(code, Some(3), "{line}");
    assert!(
        line.contains("--crit must not be longer than --warn"),
        "{line}"
    );

    let (code, line) = verdict(&["--warn", "soon"]);
    assert_eq!(code, Some(3), "{line}");
    assert!(line.starts_with("OXIROUTE UNKNOWN - "), "{line}");
    assert!(
        line.trim_end()
            .ends_with("(see `oxiroute tls check --help`)"),
        "{line}"
    );

    let (_, json) = verdict(&["--json", "--warn", "9500d"]);
    let report = serde_json::from_str::<Value>(&json).expect("JSON report");
    assert_eq!(report["status"], "warning");
    assert_eq!(report["details"][0]["name"], "edge");
}

#[tokio::test]
async fn check_and_inspect_read_the_certificate_a_server_presents() {
    let directory = TempDir::new().expect("directory");
    let daemon = daemon_serving_a_certificate(&directory).await;
    let target = daemon.traffic.to_string();
    let root = fixture_support::fixture("ca-a.pem");
    let root = root.to_str().expect("path");
    let check = |words: &[&str]| {
        let output =
            daemon.run(&[&["tls", "check", &target, "--servername", HOST], words].concat());
        (output.status.code(), stdout(&output))
    };

    let (code, line) = check(&[]);
    assert_eq!(code, Some(0), "{line}");
    assert!(line.contains(EXPIRY), "{line}");
    let (code, line) = check(&["--warn", "9500d", "--crit", "30d"]);
    assert_eq!(code, Some(1), "{line}");
    let (code, line) = check(&["--verify"]);
    assert_eq!(code, Some(2), "{line}");
    assert!(line.contains("is not trusted"), "{line}");
    let (code, line) = check(&["--verify", "--ca-file", root]);
    assert_eq!(code, Some(0), "{line}");

    let inspected = daemon.run(&[
        "--output",
        "json",
        "tls",
        "inspect",
        &target,
        "--servername",
        HOST,
    ]);
    assert!(inspected.status.success(), "{}", stderr(&inspected));
    let details = serde_json::from_slice::<Value>(&inspected.stdout).expect("JSON");
    assert_eq!(details["subject"], "CN=proxy.example.test");
    assert_eq!(details["notAfter"], EXPIRY);
    assert_eq!(details["dnsNames"][0], HOST);
    assert!(details["remainingSeconds"].as_i64().expect("seconds") > 0);

    let closed = reserve_tcp_address().to_string();
    let output = daemon.run(&["tls", "check", &closed]);
    assert_eq!(output.status.code(), Some(3), "{}", stdout(&output));
    assert!(
        stdout(&output).contains("UNKNOWN - cannot connect"),
        "{}",
        stdout(&output)
    );
}

#[tokio::test]
async fn acme_actions_on_a_certificate_without_a_managed_lineage_are_refused() {
    let directory = TempDir::new().expect("directory");
    let daemon = daemon_serving_a_certificate(&directory).await;

    let renew = daemon.run(&["tls", "renew", "--certificate", "edge"]);
    assert_eq!(renew.status.code(), Some(6), "{}", stderr(&renew));
    assert!(
        stderr(&renew).contains("managed_certificate_not_found"),
        "{}",
        stderr(&renew)
    );

    let unconfirmed = daemon.run(&["tls", "revoke", "edge"]);
    assert_eq!(
        unconfirmed.status.code(),
        Some(2),
        "{}",
        stderr(&unconfirmed)
    );
    let revoke = daemon.run(&["tls", "revoke", "edge", "--yes"]);
    assert_eq!(revoke.status.code(), Some(6), "{}", stderr(&revoke));

    let in_use = daemon.run(&["tls", "delete", "edge", "--yes"]);
    assert_eq!(in_use.status.code(), Some(7), "{}", stderr(&in_use));
    assert!(
        stderr(&in_use).contains("certificate_in_use"),
        "{}",
        stderr(&in_use)
    );

    let pause = daemon.run(&["tls", "acme", "pause", "--certificate", "edge"]);
    assert_eq!(pause.status.code(), Some(6), "{}", stderr(&pause));
}
