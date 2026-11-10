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

use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

use oxiroute_config::{Audit, ConfigDraft, Management, ValidatedConfig};
use serde_json::Value;
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
};

use config_support::empty_config;
use process_support::{ServerProcess, reserve_tcp_address, write_config, write_token};

const TOKEN: &str = "cdb85a91948758cfcb895216a3603c8fcd8aaf691f39f5fd82b5df15af14628e";
const LOGGED_MESSAGE: &str = "configured management API";

fn management_config(token_directory: &TempDir) -> (SocketAddr, ConfigDraft) {
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

fn audit_limit(max_records: u64) -> Option<Audit> {
    Some(Audit {
        max_records: Some(max_records),
        ..Audit::default()
    })
}

/// The status code and JSON body of a management request, or `None` while the socket is between
/// generations or still starting.
async fn management_get(
    management: SocketAddr,
    path: &str,
    authorization: bool,
) -> Option<(u16, Value)> {
    let bearer = if authorization {
        format!("Authorization: Bearer {TOKEN}\r\n")
    } else {
        String::new()
    };
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{bearer}\r\n");
    let mut stream = TcpStream::connect(management).await.ok()?;
    stream.write_all(request.as_bytes()).await.ok()?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.ok()?;
    let text = String::from_utf8_lossy(&response);
    let status = text.split(' ').nth(1)?.parse().ok()?;
    let body = text.split("\r\n\r\n").nth(1).unwrap_or_default();
    Some((status, serde_json::from_str(body).unwrap_or(Value::Null)))
}

async fn eventually<T>(
    management: SocketAddr,
    path: &str,
    authorization: bool,
    wanted: impl Fn(u16, &Value) -> Option<T>,
) -> T {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some((status, body)) = management_get(management, path, authorization).await
            && let Some(value) = wanted(status, &body)
        {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "{path} never reported the expected state"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn audit_status(management: SocketAddr) -> Value {
    eventually(management, "/api/v1/audit/status", true, |status, body| {
        (status == 200).then(|| body["audit"].clone())
    })
    .await
}

async fn wait_for_max_records(management: SocketAddr, expected: u64) {
    eventually(management, "/api/v1/audit/status", true, |_, body| {
        (body["audit"]["maxRecords"] == expected).then_some(())
    })
    .await;
}

#[tokio::test]
async fn management_token_file_comes_from_the_configuration() {
    let token_directory = TempDir::new().expect("token directory");
    let (management, config) = management_config(&token_directory);
    let server = ServerProcess::start(&validated(config), None);

    audit_status(management).await;
    let anonymous = management_get(management, "/api/v1/status", false).await;

    assert_eq!(anonymous.map(|(status, _)| status), Some(401));
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
    let server = ServerProcess::start(&validated(config), None);

    let status = audit_status(management).await;

    assert_eq!(status["persistent"], true);
    assert_eq!(status["maxRecords"], 123);
    assert_eq!(status["maxRecordBytes"], 4_096);
    assert_eq!(status["maxFileBytes"], 65_536);
    assert_eq!(status["maxTotalBytes"], 262_144);
    assert_eq!(status["maxRotatedFiles"], 2);
    assert!(audit_directory.path().join("log").is_dir());
    server.shutdown();
}

#[tokio::test]
async fn log_verbosity_comes_from_the_configuration() {
    let mut stderr = Vec::new();
    for log_filter in [None, Some("info")] {
        let token_directory = TempDir::new().expect("token directory");
        let (management, mut config) = management_config(&token_directory);
        config.log_filter = log_filter.map(Into::into);
        let server = ServerProcess::start(&validated(config), None);
        audit_status(management).await;
        stderr.push(server.shutdown_output());
    }

    assert!(!stderr[0].contains(LOGGED_MESSAGE), "{}", stderr[0]);
    assert!(stderr[1].contains(LOGGED_MESSAGE), "{}", stderr[1]);
}

#[tokio::test]
async fn an_audit_change_takes_effect_with_the_next_reload() {
    let token_directory = TempDir::new().expect("token directory");
    let (management, mut config) = management_config(&token_directory);
    config.audit = audit_limit(123);
    let server = ServerProcess::start(&validated(config.clone()), None);
    wait_for_max_records(management, 123).await;

    config.audit = audit_limit(456);
    write_config(&server.config_path, &validated(config));

    wait_for_max_records(management, 456).await;
    server.shutdown();
}

#[tokio::test]
async fn a_log_filter_change_takes_effect_with_the_next_reload() {
    let token_directory = TempDir::new().expect("token directory");
    let (management, mut config) = management_config(&token_directory);
    config.audit = audit_limit(123);
    let server = ServerProcess::start(&validated(config.clone()), None);
    wait_for_max_records(management, 123).await;

    config.audit = audit_limit(456);
    config.log_filter = Some("info".into());
    write_config(&server.config_path, &validated(config));
    wait_for_max_records(management, 456).await;

    assert!(server.shutdown_output().contains(LOGGED_MESSAGE));
}
