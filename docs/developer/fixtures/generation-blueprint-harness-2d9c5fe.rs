use std::{fs, os::unix::fs::PermissionsExt, path::Path};

use oxiroute_config::{
    AccessLogPolicy, Config, HealthCheck, HealthCheckType, HttpRouteAction, PassiveHealthPolicy,
};
use crate::{
    GenerationManager, baseline_acquisition_trace, baseline_reset_acquisition_trace,
    baseline_rtmp_runtime_starts, runtime_plan,
};
use serde_json::{Value, json};
use tempfile::TempDir;

const BEGIN: &str = "OXIROUTE_GENERATION_BASELINE_BEGIN";
const END: &str = "OXIROUTE_GENERATION_BASELINE_END";

fn load(source: &str) -> Config {
    oxiroute_config::load_lua(source).expect("baseline config")
}

fn normalize_decisions(config: &Config, root: &Path) -> Value {
    let mut normalized = config.clone();
    if oxiroute_config::validate_config(&mut normalized).is_err() {
        normalized = config.clone();
    }
    let mut value = serde_json::to_value(normalized).expect("serialized baseline decisions");
    normalize_value(&mut value, &root.display().to_string(), None);
    value
}

fn normalize_value(value: &mut Value, root: &str, parent: Option<&str>) {
    match value {
        Value::String(text) => {
            *text = text.replace(root, "<fixture-root>");
            if matches!(parent, Some("secret")) {
                *text = "<redacted>".into();
            }
        }
        Value::Array(values) => {
            for value in values {
                normalize_value(value, root, parent);
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                normalize_value(value, root, Some(key));
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn observe(case: &str, config: &Config, root: &Path) -> Value {
    baseline_reset_acquisition_trace();
    match runtime_plan(config) {
        Ok(plan) => json!({
            "case": case,
            "result": "ok",
            "decisions": normalize_decisions(config, root),
            "acquisitionOrder": baseline_acquisition_trace(),
            "services": plan.services.iter().map(|service| json!({
                "name": service.name,
                "protocol": service.kind.protocol(),
                "tls": service.tls.as_ref().map(|tls| json!({
                    "name": tls.name(),
                    "minVersion": format!("{:?}", tls.min_version()),
                    "alpn": format!("{:?}", tls.alpn()),
                    "defaultCertificate": tls.default_certificate(),
                    "clientAuthMode": format!("{:?}", tls.client_auth_mode()),
                    "clientAuthCaConfigured": tls.client_auth_ca_configured(),
                    "clientAuthAllowedDnsNames": tls.client_auth_allowed_dns_name_count(),
                })),
            })).collect::<Vec<_>>(),
            "pools": plan.pools.iter().map(|pool| {
                let snapshot = pool.health_snapshot();
                json!({
                    "name": snapshot.name,
                    "algorithm": snapshot.algorithm,
                    "endpoints": snapshot.endpoints.iter().map(|endpoint| json!({
                        "name": endpoint.name,
                        "address": endpoint.address.to_string(),
                        "weight": endpoint.weight,
                        "maxConnections": endpoint.max_connections,
                    })).collect::<Vec<_>>(),
                })
            }).collect::<Vec<_>>(),
            "healthSupervisor": plan.health_supervisor.is_some(),
            "rtmp": {
                "live": plan.rtmp_capabilities.live_ingest,
                "manual": plan.rtmp_capabilities.manual_recording,
                "recording": plan.rtmp_recording_supported,
            },
            "topology": {
                "nodes": plan.topology.nodes(),
                "edges": plan.topology.edges(),
            },
        }),
        Err(error) => json!({
            "case": case,
            "result": "error",
            "decisions": normalize_decisions(config, root),
            "error": error.to_string(),
            "acquisitionOrder": baseline_acquisition_trace(),
        }),
    }
}

fn document_for(config: &Config) -> crate::config_coordinator::CanonicalConfigDocument {
    use crate::config_coordinator::{CanonicalConfigCoordinator, ConfigLoadOutcome};

    let directory = TempDir::new().expect("validation document directory");
    let path = directory.path().join("oxiroute.lua");
    fs::write(&path, oxiroute_config::render_lua(config).expect("validation render"))
        .expect("validation document");
    let ConfigLoadOutcome::Loaded(document) = CanonicalConfigCoordinator::new(path)
        .expect("validation coordinator")
        .load()
    else {
        panic!("validation document load")
    };
    *document
}

fn observe_validation(case: &str, config: Config) -> Value {
    baseline_reset_acquisition_trace();
    let result = GenerationManager::new().validate_candidate(document_for(&config));
    json!({
        "case": case,
        "result": result.map_or_else(|error| error.code(), |()| "ok"),
        "rtmpRuntimeStarts": baseline_rtmp_runtime_starts(),
    })
}

fn validation_cases() -> Vec<Value> {
    ["access-log", "media", "recording"]
        .into_iter()
        .map(|case| {
            let root = TempDir::new().expect("validation fixture root");
            let mut config = comprehensive(root.path());
            match case {
                "access-log" => {
                    config.rtmp_services[0].access_log = Some(AccessLogPolicy::File {
                        path: root.path().join("missing/access.log"),
                    });
                }
                "media" => {
                    let media = root.path().join("media-file");
                    fs::write(&media, b"not a directory").expect("media fixture file");
                    config.rtmp_services[0].applications[0]
                        .hls
                        .as_mut()
                        .expect("HLS policy")
                        .root_directory = media;
                }
                "recording" => {
                    let recording = root.path().join("recording-file");
                    fs::write(&recording, b"not a directory").expect("recording fixture file");
                    config.rtmp_services[0].applications[0].recorders[0].root_directory = recording;
                }
                _ => unreachable!(),
            }
            observe_validation(case, config)
        })
        .collect()
}

fn comprehensive(root: &Path) -> Config {
    let static_root = root.join("static");
    fs::create_dir(&static_root).expect("static root");
    fs::write(static_root.join("index.html"), b"baseline").expect("static file");
    let token = root.join("forward.token");
    fs::write(&token, b"0123456789abcdef0123456789abcdef").expect("forward token");
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).expect("token mode");
    let auto_push_secret = root.join("auto-push.secret");
    fs::write(&auto_push_secret, b"0123456789abcdef0123456789abcdef").expect("auto-push secret");
    fs::set_permissions(&auto_push_secret, fs::Permissions::from_mode(0o600)).expect("auto-push secret mode");
    let auto_push_root = root.join("auto-push");
    let vod_root = root.join("vod");
    let hls_root = root.join("hls");
    let dash_root = root.join("dash");
    let recording_root = root.join("recording");
    let exec_root = root.join("exec");
    for directory in [&auto_push_root, &vod_root, &hls_root, &dash_root, &recording_root, &exec_root] {
        fs::create_dir(directory).expect("runtime fixture directory");
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).expect("runtime fixture directory mode");
    }
    let mut config = load(&format!(r#"
return {{
  version = 1,
  certificates = {{
    {{ name = "h2-cert", dns_names = {{"h2.example.test"}}, source = {{ type = "self_signed_development", validity_days = 7 }} }},
    {{ name = "h3-cert", dns_names = {{"h3.example.test", "*.h3.example.test"}}, source = {{ type = "self_signed_development", validity_days = 7 }} }},
  }},
  tls_profiles = {{
    {{ name = "h2", certificates = {{"h2-cert"}}, default_certificate = "h2-cert", min_version = "1.2", alpn = {{"h2", "http/1.1"}}, policy = {{ cipher_list = "ECDHE-ECDSA-AES128-GCM-SHA256", session_tickets = false, prefer_server_ciphers = true, session_cache = {{ name = "h2", size_bytes = 65536 }}, session_timeout_seconds = 60 }} }},
    {{ name = "h3", certificates = {{"h3-cert"}}, default_certificate = "h3-cert", min_version = "1.3", alpn = {{"h3"}} }},
  }},
  listeners = {{
    {{ name = "web", bind = {{ type = "socket", address = "127.0.0.1:18080" }}, protocol = "http", service = "web" }},
    {{ name = "gzip", bind = {{ type = "socket", address = "127.0.0.1:18084" }}, protocol = "http", service = "gzip" }},
    {{ name = "reverse-h3", bind = {{ type = "udp", address = "127.0.0.1:18443" }}, protocol = "http3", service = "web-h3", tls_profile = "h3" }},
    {{ name = "forward-h1", bind = {{ type = "socket", address = "127.0.0.1:18081" }}, protocol = "forward_http1", service = "forward" }},
    {{ name = "forward-h2", bind = {{ type = "socket", address = "127.0.0.1:18082" }}, protocol = "forward_http2", service = "forward", tls_profile = "h2" }},
    {{ name = "forward-h3", bind = {{ type = "udp", address = "127.0.0.1:18083" }}, protocol = "forward_http3", service = "forward", tls_profile = "h3" }},
    {{ name = "tcp", bind = {{ type = "socket", address = "127.0.0.1:15432" }}, protocol = "tcp", service = "tcp" }},
    {{ name = "udp", bind = {{ type = "udp", address = "127.0.0.1:15353" }}, protocol = "udp", service = "udp" }},
    {{ name = "rtmp", bind = {{ type = "socket", address = "127.0.0.1:11935" }}, protocol = "rtmp", service = "rtmp" }},
  }},
  cache_stores = {{{{ name = "memory", type = "memory", max_bytes = 1048576, max_entries = 128, max_object_bytes = 65536 }}}},
  upstream_pools = {{
    {{ name = "origin", endpoints = {{{{ type = "socket", address = "127.0.0.1:13000" }}, {{ type = "socket", address = "127.0.0.1:13001" }}}}, algorithm = {{ type = "weighted_round_robin", weights = {{2, 1}} }} }},
    {{ name = "h3-origin", endpoints = {{{{ type = "socket", address = "127.0.0.1:13443" }}}}, http_versions = {{ min = "3", max = "3" }}, tls = {{ server_name = "origin.example.test" }} }},
    {{ name = "tcp", endpoints = {{{{ type = "socket", address = "127.0.0.1:5432" }}}} }},
    {{ name = "udp", endpoints = {{{{ type = "socket", address = "127.0.0.1:5353" }}}} }},
  }},
  http_services = {{{{ name = "web", routes = {{
    {{ path = {{ kind = "exact", value = "/fixed" }}, access_policy = {{ type = "bearer_token_file", token_file_path = {:?}, header_name = "Authorization", realm = "baseline" }}, policy = {{ request_buffering = true }}, action = {{ type = "fixed_response", status = 201, body = "fixed", headers = {{{{ name = "x-baseline", value = "fixed" }}}} }} }},
    {{ path = {{ kind = "exact", value = "/redirect" }}, policy = {{ request_buffering = true }}, action = {{ type = "redirect", status = 308, location = {{ kind = "literal", value = "/fixed" }} }} }},
    {{ path = {{ kind = "segment_prefix", value = "/static" }}, policy = {{ request_buffering = true }}, action = {{ type = "static_files", root_directory = {:?}, index_files = {{"index.html"}} }} }},
    {{ host = {{ kind = "normalized_host", value = "api.example.test" }}, path = {{ kind = "segment_prefix", value = "/" }}, methods = {{"GET", "HEAD"}}, policy = {{ max_request_body_bytes = 65536, connect_timeout_ms = 1000, read_timeout_ms = 2000, write_timeout_ms = 3000, request_buffering = true }}, action = {{ type = "proxy", upstream_pool = "origin", policy = {{ cache = {{ store = "memory", methods = {{"GET", "HEAD"}}, default_ttl_ms = 10000, grace_ms = 2000, keep_ms = 3000, status_ttls = {{{{ status = 404, ttl_ms = 1000 }}}} }} }} }} }},
  }} }}, {{ name = "gzip", gzip = {{ level = 6, content_types = {{"text/plain", "application/json"}}, min_length_bytes = 64, min_http_version = "1.1", disable_on_via = true, vary = true }}, routes = {{{{ path = {{ kind = "segment_prefix", value = "/" }}, policy = {{ request_buffering = true }}, action = {{ type = "fixed_response", status = 200, body = "gzip" }} }}}} }}, {{ name = "web-h3", routes = {{{{ path = {{ kind = "segment_prefix", value = "/" }}, policy = {{ request_buffering = true }}, action = {{ type = "proxy", upstream_pool = "h3-origin", policy = {{}} }} }}}} }}}},
  forward_proxy_services = {{{{ name = "forward", enabled_versions = {{"h1", "h2", "h3"}}, allow_absolute_form = true, tls_required = false, connect = {{ enabled = true, allowed_ports = {{443, 8443}} }}, connect_udp = {{ enabled = true, allowed_ports = {{443}} }}, auth = {{ type = "bearer_token_file", token_file_path = {:?} }}, access_policy = {{ rules = {{{{ action = "allow", conditions = {{{{ type = "source_cidrs", cidrs = {{"192.0.2.0/24"}} }}}} }}}}, default_action = "deny" }}, destination_policy = {{ allow_domains = {{"example.test"}}, deny_domains = {{"blocked.example.test"}}, deny_private = false }}, connect_timeout_ms = 1000, idle_timeout_ms = 2000, lifetime_timeout_ms = 3000, max_request_body_bytes = 65536, max_header_bytes = 16384, max_connections = 32, cache = {{ store = "memory", default_ttl_ms = 5000, grace_ms = 500, keep_ms = 750 }} }}}},
  rtmp_services = {{{{ name = "rtmp", outbound_chunk_size = 4096, max_inbound_message_size = 1048576, ack_window_size = 5000000, outbound_policy = {{ deny_private = false, rtmps = "allowed", max_chain_depth = 4 }}, callbacks = {{ on_connect = "http://127.0.0.1:19090/connect", on_disconnect = "http://127.0.0.1:19090/disconnect", notify_method = "post", timeout_ms = 1200 }}, auto_push = {{ enabled = true, socket_dir = {:?}, secret_file = {:?}, reconnect_ms = 1000, connect_timeout_ms = 500, handshake_timeout_ms = 750, max_peers = 2, max_queue_messages = 16, max_queue_bytes = 65536, max_streams = 4 }}, exec_profiles = {{{{ name = "publisher", application = "live", mode = "command", trigger = "publisher", executable = "/bin/true", arguments = {{"--baseline"}}, environment = {{{{ name = "BASELINE", value = "fixture" }}}}, working_directory = {:?}, filesystem = "working_directory", network = "disabled", timeout_ms = 1000, shutdown_timeout_ms = 500, max_processes = 1, max_queue_messages = 8, max_queue_bytes = 4096, max_stdout_bytes = 1024, max_stderr_bytes = 1024 }}}}, applications = {{{{ name = "live", live = true, idle_streams = false, publish = {{ rules = {{{{ action = "allow", network = "192.0.2.0/24" }}}} }}, play = {{ rules = {{{{ action = "deny", network = "198.51.100.0/24" }}}} }}, limits = {{ max_connections = 100, max_publishers = 5, max_viewers = 95 }}, fanout = {{ max_subscribers = 32, max_queue_messages_per_subscriber = 64, max_queue_bytes_per_subscriber = 262144 }}, push_targets = {{{{ host = "127.0.0.1", port = 1936, application = "$name", stream_name = "mirror", scheme = "rtmp" }}}}, pull_targets = {{{{ host = "127.0.0.1", port = 1937, application = "origin", stream_name = "source", scheme = "rtmp" }}}}, relay = {{ max_queue_messages = 32, max_queue_bytes = 131072, buffer_ms = 1000, push_reconnect_ms = 2000, pull_reconnect_ms = 3000, dns_refresh_ms = 5000, connect_timeout_ms = 1000, handshake_timeout_ms = 1500 }}, callbacks = {{ on_publish = "http://127.0.0.1:19090/publish", on_publish_done = "http://127.0.0.1:19090/publish-done", on_play = "http://127.0.0.1:19090/play", on_play_done = "http://127.0.0.1:19090/play-done", on_done = "http://127.0.0.1:19090/done", on_update = "http://127.0.0.1:19090/update", notify_method = "get", timeout_ms = 1300, notify_update_timeout_ms = 1400, notify_update_strict = true, notify_relay_redirect = true }}, vod = {{ max_sessions = 2, max_file_bytes = 1048576, max_duration_ms = 60000, sources = {{{{ type = "local", name = "archive", root_directory = {:?} }}, {{ type = "http", name = "origin", origin = "http://127.0.0.1:19091/media" }}}} }}, hls = {{ root_directory = {:?}, segment_duration_ms = 2000, max_segment_duration_ms = 10000, playlist_length_ms = 30000 }}, dash = {{ root_directory = {:?}, segment_duration_ms = 5000, max_segment_duration_ms = 15000, playlist_length_ms = 30000 }}, recorders = {{{{ name = "archive", start = "manual", root_directory = {:?}, record_mask = {{ audio = true, video = true, keyframes = false }}, suffix_template = "-%Y%m%d.flv", notify = true, max_queue_messages = 32, max_queue_bytes = 1048576, shutdown_timeout_ms = 1000, max_active_recorders = 2 }}}} }}}} }}}},
  l4_services = {{
    {{ name = "tcp", upstream_pool = "tcp", connect_timeout_ms = 1000, idle_timeout_ms = 2000 }},
    {{ name = "udp", upstream_pool = "udp", connect_timeout_ms = 1000, idle_timeout_ms = 2000, udp = {{ max_datagram_bytes = 1232, max_sessions = 32, max_session_bytes = 65536, max_queue_datagrams = 8, max_queue_bytes = 16384 }} }},
  }},
}}
"#, token, static_root, token, auto_push_root, auto_push_secret, exec_root, vod_root, hls_root, dash_root, recording_root));
    config.upstream_pools[0].health_check = Some(HealthCheck {
        kind: HealthCheckType::Tcp,
        interval_ms: 5_000,
        timeout_ms: 1_000,
        healthy_threshold: 1,
        unhealthy_threshold: 2,
        startup: oxiroute_config::HealthStartup::default(),
        fast_interval_ms: None,
        down_interval_ms: None,
        host: None,
        path: None,
        expected_status: None,
        http_version: None,
    });
    config.upstream_pools[0].passive_health = Some(PassiveHealthPolicy {
        error_limit: 2,
        initial_backoff_ms: 30_000,
        max_backoff_ms: 30_000,
        ..PassiveHealthPolicy::default()
    });
    config
}

#[test]
fn emit_authenticated_generation_baseline() {
    let root = TempDir::new().expect("baseline root");
    let comprehensive = comprehensive(root.path());
    let mut unsupported = comprehensive.clone();
    let HttpRouteAction::Proxy { .. } = unsupported.http_services[0].routes[3].action else { panic!("proxy route") };
    unsupported.http_services[0].routes[3].policy.request_buffering = true;
    unsupported.http_services[0].routes[3].policy.max_request_body_bytes = None;

    let output = json!({
        "schemaVersion": 2,
        "sourceCommit": "2d9c5fe66cd096d7a1d8e3bada8d5784b5f97f6c",
        "coverage": ["normalized_validated_decisions", "runtime_services_tls", "runtime_pool_endpoints", "exact_topology", "errors", "acquisition_trace_stop_points", "generation_validation_environmental_failures"],
        "cases": [
            observe("comprehensive", &comprehensive, root.path()),
            observe("unsupported_before_tls", &unsupported, root.path()),
        ],
        "validationCases": validation_cases(),
    });
    println!("{BEGIN}");
    println!("{}", serde_json::to_string_pretty(&output).expect("baseline JSON"));
    println!("{END}");
}
