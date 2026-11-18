use std::os::unix::fs::PermissionsExt as _;

use serde_json::json;

use super::{Client, Endpoint, MANAGEMENT_TOKEN_ENV, Package, Response, read_token};
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

struct Layout {
    directory: tempfile::TempDir,
}

impl Layout {
    fn new() -> Self {
        Self {
            directory: tempfile::TempDir::new().expect("directory"),
        }
    }

    fn path(&self, name: &str) -> std::path::PathBuf {
        self.directory.path().join(name)
    }

    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.path(name), text).expect("file");
    }

    fn package(&self) -> Package {
        Package::at(
            self.path("oxiroute.env"),
            vec![self.path("oxiroute.kdl"), self.path("oxiroute.lua")],
            self.path("management.token"),
        )
    }
}

#[test]
fn the_endpoint_comes_from_the_environment_file_then_the_management_bind() {
    let layout = Layout::new();
    assert_eq!(layout.package().discovered_endpoint(), None);

    layout.write(
        "oxiroute.kdl",
        "version 1\n(object)management { bind \"127.0.0.1:9081\" }\n",
    );
    assert_eq!(
        layout.package().discovered_endpoint().as_deref(),
        Some("http://127.0.0.1:9081")
    );

    layout.write(
        "oxiroute.env",
        "RUST_LOG=info\nOXIROUTE_ENDPOINT=http://127.0.0.1:9082\n",
    );
    assert_eq!(
        layout.package().discovered_endpoint().as_deref(),
        Some("http://127.0.0.1:9082")
    );
}

#[test]
fn a_config_without_a_management_block_or_a_broken_one_discovers_nothing() {
    let layout = Layout::new();
    layout.write("oxiroute.kdl", "version 1\n");
    layout.write("oxiroute.lua", "this is not a configuration");
    assert_eq!(layout.package().discovered_endpoint(), None);
}

#[test]
fn token_path_resolution_obeys_precedence_and_default_presence() {
    let layout = Layout::new();
    let explicit = layout.path("explicit.token");
    let package_token = layout.path("package.token");
    let config_token = layout.path("config.token");

    assert_eq!(layout.package().token_path(None).expect("none"), None);
    layout.write("management.token", "placeholder");
    assert_eq!(
        layout.package().token_path(None).expect("default path"),
        Some(layout.path("management.token"))
    );

    layout.write(
        "oxiroute.kdl",
        &format!(
            "version 1\n(object)management {{ bind \"127.0.0.1:9081\"\ntoken_file \"{}\" }}\n",
            config_token.display()
        ),
    );
    assert_eq!(
        layout.package().token_path(None).expect("config path"),
        Some(config_token)
    );

    layout.write(
        "oxiroute.env",
        &format!(
            "RUST_LOG=info\n{MANAGEMENT_TOKEN_ENV}={}\n",
            package_token.display()
        ),
    );
    assert_eq!(
        layout.package().token_path(None).expect("package path"),
        Some(package_token)
    );
    assert_eq!(
        layout
            .package()
            .token_path(Some(&explicit))
            .expect("explicit path"),
        Some(explicit)
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
    let message = read_token(&path).expect_err("insecure mode").to_string();
    assert!(message.contains(&path.display().to_string()), "{message}");
    assert!(message.contains("mode 0644"), "{message}");
    assert!(message.contains("0400 or 0600"), "{message}");
    assert!(!message.contains(expected));
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
    let message = error.to_string();
    assert!(message.contains("no management token file"), "{message}");
    assert!(
        message.contains("OXIROUTE_MANAGEMENT_TOKEN_FILE"),
        "{message}"
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

#[test]
fn token_errors_name_the_path_and_the_requirement() {
    let directory = tempfile::TempDir::new().expect("directory");
    let message = |path: &std::path::Path| read_token(path).expect_err("refused").to_string();
    let requirement =
        "regular file with mode 0400 or 0600 holding 32 to 512 visible ASCII characters";

    let missing = directory.path().join("missing.token");
    let text = message(&missing);
    assert!(text.contains(&missing.display().to_string()), "{text}");
    assert!(text.contains("No such file or directory"), "{text}");
    assert!(text.contains(requirement), "{text}");

    let short = directory.path().join("short.token");
    std::fs::write(&short, "secret").expect("token");
    std::fs::set_permissions(&short, std::fs::Permissions::from_mode(0o600)).expect("mode");
    let text = message(&short);
    assert!(text.contains("token of 6 characters"), "{text}");
    assert!(!text.contains("secret"), "{text}");

    let link = directory.path().join("link.token");
    std::os::unix::fs::symlink(&short, &link).expect("symlink");
    assert!(message(&link).contains("symbolic link"));
    assert!(message(directory.path()).contains("not a regular file"));
}

#[test]
fn authorization_failures_say_what_to_check() {
    let unauthorized = CliError::Status(401, "unauthorized".into()).to_string();
    assert!(unauthorized.contains("--token-file"), "{unauthorized}");
    assert!(
        unauthorized.contains("OXIROUTE_MANAGEMENT_TOKEN_FILE"),
        "{unauthorized}"
    );
    let forbidden = CliError::Status(403, "forbidden".into()).to_string();
    assert!(forbidden.contains("scope"), "{forbidden}");
}

#[test]
fn rejections_list_every_diagnostic_and_fall_back_to_the_message() {
    let rejected = Response {
        status: 422,
        content_type: "application/json".into(),
        body: br#"{"error":{"code":"config_rejected"},"diagnostics":[
            {"code":"E_INVALID_VALUE","path":"/upstream_pools/farm","detail":"duplicate endpoint"},
            {"code":"E_SYNTAX","line":3,"message":"unexpected token"}]}"#
            .to_vec(),
    };
    let Err(CliError::Status(_, text)) = rejected.into_result() else {
        panic!("expected failed response")
    };
    let lines = text.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(
        lines[0].starts_with("config_rejected: E_INVALID_VALUE"),
        "{text}"
    );
    assert!(lines[0].contains("/upstream_pools/farm"), "{text}");
    assert!(
        lines[1].contains("E_SYNTAX") && lines[1].contains("line 3"),
        "{text}"
    );

    let plain = Response {
        status: 400,
        content_type: "application/json".into(),
        body: br#"{"error":{"code":"bad_request","message":"the request body is not JSON"}}"#
            .to_vec(),
    };
    let Err(CliError::Status(_, text)) = plain.into_result() else {
        panic!("expected failed response")
    };
    assert_eq!(text, "bad_request: the request body is not JSON");
}
