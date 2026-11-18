use std::os::unix::fs::PermissionsExt as _;

use serde_json::json;

use super::{Client, Endpoint, MANAGEMENT_TOKEN_ENV, Response, read_token, resolve_token_path};
use crate::cli::error::{CliError, EXIT_AUTH, EXIT_CONFLICT, EXIT_NOT_FOUND};

#[test]
fn response_parser_preserves_json_fields_and_maps_exit_categories() {
    let response = Response::parse(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 24\r\n\r\n{\"ready\":true,\"value\":7}",
        )
        .expect("response");
    assert_eq!(
        response.json().expect("JSON"),
        json!({ "ready": true, "value": 7 })
    );
    assert_eq!(
        CliError::Status(401, "unauthorized".into()).exit_code(),
        EXIT_AUTH
    );
    assert_eq!(
        CliError::Status(404, "missing".into()).exit_code(),
        EXIT_NOT_FOUND
    );
    assert_eq!(
        CliError::Status(409, "conflict".into()).exit_code(),
        EXIT_CONFLICT
    );
}

#[test]
fn response_errors_surface_redacted_diagnostic_codes() {
    let response = Response {
        status: 422,
        content_type: "application/json".into(),
        body: br#"{"error":{"code":"config_rejected"},"diagnostics":[{"code":"E_NATIVE_SOURCE"}]}"#
            .to_vec(),
    };

    let Err(CliError::Status(status, code)) = response.into_result() else {
        panic!("expected failed response")
    };
    assert_eq!(status, 422);
    assert_eq!(code, "config_rejected: E_NATIVE_SOURCE");
}

#[test]
fn response_parser_supports_interim_and_chunked_framing() {
    let response = Response::parse(
            b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n7\r\n{\"ok\":1\r\n1\r\n}\r\n0\r\nX-Test: yes\r\n\r\n",
        )
        .expect("chunked response");
    assert_eq!(response.json().expect("JSON"), json!({ "ok": 1 }));
}

#[test]
fn response_parser_rejects_conflicting_lengths() {
    let error =
        Response::parse(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nx")
            .expect_err("conflicting lengths");
    assert!(error.to_string().contains("conflicting"));
}

#[test]
fn token_path_resolution_obeys_precedence_and_default_presence() {
    let directory = tempfile::TempDir::new().expect("directory");
    let explicit = directory.path().join("explicit.token");
    let configured = directory.path().join("configured.token");
    let package = directory.path().join("oxiroute.env");
    let package_token = directory.path().join("package.token");
    let default = directory.path().join("management.token");

    assert_eq!(
        resolve_token_path(Some(&explicit), Some(&configured), &package, &default)
            .expect("explicit path"),
        Some(explicit.clone())
    );
    assert_eq!(
        resolve_token_path(None, Some(&configured), &package, &default).expect("configured path"),
        Some(configured.clone())
    );

    std::fs::write(
        &package,
        format!(
            "RUST_LOG=info\n{MANAGEMENT_TOKEN_ENV}={}\n",
            package_token.display()
        ),
    )
    .expect("package environment");
    assert_eq!(
        resolve_token_path(Some(&explicit), Some(&configured), &package, &default)
            .expect("explicit path over package path"),
        Some(explicit.clone())
    );
    assert_eq!(
        resolve_token_path(None, Some(&configured), &package, &default)
            .expect("configured path over package path"),
        Some(configured)
    );
    assert_eq!(
        resolve_token_path(None, None, &package, &default).expect("package path"),
        Some(package_token)
    );

    std::fs::remove_file(&package).expect("remove package environment");
    assert_eq!(
        resolve_token_path(None, None, &package, &default).expect("missing default"),
        None
    );
    std::fs::write(&default, b"placeholder").expect("default token");
    assert_eq!(
        resolve_token_path(None, None, &package, &default).expect("default path"),
        Some(default)
    );
}

#[test]
fn restrictive_token_files_use_os_access_without_an_owner_check() {
    let directory = tempfile::TempDir::new().expect("directory");
    let path = directory.path().join("token");
    let expected = "0123456789abcdefghijklmnopqrstuv";
    std::fs::write(&path, expected).expect("token");
    for mode in [0o400, 0o600] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("mode");
        assert_eq!(read_token(&path).expect("read token").as_str(), expected);
    }
}

#[test]
fn invalid_token_permissions_are_rejected_without_exposing_contents() {
    let directory = tempfile::TempDir::new().expect("directory");
    let path = directory.path().join("token");
    let expected = "0123456789abcdefghijklmnopqrstuv";
    std::fs::write(&path, expected).expect("token");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("mode");
    let error = read_token(&path).expect_err("insecure mode");
    assert!(error.to_string().contains("could not be read securely"));
    assert!(!error.to_string().contains(expected));
}

#[test]
fn authenticated_requests_report_a_missing_token_before_connecting() {
    let client = Client::new(
        Endpoint::parse("http://127.0.0.1:1").expect("endpoint"),
        None,
    );
    let error = client
        .request("GET", "/api/v1/status", &[], &[], true)
        .expect_err("missing token");
    assert_eq!(
        error.to_string(),
        "management token file is missing or unreadable"
    );
}

#[test]
fn token_errors_never_include_token_contents() {
    let directory = tempfile::TempDir::new().expect("directory");
    let path = directory.path().join("token");
    std::fs::write(&path, "secret").expect("token");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("mode");
    let error = read_token(&path).expect_err("short token");
    assert!(!error.to_string().contains("secret"));
}

#[test]
fn unix_endpoints_need_an_absolute_path() {
    assert!(
        Endpoint::parse("unix:/run/oxiroute/read.sock").is_ok_and(|endpoint| endpoint.is_unix())
    );
    assert!(Endpoint::parse("unix:relative.sock").is_err());
}
