//! Settings that used to be reachable only through environment variables are read from the
//! configuration: the built process starts with none of them in its environment.
#![allow(dead_code)]

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/http.rs"]
mod http_support;
#[path = "support/process.rs"]
mod process_support;

use oxiroute_config::{Audit, ConfigDraft, Management, ValidatedConfig};
use tempfile::TempDir;

use config_support::empty_config;
use http_support::http_request;
use process_support::{ServerProcess, reserve_tcp_address, write_token};

const TOKEN: &str = "cdb85a91948758cfcb895216a3603c8fcd8aaf691f39f5fd82b5df15af14628e";

fn management_config(token_directory: &TempDir) -> (std::net::SocketAddr, ConfigDraft) {
    let management = reserve_tcp_address();
    let token_file = write_token(token_directory.path(), TOKEN, 0o600);
    let config = ConfigDraft {
        management: Some(Management {
            bind: management,
            ui_dir: None,
            token_file: Some(token_file),
        }),
        ..empty_config()
    };
    (management, config)
}

fn validated(config: ConfigDraft) -> ValidatedConfig {
    config.validate().expect("valid process config")
}

#[tokio::test]
async fn management_token_file_comes_from_the_configuration() {
    let token_directory = TempDir::new().expect("token directory");
    let (management, config) = management_config(&token_directory);
    let mut server = ServerProcess::start(&validated(config), None);
    server.wait_for_tcp(management).await;

    let authorization = format!("Bearer {TOKEN}");
    let status = http_request(
        management,
        "GET",
        "/api/v1/status",
        &[("Authorization", &authorization)],
        &[],
    )
    .await;
    let anonymous = http_request(management, "GET", "/api/v1/status", &[], &[]).await;

    assert_eq!(status.status, 200);
    assert_eq!(anonymous.status, 401);
    server.shutdown();
}

#[tokio::test]
async fn audit_location_and_limits_come_from_the_configuration() {
    let token_directory = TempDir::new().expect("token directory");
    let audit_directory = TempDir::new().expect("audit directory");
    let (management, mut config) = management_config(&token_directory);
    config.audit = Some(Audit {
        dir: Some(audit_directory.path().join("log")),
        max_records: Some(123),
        max_record_bytes: Some(4_096),
        max_file_bytes: Some(65_536),
        max_total_bytes: Some(262_144),
        max_rotated_files: Some(2),
    });
    let mut server = ServerProcess::start(&validated(config), None);
    server.wait_for_tcp(management).await;

    let authorization = format!("Bearer {TOKEN}");
    let status = http_request(
        management,
        "GET",
        "/api/v1/audit/status",
        &[("Authorization", &authorization)],
        &[],
    )
    .await;

    assert_eq!(status.status, 200);
    let status = &status.json()["audit"];
    assert_eq!(status["persistent"], true);
    assert_eq!(status["maxRecords"], 123);
    assert_eq!(status["maxRecordBytes"], 4_096);
    assert_eq!(status["maxFileBytes"], 65_536);
    assert_eq!(status["maxTotalBytes"], 262_144);
    assert_eq!(status["maxRotatedFiles"], 2);
    assert!(audit_directory.path().join("log").is_dir());
    server.shutdown();
}
