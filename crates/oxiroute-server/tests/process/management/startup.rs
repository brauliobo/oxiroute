use std::{
    fs,
    time::{Duration, Instant},
};

use tempfile::TempDir;

use crate::config_support::{empty_config, socket_bind};
use crate::process_support::{
    ServerProcess, output_text, reserve_tcp_address, run_to_failure, write_config, write_token,
};
use crate::shared::{TOKEN, management_config, recording_candidate, rtmp_listener_config};

#[test]
fn sigterm_during_runtime_startup_is_bounded() {
    let ui = TempDir::new().expect("startup UI directory");
    fs::write(ui.path().join("index.html"), "<!doctype html>").expect("index");
    fs::create_dir(ui.path().join("assets")).expect("assets");
    for index in 0..256 {
        fs::write(ui.path().join("assets").join(format!("{index}.js")), b"x").expect("asset");
    }
    let config = management_config(reserve_tcp_address(), Some(ui.path().to_path_buf()));
    let server = ServerProcess::start(
        &config.clone().validate().expect("valid process config"),
        Some(TOKEN),
    );
    std::thread::sleep(Duration::from_millis(10));
    let started = Instant::now();

    server.shutdown();

    assert!(started.elapsed() < Duration::from_secs(6));
}

#[test]
fn built_process_rejects_invalid_token_config_and_recording_roots() {
    let token_case = TempDir::new().expect("invalid token case");
    let token_config_path = token_case.path().join("oxiroute.lua");
    let token_config = management_config(reserve_tcp_address(), None);
    write_config(
        &token_config_path,
        &token_config.validate().expect("valid token config"),
    );
    let token_path = write_token(token_case.path(), TOKEN, 0o644);
    let token_failure = run_to_failure(&token_config_path, Some(&token_path));
    assert!(!token_failure.status.success());
    let token_output = output_text(&token_failure);
    assert!(
        token_output.contains("candidate management token preparation failed"),
        "unexpected token failure: {token_output}"
    );
    assert!(!token_output.contains(TOKEN));
    assert!(!token_output.contains(&token_path.display().to_string()));

    let config_case = TempDir::new().expect("invalid config case");
    let invalid_config_path = config_case.path().join("invalid.lua");
    fs::write(&invalid_config_path, "return {").unwrap();
    let config_failure = run_to_failure(&invalid_config_path, None);
    assert!(!config_failure.status.success());
    let config_output = output_text(&config_failure);
    assert!(config_output.contains("canonical configuration was rejected"));

    let recording_case = TempDir::new().expect("invalid recording case");
    let recording_config_path = recording_case.path().join("oxiroute.lua");
    let secret_root = recording_case.path().join("missing-secret-recording-root");
    let recording_config = recording_candidate(&empty_config(), &secret_root);
    write_config(
        &recording_config_path,
        &recording_config.validate().expect("valid recording config"),
    );
    let recording_failure = run_to_failure(&recording_config_path, None);
    assert!(!recording_failure.status.success());
    let recording_output = output_text(&recording_failure);
    assert!(recording_output.contains("candidate runtime preparation failed"));
    assert!(!recording_output.contains(&secret_root.display().to_string()));
}

#[test]
fn built_process_fails_before_runtime_when_a_tcp_listener_cannot_bind() {
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("occupied listener");
    let address = occupied.local_addr().expect("occupied address");
    let directory = TempDir::new().expect("bind failure case");
    let config_path = directory.path().join("oxiroute.lua");
    write_config(
        &config_path,
        &rtmp_listener_config(socket_bind(address))
            .validate()
            .expect("valid RTMP listener config"),
    );

    let failure = run_to_failure(&config_path, None);

    assert!(!failure.status.success());
    let output = output_text(&failure);
    assert!(
        output.contains("could not bind socket"),
        "unexpected listener failure: {output}"
    );
}

#[test]
fn built_direct_process_reports_the_tcp_bind_cause() {
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("occupied listener");
    let address = occupied.local_addr().expect("occupied address");
    let directory = TempDir::new().expect("direct bind failure case");
    let config_path = directory.path().join("oxiroute.lua");
    write_config(
        &config_path,
        &rtmp_listener_config(socket_bind(address))
            .validate()
            .expect("valid RTMP listener config"),
    );
    let token_path = write_token(directory.path(), TOKEN, 0o600);

    let failure = run_to_failure(&config_path, Some(&token_path));

    assert!(!failure.status.success());
    let output = output_text(&failure);
    assert!(output.contains("could not bind socket"), "{output}");
    assert!(output.contains(&address.to_string()), "{output}");
}
