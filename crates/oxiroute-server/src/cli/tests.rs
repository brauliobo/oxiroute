use super::*;

#[test]
fn parses_repeated_pool_targets_and_stable_global_options() {
    let cli = Cli::try_parse_from([
        "oxiroute",
        "--endpoint",
        "http://127.0.0.1:9900",
        "--output",
        "json",
        "server",
        "drain",
        "--pool",
        "public-v4",
        "--pool",
        "public-v6",
        "origin-a",
    ])
    .expect("CLI");
    let Command::Server {
        command: ServerCommand::Drain(target),
    } = cli.command()
    else {
        panic!("server drain command")
    };
    assert_eq!(target.pools, ["public-v4", "public-v6"]);
    assert_eq!(target.server, "origin-a");
    assert!(matches!(cli.output, Output::Json));
}

#[test]
fn combines_runtime_offline_and_management_commands() {
    let explicit =
        Cli::try_parse_process_from(["oxiroute", "serve", "edge.lua"]).expect("explicit serve");
    let legacy = Cli::try_parse_process_from(["oxiroute", "edge.lua"]).expect("legacy serve");
    let check = Cli::try_parse_process_from(["oxiroute", "config", "check", "edge.lua"])
        .expect("config check");
    let get = Cli::try_parse_process_from(["oxiroute", "config", "get"]).expect("config get");
    let default = Cli::try_parse_process_from(["oxiroute"]).expect("default serve");

    assert!(matches!(
        explicit.command(),
        Command::Serve { config } if config == &PathBuf::from("edge.lua")
    ));
    assert!(matches!(
        legacy.command(),
        Command::Serve { config } if config == &PathBuf::from("edge.lua")
    ));
    assert!(matches!(
        check.command(),
        Command::Config {
            command: ConfigCommand::Check { config }
        } if config == &PathBuf::from("edge.lua")
    ));
    assert!(matches!(
        get.command(),
        Command::Config {
            command: ConfigCommand::Get
        }
    ));
    assert!(matches!(
        default.command(),
        Command::Serve { config } if config == &PathBuf::from("oxiroute.kdl")
    ));
}

#[test]
fn parses_canonical_config_composition_inputs_in_order() {
    let cli = Cli::try_parse_process_from([
        "oxiroute",
        "config",
        "compose",
        "--format",
        "hocon",
        "nginx.lua",
        "haproxy.lua",
    ])
    .expect("config compose");

    assert!(matches!(
        cli.command(),
        Command::Config {
            command: ConfigCommand::Compose { format, configs }
        } if *format == ComposeFormat::Hocon
            && configs == &[PathBuf::from("nginx.lua"), PathBuf::from("haproxy.lua")]
    ));
}

#[test]
fn parses_explicit_haproxy_preprocessing_environment() {
    let cli = Cli::try_parse_process_from([
        "oxiroute",
        "import",
        "haproxy",
        "/etc/haproxy/haproxy.cfg",
        "--node-ip",
        "192.0.2.37",
        "--gpu1-defined",
        "--shadow-port-offset",
        "10000",
        "--format",
        "hocon",
        "--output",
        "preview",
    ])
    .expect("HAProxy import");

    let Command::Import {
        command:
            ImportCommand::Haproxy {
                configs,
                node_ip,
                gpu1_defined,
                shadow_port_offset,
                format,
                output,
            },
    } = cli.command()
    else {
        panic!("HAProxy import command")
    };
    assert_eq!(configs, &[PathBuf::from("/etc/haproxy/haproxy.cfg")]);
    assert_eq!(*node_ip, Some("192.0.2.37".parse().unwrap()));
    assert!(*gpu1_defined);
    assert_eq!(*shadow_port_offset, Some(10_000));
    assert_eq!(*format, ComposeFormat::Hocon);
    assert!(matches!(output, ImportOutput::Preview));
}

#[test]
fn parses_nginx_timezone_and_shadow_inputs() {
    let cli = Cli::try_parse_process_from([
        "oxiroute",
        "import",
        "nginx",
        "/etc/nginx/nginx.conf",
        "--host-timezone",
        "America/Argentina/Buenos_Aires",
        "--default-access-log-file",
        "/var/lib/oxiroute/http-access.jsonl",
        "--recording-root",
        "/mnt/cloud/4tb/cam-rtmp",
        "--default-error-server",
        "nginx/1.30.2",
        "--shadow-port-offset",
        "10000",
        "--format",
        "lua",
        "--output",
        "preview",
    ])
    .expect("nginx import");

    assert!(matches!(
        cli.command(),
        Command::Import {
            command: ImportCommand::Nginx {
                host_timezone: Some(timezone),
                default_access_log_file: Some(path),
                recording_root: Some(recording_root),
                default_error_server: Some(server),
                shadow_port_offset: Some(10_000),
                format: ComposeFormat::Lua,
                output: ImportOutput::Preview,
                ..
            }
        } if timezone == "America/Argentina/Buenos_Aires"
            && path == Path::new("/var/lib/oxiroute/http-access.jsonl")
            && recording_root == Path::new("/mnt/cloud/4tb/cam-rtmp")
            && server == "nginx/1.30.2"
    ));
}

