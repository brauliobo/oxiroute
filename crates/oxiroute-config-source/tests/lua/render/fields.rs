use crate::fixtures::{complete_config, normalized_round_trip};
use crate::lua_support::{load_lua, render_lua};

const RENDERED_FIELDS: &[&str] = &[
    "version",
    "max_connections",
    "management",
    "certificates",
    "tls_profiles",
    "listeners",
    "cache_stores",
    "upstream_pools",
    "http_services",
    "forward_proxy_services",
    "rtmp_services",
    "l4_services",
    "bind",
    "address",
    "ui_dir",
    "name",
    "dns_names",
    "source",
    "type",
    "certificate_chain_path",
    "private_key_path",
    "live_directory_path",
    "archive_directory_path",
    "default_certificate",
    "min_version",
    "alpn",
    "policy",
    "cipher_list",
    "dh_parameters_path",
    "client_auth",
    "mode",
    "allowed_dns_names",
    "session_cache",
    "size_bytes",
    "session_timeout_seconds",
    "session_tickets",
    "prefer_server_ciphers",
    "backend",
    "ktls",
    "protocol",
    "service",
    "tls_profile",
    "max_connections",
    "servers",
    "algorithm",
    "health_check",
    "passive_health",
    "resilience",
    "fail_open",
    "min_members",
    "ejection_jitter_percent",
    "queue_while_unavailable_ms",
    "unavailable_retry_after_seconds",
    "unavailable_body",
    "observe",
    "on_error",
    "error_limit",
    "mark_down",
    "mark_up",
    "initial_backoff_ms",
    "max_backoff_ms",
    "recovery_threshold",
    "circuit_breaker",
    "window_ms",
    "min_requests",
    "failure_ratio_percent",
    "open_ms",
    "half_open_max",
    "half_open_successes",
    "readmission",
    "successes",
    "probe",
    "expected_status",
    "tls",
    "http_versions",
    "server_name",
    "ca_certificate_path",
    "min",
    "max",
    "interval_ms",
    "timeout_ms",
    "healthy_threshold",
    "unhealthy_threshold",
    "host",
    "path",
    "port",
    "routes",
    "automatic_response_headers",
    "upstream_io_timeout_ms",
    "max_request_body_bytes",
    "max_retries",
    "kind",
    "value",
    "access_policy",
    "token_file_path",
    "header_name",
    "realm",
    "action",
    "policy",
    "upstream_host",
    "upstream_path_rewrite",
    "unix_fallback",
    "request_headers",
    "response_headers",
    "response_cookie_path_rewrites",
    "operation",
    "from",
    "to",
    "retry",
    "triggers",
    "method_safety",
    "body_safety",
    "final_redispatch",
    "status",
    "body",
    "headers",
    "location",
    "index_files",
    "spa_fallback",
    "recorders",
    "start",
    "root_directory",
    "suffix_template",
    "append_unix_seconds",
    "rotation_interval_ms",
    "max_queue_messages",
    "max_queue_bytes",
    "shutdown_timeout_ms",
    "max_storage_bytes",
    "max_storage_files",
    "max_active_recorders",
    "methods",
    "upstream_pool",
    "connect_timeout_ms",
    "idle_timeout_ms",
    "lifetime_timeout_ms",
    "udp",
    "max_datagram_bytes",
    "max_sessions",
    "max_session_bytes",
    "max_queue_datagrams",
    "max_queue_bytes",
];

#[test]
fn exhaustively_round_trips_every_current_type_field_and_variant() {
    let (normalized, source) = normalized_round_trip(complete_config());

    assert!(source.contains("max_connections = null,"));
    assert!(source.contains("max_request_body_bytes = null,"));

    for field in RENDERED_FIELDS {
        assert!(
            source.contains(&format!("{field} =")),
            "renderer omitted `{field}`"
        );
    }

    assert_eq!(render_lua(&normalized).expect("second render"), source);
    assert_eq!(
        render_lua(&load_lua(&source).expect("reload")).expect("render after reload"),
        source
    );
}

#[test]
fn serializes_the_complete_canonical_model_for_typed_ui_data() {
    let (config, _) = normalized_round_trip(complete_config());
    let value = serde_json::to_value(config).expect("Config JSON serialization");

    assert_eq!(value["certificates"][0]["source"]["type"], "files");
    assert_eq!(value["certificates"][1]["source"]["type"], "certbot");
    assert_eq!(value["tls_profiles"][0]["min_version"], "1.3");
    assert_eq!(value["tls_profiles"][0]["alpn"][0], "h2");
    assert_eq!(
        value["tls_profiles"][0]["policy"]["client_auth"]["mode"],
        "optional"
    );
    assert_eq!(
        value["tls_profiles"][0]["policy"]["client_auth"]["allowed_dns_names"][0],
        "client.example.test"
    );
    assert_eq!(
        value["tls_profiles"][0]["policy"]["session_cache"]["name"],
        "le_nginx_SSL"
    );
    assert_eq!(
        value["tls_profiles"][0]["policy"]["session_timeout_seconds"],
        86_400
    );
    assert_eq!(value["listeners"][2]["protocol"], "tcp");
    assert_eq!(value["listeners"][0]["bind"]["type"], "socket");
    assert_eq!(value["listeners"][3]["bind"]["type"], "unix");
    assert_eq!(
        value["listeners"][3]["max_connections"],
        serde_json::Value::Null
    );
    assert_eq!(
        value["upstream_pools"][0]["servers"][0]["endpoint"]["type"],
        "socket"
    );
    assert_eq!(
        value["upstream_pools"][0]["servers"][1]["endpoint"]["type"],
        "dns"
    );
    assert_eq!(
        value["upstream_pools"][3]["servers"][0]["endpoint"]["type"],
        "unix"
    );
    assert_eq!(value["upstream_pools"][1]["algorithm"], "least_connections");
    assert_eq!(value["upstream_pools"][0]["http_versions"]["max"], "2");
    assert_eq!(value["upstream_pools"][1]["health_check"]["type"], "http");
    assert_eq!(value["upstream_pools"][2]["health_check"]["type"], "tcp");
    assert_eq!(
        value["http_services"][0]["routes"][0]["host"]["kind"],
        "normalized_host"
    );
    assert_eq!(
        value["http_services"][0]["routes"][0]["action"]["type"],
        "proxy"
    );
    assert_eq!(
        value["http_services"][0]["routes"][1]["action"]["type"],
        "fixed_response"
    );
    assert_eq!(
        value["http_services"][0]["routes"][2]["action"]["type"],
        "redirect"
    );
    assert_eq!(
        value["http_services"][0]["routes"][3]["action"]["type"],
        "static_files"
    );
    assert_eq!(
        value["http_services"][0]["routes"][3]["action"]["etag"],
        false
    );
    assert_eq!(
        value["rtmp_services"][0]["applications"][0]["recorders"][0]["start"],
        "continuous"
    );
    assert_eq!(
        value["rtmp_services"][0]["applications"][0]["recorders"][0]["root_directory"],
        "/var/lib/oxiroute/recordings"
    );
    assert_eq!(
        value["rtmp_services"][0]["applications"][0]["recorders"][1]["start"],
        "manual"
    );
    assert_eq!(
        value["rtmp_services"][0]["applications"][0]["recorders"][1]["rotation_interval_ms"],
        serde_json::Value::Null
    );
    assert_eq!(
        value["http_services"][1]["max_request_body_bytes"],
        serde_json::Value::Null
    );
    assert_eq!(
        value["l4_services"][1]["lifetime_timeout_ms"],
        serde_json::Value::Null
    );
}
