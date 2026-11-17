use std::fs;

use serde_json::json;
use tokio::net::TcpStream;

use crate::http_support::http_request;
use crate::process_support::{ServerProcess, reserve_tcp_address};
use crate::shared::{
    TOKEN, begin_persistent_request, finish_persistent_request, management_config,
    persistent_request,
};

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn authenticated_local_shutdown_uses_the_graceful_process_channel() {
    let management_address = reserve_tcp_address();
    let config = management_config(management_address, None);
    let mut server = ServerProcess::start(
        &config.clone().validate().expect("valid process config"),
        Some(TOKEN),
    );
    server.wait_for_tcp(management_address).await;
    let authorization = format!("Bearer {TOKEN}");
    let mut management = TcpStream::connect(management_address)
        .await
        .expect("persistent management connection");
    let active_revision = persistent_request(
        &mut management,
        "GET",
        "/api/v1/generations",
        &[("Authorization", &authorization)],
        &[],
    )
    .await
    .json()["generation"]["activeRevision"]
        .as_str()
        .expect("active revision")
        .to_owned();
    let config_snapshot = persistent_request(
        &mut management,
        "GET",
        "/api/v1/config",
        &[("Authorization", &authorization)],
        &[],
    )
    .await
    .json();
    let disk_revision = config_snapshot["diskRevision"]
        .as_str()
        .expect("disk revision")
        .to_owned();
    let disk_before_shutdown = fs::read(&server.config_path).expect("canonical config");
    let mut changed_config = config.clone();
    changed_config.max_connections = Some(777);
    let config_body = serde_json::to_vec(&json!({ "config": changed_config })).unwrap();
    let mutation_body = serde_json::to_vec(&json!({
        "expectedActiveRevision": active_revision,
    }))
    .unwrap();

    let get = http_request(
        management_address,
        "GET",
        "/api/v1/process/shutdown",
        &[("Authorization", &authorization)],
        &[],
    )
    .await;
    assert_eq!(get.status, 405);
    let unauthorized = http_request(
        management_address,
        "POST",
        "/api/v1/process/shutdown",
        &[("Content-Type", "application/json")],
        &mutation_body,
    )
    .await;
    assert_eq!(unauthorized.status, 401);
    let drain = http_request(
        management_address,
        "POST",
        "/api/v1/process/drain",
        &[
            ("Authorization", &authorization),
            ("Content-Type", "application/json"),
        ],
        &mutation_body,
    )
    .await;
    assert_eq!(drain.status, 202);
    let ready = http_request(management_address, "GET", "/ready", &[], &[]).await;
    assert_eq!(ready.status, 503);
    assert_eq!(ready.json()["ready"], false);
    // Shutdown ends idle HTTP/1 connections, so the mutations are in flight (head sent, body
    // withheld) when it is requested and finish afterwards.
    let json_headers = [
        ("Authorization", authorization.as_str()),
        ("Content-Type", "application/json"),
    ];
    let put_headers = [
        json_headers[0],
        json_headers[1],
        ("If-Config-Revision", disk_revision.as_str()),
    ];
    let mut validation = TcpStream::connect(management_address)
        .await
        .expect("validation connection");
    begin_persistent_request(
        &mut validation,
        "POST",
        "/api/v1/config/validate",
        &json_headers,
        config_body.len(),
    )
    .await;
    let mut write = TcpStream::connect(management_address)
        .await
        .expect("write connection");
    begin_persistent_request(
        &mut write,
        "PUT",
        "/api/v1/config",
        &put_headers,
        config_body.len(),
    )
    .await;
    let shutdown = persistent_request(
        &mut management,
        "POST",
        "/api/v1/process/shutdown",
        &json_headers,
        &mutation_body,
    )
    .await;
    assert_eq!(shutdown.status, 202);
    assert_eq!(shutdown.json()["outcome"], "shutdown_requested");

    let validated = finish_persistent_request(&mut validation, &config_body).await;
    assert_eq!(validated.status, 200, "{}", validated.text());
    let rejected = finish_persistent_request(&mut write, &config_body).await;
    assert_eq!(rejected.status, 409, "{}", rejected.text());
    assert_eq!(rejected.json()["error"]["code"], "mutation_in_progress");
    assert_eq!(
        fs::read(&server.config_path).expect("canonical config after rejected PUT"),
        disk_before_shutdown
    );

    drop((management, validation, write));
    tokio::task::spawn_blocking(move || server.wait_for_exit())
        .await
        .expect("wait task");
}