#[test]
fn haproxy_gpu_presence_requires_a_node_ip() {
    assert!(
        Cli::try_parse_process_from([
            "oxiroute",
            "import",
            "haproxy",
            "/etc/haproxy/haproxy.cfg",
            "--gpu1-defined",
        ])
        .is_err()
    );
}

#[test]
fn haproxy_shadow_preview_shifts_only_listener_ports() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../oxiroute-import/tests/fixtures/live/host-c/haproxy.cfg");
    let output = import_haproxy(
        &[fixture],
        Some("192.0.2.37".parse().unwrap()),
        true,
        Some(10_000),
        ConfigFormat::Hocon,
        ImportOutput::Preview,
    )
    .expect("shadow preview");

    for listener in ["192.0.2.37:20440", "192.0.2.37:22002", "192.0.2.37:18080"] {
        assert!(output.contains(listener), "missing {listener}");
    }
    assert!(output.contains("127.0.0.1:10450"));
    assert!(output.contains("127.0.0.1:10451"));
    assert!(oxiroute_config_source::decode_value(ConfigFormat::Hocon, output.as_bytes()).is_ok());
}

#[test]
fn haproxy_shadow_preview_rejects_port_overflow_and_report_mode() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../oxiroute-import/tests/fixtures/live/host-c/haproxy.cfg");
    let paths = [fixture];
    let node_ip = Some("192.0.2.37".parse().unwrap());

    assert!(
        import_haproxy(
            &paths,
            node_ip,
            true,
            Some(60_000),
            ConfigFormat::Kdl,
            ImportOutput::Preview,
        )
        .is_err()
    );
    assert!(
        import_haproxy(
            &paths,
            node_ip,
            true,
            Some(10_000),
            ConfigFormat::Kdl,
            ImportOutput::Report,
        )
        .is_err()
    );
}

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
    use std::os::unix::fs::PermissionsExt as _;

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
    use std::os::unix::fs::PermissionsExt as _;

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
    let client = Client {
        endpoint: Endpoint::parse("http://127.0.0.1:1").expect("endpoint"),
        token: None,
    };
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
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::TempDir::new().expect("directory");
    let path = directory.path().join("token");
    std::fs::write(&path, "secret").expect("token");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("mode");
    let error = read_token(&path).expect_err("short token");
    assert!(!error.to_string().contains("secret"));
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
fn rejects_empty_batch_targets() {
    assert!(Cli::try_parse_from(["oxiroute", "pool", "drain"]).is_err());
    assert!(Cli::try_parse_from(["oxiroute", "listener", "ready", ""]).is_err());
    assert!(Cli::try_parse_from(["oxiroute", "server", "show", "--pool", "", "origin"]).is_err());
}

#[test]
fn parses_registration_commands_and_socket_endpoints() {
    let cli = Cli::try_parse_from([
        "oxiroute",
        "--socket",
        "/run/oxiroute/register.sock",
        "server",
        "register",
        "--pool",
        "voice",
        "tts-1",
        "--address",
        "192.0.2.10:8000",
        "--ttl",
        "15s",
        "--label",
        "gpu=0",
        "--heartbeat",
    ])
    .expect("CLI");
    let Command::Server {
        command: ServerCommand::Register(args),
    } = cli.command()
    else {
        panic!("server register command")
    };
    assert_eq!(args.ttl, Some(Duration::from_secs(15)));
    assert_eq!(args.labels, [("gpu".to_owned(), "0".to_owned())]);
    assert!(args.heartbeat);
    assert_eq!(
        cli.socket.as_deref(),
        Some(Path::new("/run/oxiroute/register.sock"))
    );

    assert!(
        Endpoint::parse("unix:/run/oxiroute/read.sock").is_ok_and(|endpoint| endpoint.is_unix())
    );
    assert!(Endpoint::parse("unix:relative.sock").is_err());
    assert_eq!(registry::parse_ttl("500ms"), Ok(Duration::from_millis(500)));
    assert_eq!(registry::parse_ttl("2m"), Ok(Duration::from_secs(120)));
    assert!(registry::parse_ttl("soon").is_err());
}

#[test]
fn created_tokens_authenticate_once_and_ids_are_not_reused() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().expect("directory");
    let file = directory.path().join("registry.tokens");
    let pools = ["voice".to_owned(), "render".to_owned()];

    let secret =
        registry::create_token(&file, "agent-a", &pools, Some("gpu-"), false).expect("token");
    let read = registry::create_token(&file, "dash", &[], None, true).expect("read token");

    assert_eq!(
        std::fs::metadata(&file).expect("file").permissions().mode() & 0o777,
        0o640
    );
    let text = std::fs::read_to_string(&file).expect("tokens");
    assert!(!text.contains(&secret), "only the digest is stored");
    let store = crate::registry::auth::TokenStore::open(&file).expect("store");
    assert_eq!(
        store
            .authenticate(secret.as_bytes())
            .map(|principal| principal.scope),
        Some(crate::registry::auth::Scope::Register {
            pools: pools.to_vec(),
            name_prefix: Some("gpu-".into()),
        })
    );
    assert_eq!(
        store
            .authenticate(read.as_bytes())
            .map(|principal| principal.scope),
        Some(crate::registry::auth::Scope::Read)
    );
    assert!(registry::create_token(&file, "agent-a", &pools, None, false).is_err());
    assert!(registry::create_token(&file, "bad id", &pools, None, false).is_err());
}
