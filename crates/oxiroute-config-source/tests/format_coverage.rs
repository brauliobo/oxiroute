use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use oxiroute_config::{
    CertificateSource, ConfigDraft, HttpHostSelector, HttpPathSelector, HttpRouteAction,
    UpstreamAlgorithm, ValidatedConfig,
};
use oxiroute_config_source::{
    ConfigFormat, ConfigSourceError, MAX_SOURCE_BYTES, decode_value, render_config, resolve_source,
    resolve_source_with_format,
};
use serde_json::Value;
use tempfile::tempdir;

const ALL_FORMATS: [ConfigFormat; 7] = [
    ConfigFormat::Kdl,
    ConfigFormat::Lua,
    ConfigFormat::Uci,
    ConfigFormat::Hocon,
    ConfigFormat::Yaml,
    ConfigFormat::Toml,
    ConfigFormat::Oxi,
];

fn write_file(path: &Path, contents: impl AsRef<[u8]>) -> PathBuf {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directories");
    }
    fs::write(path, contents).expect("write fixture file");
    fs::canonicalize(path).expect("canonicalize fixture file")
}

/// Encodes a JSON tree into deterministic `OpenWrt` UCI `config json` records.
fn encode_uci_json(value: &Value) -> String {
    fn quote(text: &str) -> String {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
    fn emit(
        value: &Value,
        name: &str,
        parent: Option<&str>,
        rel: Option<(&str, String)>,
        counter: &mut usize,
        out: &mut String,
    ) {
        if !out.is_empty() {
            out.push('\n');
        }
        let _ = writeln!(out, "config json {}", quote(name));
        if let Some(p) = parent {
            let _ = writeln!(out, "\toption parent {}", quote(p));
        }
        if let Some((field, val)) = rel {
            let _ = writeln!(out, "\toption {field} {}", quote(&val));
        }
        let (kind, scalar) = match value {
            Value::Object(_) => ("object", None),
            Value::Array(_) => ("array", None),
            Value::String(s) => ("string", Some(s.clone())),
            Value::Number(n) => ("number", Some(n.to_string())),
            Value::Bool(b) => ("bool", Some(b.to_string())),
            Value::Null => ("null", None),
        };
        let _ = writeln!(out, "\toption kind {}", quote(kind));
        if let Some(val) = scalar {
            let _ = writeln!(out, "\toption value {}", quote(&val));
        }
        match value {
            Value::Object(map) => {
                let mut entries: Vec<_> = map.iter().collect();
                entries.sort_unstable_by_key(|(k, _)| *k);
                for (key, child) in entries {
                    *counter += 1;
                    let child_name = format!("n{counter:04}");
                    emit(
                        child,
                        &child_name,
                        Some(name),
                        Some(("key", key.clone())),
                        counter,
                        out,
                    );
                }
            }
            Value::Array(items) => {
                for (idx, child) in items.iter().enumerate() {
                    *counter += 1;
                    let child_name = format!("n{counter:04}");
                    emit(
                        child,
                        &child_name,
                        Some(name),
                        Some(("index", idx.to_string())),
                        counter,
                        out,
                    );
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    let mut out = String::new();
    let mut counter = 0usize;
    emit(value, "root", None, None, &mut counter, &mut out);
    out
}

const KITCHEN_SINK_JSON: &str = r#"{
  "version": 1, "max_connections": 8192, "listener_workers": 4,
  "http_fast_lane": false, "http_data_plane": "tokio",
  "connection_scheduling": "aged_per_worker", "connection_age_ms": 25,
  "worker_scaling": "dynamic", "min_active_workers": 2, "target_busy_percent": 60, "scale_down_delay_ms": 3000,
  "tls_handshake_threads": 2, "http3_congestion_control": "cubic", "http3_segmentation_offload": false,
  "retirement": { "drain_deadline_ms": 60000, "max_retired_generations": 3 },
  "log_filter": "info,oxiroute_server=debug", "cpu_level": "scalar",
  "management": { "bind": "127.0.0.1:9090", "ui_dir": "/usr/share/oxiroute/ui", "token_file": "/etc/oxiroute/mgmt.token" },
  "audit": { "dir": "/var/log/oxiroute/audit", "max_records": 5000, "max_record_bytes": 8192, "max_file_bytes": 524288, "max_total_bytes": 4194304, "max_rotated_files": 4 },
  "config_history": { "max_entries": 15, "max_total_bytes": 8388608 },
  "client_tracking": { "enabled": true, "max_clients": 4096, "ipv6_prefix": 64 },
  "registry": {
    "state_dir": "/var/lib/oxiroute/registry", "tokens_file": "/etc/oxiroute/registry.tokens",
    "endpoints": [
      { "type": "unix", "path": "/run/oxiroute/registry.sock", "mode": 432, "role": "register", "pools": ["app-pool"] },
      { "type": "tcp", "bind": "127.0.0.1:9450", "allow_cidrs": ["127.0.0.0/8"] }
    ]
  },
  "stats": {
    "binds": ["127.0.0.1:9101"], "admin_token_file": "/etc/oxiroute/stats.token",
    "pages": [{
      "bind": "127.0.0.1:9102", "uri_prefix": "/stats", "refresh_ms": 2000, "admin": "localhost", "max_connections": 64,
      "downstream_timeouts": { "client_timeout_ms": 15000, "request_timeout_ms": 5000, "keepalive_timeout_ms": 10000 }
    }]
  },
  "certificates": [
    { "name": "files-cert", "dns_names": ["files.example.test"], "source": { "type": "files", "certificate_chain_path": "/etc/oxiroute/files.pem", "private_key_path": "/etc/oxiroute/files.key" } },
    { "name": "certbot-cert", "dns_names": ["certbot.example.test"], "source": { "type": "certbot", "live_directory_path": "/etc/letsencrypt/live/certbot.example.test", "archive_directory_path": "/etc/letsencrypt/archive/certbot.example.test" } },
    {
      "name": "acme-cert", "dns_names": ["*.acme.example.test"],
      "source": {
        "type": "acme_managed", "directory_url": "https://acme-v02.api.letsencrypt.org/directory", "state_root": "/var/lib/oxiroute/acme",
        "contacts": ["mailto:ops@example.test"], "terms_agreed": true, "challenge": "dns01", "key_type": "ecdsa_p256",
        "allowed_dns_suffixes": ["acme.example.test"], "retained_revisions": 3, "retention_days": 30,
        "dns01": { "provider": "cloudflare", "credential_file": "/etc/oxiroute/cf.ini", "timeout_seconds": 120 }
      }
    },
    { "name": "dev-cert", "dns_names": ["dev.example.test"], "source": { "type": "self_signed_development", "validity_days": 14, "key_type": "rsa_2048" } }
  ],
  "tls_profiles": [
    {
      "name": "edge-tls", "certificates": ["files-cert", "certbot-cert", "acme-cert", "dev-cert"], "default_certificate": "files-cert",
      "min_version": "1.3", "alpn": ["h2", "http/1.1"],
      "policy": {
        "client_auth": { "mode": "optional", "ca_certificate_path": "/etc/oxiroute/client-ca.pem", "allowed_dns_names": ["client.example.test"] },
        "session_cache": { "name": "edge_sessions", "size_bytes": 65536 },
        "session_timeout_seconds": 600, "session_tickets": true, "prefer_server_ciphers": true,
        "backend": "rustls", "key_exchange_groups": ["x25519mlkem768", "x25519", "secp256r1"], "ktls": "off"
      }
    },
    { "name": "h3-tls", "certificates": ["dev-cert"], "default_certificate": "dev-cert", "min_version": "1.3", "alpn": ["h3"] }
  ],
  "cache_stores": [
    { "name": "mem-cache", "type": "memory", "max_bytes": 16777216, "max_entries": 1024 },
    { "name": "disk-cache", "type": "disk", "root_directory": "/var/cache/oxiroute", "max_bytes": 67108864, "max_files": 2048 }
  ],
  "upstream_pools": [
    {
      "name": "app-pool",
      "servers": [
        { "name": "s1", "endpoint": { "type": "socket", "address": "127.0.0.1:3001" }, "max_connections": 250 },
        { "name": "s2", "endpoint": { "type": "dns", "host": "app2.internal", "port": 3002 }, "dns_resolution": "startup" }
      ],
      "algorithm": { "type": "weighted_round_robin", "weights": [3, 2] },
      "health_check": { "type": "http", "interval_ms": 2000, "fast_interval_ms": 1000, "down_interval_ms": 5000, "timeout_ms": 500, "healthy_threshold": 2, "unhealthy_threshold": 3, "startup": "checking", "host": "app.internal", "path": "/healthz", "expected_status": 200, "http_version": "1.1" },
      "passive_health": {
        "observe": "layer7", "on_error": "count", "error_limit": 3, "mark_down": true, "mark_up": true,
        "initial_backoff_ms": 1000, "max_backoff_ms": 10000, "recovery_threshold": 2,
        "readmission": { "successes": 2, "interval_ms": 2000, "timeout_ms": 500, "probe": { "path": "/ready", "host": "app.internal", "expected_status": 200 } },
        "circuit_breaker": { "window_ms": 30000, "min_requests": 20, "failure_ratio_percent": 50, "open_ms": 10000, "half_open_max": 3, "half_open_successes": 2 },
        "outlier_detection": { "factor_percent": 300, "min_samples": 10, "interval_ms": 15000, "max_ejected_percent": 33 }
      },
      "queue_timeout_ms": 4000, "connect_timeout_ms": 3000, "server_timeout_ms": 25000, "connection_reuse": "always",
      "resilience": { "fail_open": true, "min_members": 1, "ejection_jitter_percent": 15, "queue_while_unavailable_ms": 1500, "unavailable_retry_after_seconds": 5, "unavailable_body": "temporarily unavailable" },
      "queue": { "classes": [{ "name": "interactive", "weight": 4 }, { "name": "batch", "weight": 1 }], "discipline": "strict", "class_header": "x-priority", "default_class": "batch", "tenant_header": "x-tenant", "max_tenants": 64, "aging_ms": 1500 },
      "registration": { "max_servers": 16, "min_ttl_ms": 2000, "default_ttl_ms": 15000, "max_ttl_ms": 60000, "allowed_cidrs": ["10.0.0.0/8"], "require_peer_address": false },
      "canary": { "path": "/canary?probe=1", "host": "app.internal", "expected_status": 200, "interval_ms": 10000, "timeout_ms": 1000, "max_latency_ms": 250 }
    },
    {
      "name": "tls-pool",
      "servers": [{ "name": "t1", "endpoint": { "type": "socket", "address": "127.0.0.1:3443" } }],
      "tls": { "server_name": "app.internal", "verify": "full", "ca_certificate_path": "/etc/oxiroute/upstream-ca.pem" },
      "http_versions": { "min": "1.1", "max": "2" }
    },
    { "name": "unix-pool", "servers": [{ "name": "u1", "endpoint": { "type": "unix", "path": "/run/oxiroute/app3.sock" } }] },
    { "name": "l4-pool", "servers": [{ "name": "l4-1", "endpoint": { "type": "socket", "address": "127.0.0.1:4001" } }] }
  ],
  "http_services": [{
    "name": "web", "automatic_response_headers": false, "upstream_io_timeout_ms": 25000, "max_request_body_bytes": 2097152, "drain_deadline_ms": 45000,
    "gzip": { "level": 6, "content_types": ["text/html", "application/json"], "min_length_bytes": 128, "min_http_version": "1.1", "disable_on_via": true, "vary": true },
    "access_log": { "type": "file", "path": "/var/log/oxiroute/access.log" },
    "routes": [
      {
        "host": { "kind": "normalized_host", "value": "api.example.test" }, "path": { "kind": "segment_prefix", "value": "/api" }, "methods": ["GET", "POST"],
        "access_policy": { "type": "bearer_token_file", "token_file_path": "/etc/oxiroute/api.token", "header_name": "authorization", "realm": "api" },
        "policy": { "max_request_body_bytes": 1048576, "connect_timeout_ms": 3000, "read_timeout_ms": 20000, "write_timeout_ms": 20000, "request_buffering": true, "response_buffering": true },
        "action": {
          "type": "proxy", "upstream_pool": "app-pool",
          "policy": {
            "upstream_host": { "type": "literal", "value": "app.internal" },
            "upstream_path_rewrite": { "from": "/api", "to": "/v2" },
            "request_headers": [
              { "operation": "set", "name": "x-forwarded-for", "value": { "type": "appended_x_forwarded_for", "max_bytes": 4096 } },
              { "operation": "set", "name": "x-forwarded-proto", "value": { "type": "downstream_scheme" } },
              { "operation": "remove", "name": "x-debug" }
            ],
            "response_headers": [
              { "operation": "set", "name": "x-served-by", "value": "oxiroute" },
              { "operation": "add", "name": "x-edge", "value": "eu-1" },
              { "operation": "remove", "name": "server" }
            ],
            "response_cookie_path_rewrites": [{ "from": "/v2", "to": "/api", "mode": "nginx_prefix" }],
            "response_cookie_attributes": [{ "name": "sid", "secure": true, "http_only": true, "same_site": "strict" }],
            "retry": { "max_retries": 2, "triggers": ["connect_failure", "connect_timeout"], "response_statuses": [502, 503], "method_safety": "all", "body_safety": "buffered", "target": "same_server", "delay_ms": 100, "jitter_percent": 20, "final_redispatch": true, "budget": { "per_second": 50, "ratio_percent": 20 } },
            "timeouts": { "connect_ms": 2000, "header_ms": 5000, "idle_ms": 10000, "total_ms": 20000 },
            "cache": { "store": "mem-cache", "methods": ["GET", "HEAD"], "default_ttl_ms": 30000, "status_ttls": [{ "status": 200, "ttl_ms": 60000 }], "stale_on": ["connect_failure", "origin_503"], "compression": { "codings": ["zstd", "br", "gzip"], "precompress": ["gzip"], "min_length_bytes": 256, "gzip_level": 6, "brotli_level": 4, "zstd_level": 3 } },
            "queue_headers": true, "queue_class": "interactive", "affinity": { "type": "hash", "key": "header:x-tenant" }
          }
        }
      },
      { "host": { "kind": "exact_authority", "value": "health.example.test:8443" }, "path": { "kind": "exact", "value": "/healthz" }, "methods": ["GET"], "action": { "type": "fixed_response", "status": 200, "body": "ok", "headers": [{ "name": "content-type", "value": "text/plain" }] } },
      { "host": { "kind": "nginx_leading_wildcard", "value": "legacy.example.test" }, "path": { "kind": "raw_prefix", "value": "/old" }, "action": { "type": "redirect", "status": 301, "location": { "kind": "request_template", "value": "https://api.example.test$request_uri" }, "headers": [{ "name": "x-moved", "value": "1" }] } },
      {
        "host": { "kind": "nginx_leading_dot", "value": "static.example.test" }, "path": { "kind": "segment_prefix", "value": "/" },
        "access_policy": { "type": "basic_htpasswd_file", "htpasswd_file_path": "/etc/oxiroute/users.htpasswd", "realm": "static" },
        "action": {
          "type": "static_files", "root_directory": "/srv/www", "path_mapping": "alias", "index_files": ["index.html"],
          "try_files": [{ "type": "request_path" }, { "type": "request_path_directory" }, { "type": "relative", "path": "index.html" }, { "type": "status", "status": 404 }],
          "autoindex": true, "autoindex_exact_size": false, "autoindex_local_time": true,
          "mime": { "default_type": "application/octet-stream", "types": [{ "extension": "html", "content_type": "text/html" }, { "extension": "css", "content_type": "text/css" }] },
          "headers": [{ "name": "cache-control", "value": "public, max-age=60" }],
          "error_responses": [{ "statuses": [500, 502, 503, 504], "file": "50x.html" }]
        }
      },
      { "host": { "kind": "normalized_host", "value": "ping.example.test" }, "path": { "kind": "ascii_case_insensitive_exact", "value": "/PING" }, "action": { "type": "fixed_response", "status": 204 } }
    ]
  },
  {
    "name": "web-h3",
    "routes": [{ "path": { "kind": "segment_prefix", "value": "/" }, "policy": { "max_request_body_bytes": 1048576, "request_buffering": true, "response_buffering": false }, "action": { "type": "fixed_response", "status": 200, "body": "h3" } }]
  }],
  "forward_proxy_services": [{
    "name": "egress", "enabled_versions": ["h1", "h2"], "allow_absolute_form": true, "tls_required": false,
    "connect": { "enabled": true, "allowed_ports": [443, 8443] },
    "access_policy": { "rules": [{ "action": "allow", "conditions": [{ "type": "methods", "methods": ["GET", "CONNECT"] }, { "type": "source_cidrs", "cidrs": ["10.0.0.0/8"] }] }], "default_action": "deny" },
    "destination_policy": { "allow_domains": ["example.org"], "deny_private": true },
    "header_policy": { "forwarded_for": "delete", "via": "preserve" },
    "cache": { "store": "disk-cache", "default_ttl_ms": 60000 }, "max_request_body_bytes": 1048576
  }],
  "rtmp_services": [{
    "name": "ingest", "outbound_chunk_size": 4096, "access_log": { "type": "disabled" },
    "applications": [{
      "name": "live", "live": true,
      "publish": { "rules": [{ "action": "allow", "network": "127.0.0.0/8" }], "token": { "source": "stream_query", "parameter": "key", "secret": "rtmp-publish-secret" } },
      "hls": { "root_directory": "/var/lib/oxiroute/hls", "segment_duration_ms": 2000, "playlist_length_ms": 10000 },
      "dash": { "root_directory": "/var/lib/oxiroute/dash", "segment_duration_ms": 2000, "playlist_length_ms": 10000 },
      "recorders": [{ "name": "archive", "root_directory": "/var/lib/oxiroute/recordings", "suffix_template": "-%Y-%m-%d.flv", "timezone": "America/Argentina/Buenos_Aires", "time_basis": "segment_end", "segment_naming": "nginx_compatible" }]
    }]
  }],
  "l4_services": [
    { "name": "tcp-svc", "upstream_pool": "l4-pool", "connect_timeout_ms": 3000, "idle_timeout_ms": 60000, "lifetime_timeout_ms": 300000, "proxy_protocol": { "version": "v2", "timeout_ms": 2000 } },
    { "name": "udp-svc", "upstream_pool": "l4-pool", "idle_timeout_ms": 30000, "udp": { "max_datagram_bytes": 1400, "max_sessions": 512, "max_session_bytes": 65536, "max_queue_datagrams": 32, "max_queue_bytes": 65536 } }
  ],
  "listeners": [
    {
      "name": "https-v4", "bind": { "type": "socket", "address": "0.0.0.0:8443" },
      "protocol": "http", "service": "web", "tls_profile": "edge-tls", "max_connections": 2048,
      "downstream_timeouts": { "client_timeout_ms": 30000, "request_timeout_ms": 10000, "keepalive_timeout_ms": 15000 },
      "h2_initial_stream_window": 262144, "h2_initial_connection_window": 2097152
    },
    { "name": "https-v6", "bind": { "type": "socket", "address": "[::]:8443", "ipv6_only": true }, "protocol": "http", "service": "web", "tls_profile": "edge-tls" },
    { "name": "http3-in", "bind": { "type": "udp", "address": "127.0.0.1:8443" }, "protocol": "http3", "service": "web-h3", "tls_profile": "h3-tls" },
    { "name": "http-unix", "bind": { "type": "unix", "path": "/run/oxiroute/http.sock", "mode": 432 }, "protocol": "http", "service": "web" },
    { "name": "fwd-in", "bind": { "type": "socket", "address": "127.0.0.1:3128" }, "protocol": "forward_http1", "service": "egress" },
    { "name": "rtmp-in", "bind": { "type": "socket", "address": "127.0.0.1:1935" }, "protocol": "rtmp", "service": "ingest" },
    { "name": "tcp-in", "bind": { "type": "socket", "address": "127.0.0.1:5432" }, "protocol": "tcp", "service": "tcp-svc", "proxy_protocol": { "version": "auto", "timeout_ms": 3000 } },
    { "name": "udp-in", "bind": { "type": "udp", "address": "127.0.0.1:5353" }, "protocol": "udp", "service": "udp-svc" }
  ]
}"#;

fn kitchen_sink_config() -> ValidatedConfig {
    let draft: ConfigDraft =
        serde_json::from_str(KITCHEN_SINK_JSON).expect("kitchen sink draft deserializes");
    draft.validate().expect("kitchen sink draft validates")
}

#[test]
fn kitchen_sink_validated_config_round_trips_identically_across_all_7_formats() {
    let kitchen_sink = kitchen_sink_config();
    let expected_kdl =
        render_config(ConfigFormat::Kdl, &kitchen_sink).expect("canonical KDL render");

    for format in ALL_FORMATS {
        let rendered = render_config(format, &kitchen_sink)
            .unwrap_or_else(|err| panic!("render failed for {format:?}: {err}"));
        let resolved = resolve_source_with_format(Path::new("config"), rendered.as_bytes(), format)
            .unwrap_or_else(|err| panic!("resolve failed for {format:?}: {err}"));

        assert_eq!(resolved.format, format);
        assert!(
            !resolved.compositional,
            "{format:?} should not be compositional"
        );
        assert_eq!(
            resolved.config, kitchen_sink,
            "config mismatch for {format:?}"
        );
        assert_eq!(
            resolved.canonical_kdl, expected_kdl,
            "KDL mismatch for {format:?}"
        );

        let rerendered = render_config(format, &resolved.config)
            .unwrap_or_else(|err| panic!("re-render failed for {format:?}: {err}"));
        assert_eq!(
            rerendered, rendered,
            "non-deterministic render for {format:?}"
        );
    }
}

const TEMPLATES_YAML: &str = r#"
version: 1
vars:
  bind_host: "127.0.0.1"
  edge: { workers: 4, api_port: 4100 }
templates:
  app_pool:
    params: { port: 3000, algorithm: "round_robin" }
    algorithm: "{{algorithm}}"
    endpoints: [{ type: "socket", address: "{{bind_host}}:{{port}}" }]
listener_workers: "{{edge.workers}}"
upstream_pools:
  - { name: "default-app", use: "app_pool" }
  - { name: "api-app", use: { name: "app_pool", port: "{{edge.api_port}}", algorithm: "least_connections" } }
http_services:
  - name: "web"
    routes:
      - { path: { kind: "segment_prefix", value: "/api" }, action: { type: "proxy", upstream_pool: "api-app", policy: {} } }
      - { path: { kind: "segment_prefix", value: "/" }, action: { type: "proxy", upstream_pool: "default-app", policy: {} } }
listeners:
  - { name: "http-in", bind: { type: "socket", address: "{{bind_host}}:8080" }, protocol: "http", service: "web" }
"#;

const TEMPLATES_TOML: &str = r#"
version = 1
listener_workers = "{{edge.workers}}"
[vars]
bind_host = "127.0.0.1"
edge = { workers = 4, api_port = 4100 }
[templates.app_pool]
algorithm = "{{algorithm}}"
params = { port = 3000, algorithm = "round_robin" }
endpoints = [{ type = "socket", address = "{{bind_host}}:{{port}}" }]
[[upstream_pools]]
name = "default-app"
use = "app_pool"
[[upstream_pools]]
name = "api-app"
use = { name = "app_pool", port = "{{edge.api_port}}", algorithm = "least_connections" }
[[http_services]]
name = "web"
routes = [
  { path = { kind = "segment_prefix", value = "/api" }, action = { type = "proxy", upstream_pool = "api-app", policy = {} } },
  { path = { kind = "segment_prefix", value = "/" }, action = { type = "proxy", upstream_pool = "default-app", policy = {} } },
]
[[listeners]]
name = "http-in"
bind = { type = "socket", address = "{{bind_host}}:8080" }
protocol = "http"
service = "web"
"#;

const TEMPLATES_LUA: &str = r#"
return {
  version = 1,
  vars = { bind_host = "127.0.0.1", edge = { workers = 4, api_port = 4100 } },
  templates = {
    app_pool = {
      params = { port = 3000, algorithm = "round_robin" },
      algorithm = "{{algorithm}}",
      endpoints = { { type = "socket", address = "{{bind_host}}:{{port}}" } },
    },
  },
  listener_workers = "{{edge.workers}}",
  upstream_pools = {
    { name = "default-app", use = "app_pool" },
    { name = "api-app", use = { name = "app_pool", port = "{{edge.api_port}}", algorithm = "least_connections" } },
  },
  http_services = {{
    name = "web",
    routes = {
      { path = { kind = "segment_prefix", value = "/api" }, action = { type = "proxy", upstream_pool = "api-app", policy = {} } },
      { path = { kind = "segment_prefix", value = "/" }, action = { type = "proxy", upstream_pool = "default-app", policy = {} } },
    },
  }},
  listeners = {{
    name = "http-in",
    bind = { type = "socket", address = "{{bind_host}}:8080" },
    protocol = "http",
    service = "web",
  }},
}
"#;

const TEMPLATES_OXI: &str = r"
version 1
vars
  bind_host 127.0.0.1
  edge
    workers 4
    api_port 4100
template app_pool
  params port=3000 algorithm=round_robin
  algorithm {{algorithm}}
  endpoints
    - type socket
      address {{bind_host}}:{{port}}
listener_workers {{edge.workers}}
pool default-app
  use app_pool
pool api-app
  use app_pool port={{edge.api_port}} algorithm=least_connections
service web
  routes
    - path
        kind segment_prefix
        value /api
      action
        type proxy
        upstream_pool api-app
        policy {}
    - path
        kind segment_prefix
        value /
      action
        type proxy
        upstream_pool default-app
        policy {}
listener http-in
  bind
    type socket
    address {{bind_host}}:8080
  protocol http
  service web
";

const TEMPLATES_HOCON: &str = r#"
version = 1
vars { bind_host = "127.0.0.1", edge { workers = 4, api_port = 4100 } }
templates {
  app_pool {
    params { port = 3000, algorithm = "round_robin" }
    algorithm = "{{algorithm}}"
    endpoints = [{ type = "socket", address = "{{bind_host}}:{{port}}" }]
  }
}
listener_workers = "{{edge.workers}}"
upstream_pools = [
  { name = "default-app", use = "app_pool" },
  { name = "api-app", use = { name = "app_pool", port = "{{edge.api_port}}", algorithm = "least_connections" } }
]
http_services = [{
  name = "web"
  routes = [
    { path = { kind = "segment_prefix", value = "/api" }, action = { type = "proxy", upstream_pool = "api-app", policy = {} } },
    { path = { kind = "segment_prefix", value = "/" }, action = { type = "proxy", upstream_pool = "default-app", policy = {} } }
  ]
}]
listeners = [{ name = "http-in", bind = { type = "socket", address = "{{bind_host}}:8080" }, protocol = "http", service = "web" }]
"#;

const TEMPLATES_KDL: &str = r#"
version 1
(object)vars { bind_host "127.0.0.1"; (object)edge { workers 4; api_port 4100 } }
(object)templates {
  (object)app_pool {
    (object)params { port 3000; algorithm "round_robin" }
    algorithm "{{algorithm}}"
    (array)endpoints { (object)- { type "socket"; address "{{bind_host}}:{{port}}" } }
  }
}
listener_workers "{{edge.workers}}"
(array)upstream_pools {
  (object)- { name "default-app"; use "app_pool" }
  (object)- { name "api-app"; (object)use { name "app_pool"; port "{{edge.api_port}}"; algorithm "least_connections" } }
}
(array)http_services {
  (object)- {
    name "web"
    (array)routes {
      (object)- { (object)path { kind "segment_prefix"; value "/api" }; (object)action { type "proxy"; upstream_pool "api-app"; (object)policy {} } }
      (object)- { (object)path { kind "segment_prefix"; value "/" }; (object)action { type "proxy"; upstream_pool "default-app"; (object)policy {} } }
    }
  }
}
(array)listeners { (object)- { name "http-in"; (object)bind { type "socket"; address "{{bind_host}}:8080" }; protocol "http"; service "web" } }
"#;

#[test]
fn parameterized_templates_and_vars_produce_identical_config_across_all_7_formats() {
    let uci_source = encode_uci_json(
        &decode_value(ConfigFormat::Yaml, TEMPLATES_YAML.as_bytes()).expect("decode YAML tree"),
    );
    let sources: [(ConfigFormat, &str); 7] = [
        (ConfigFormat::Kdl, TEMPLATES_KDL),
        (ConfigFormat::Lua, TEMPLATES_LUA),
        (ConfigFormat::Uci, &uci_source),
        (ConfigFormat::Hocon, TEMPLATES_HOCON),
        (ConfigFormat::Yaml, TEMPLATES_YAML),
        (ConfigFormat::Toml, TEMPLATES_TOML),
        (ConfigFormat::Oxi, TEMPLATES_OXI),
    ];

    let baseline = resolve_source_with_format(
        Path::new("cfg"),
        TEMPLATES_YAML.as_bytes(),
        ConfigFormat::Yaml,
    )
    .expect("baseline YAML template config resolves");

    let draft = baseline.config.as_draft();
    assert_eq!(draft.listener_workers, Some(4));
    assert_eq!(
        draft.upstream_pools[0].algorithm,
        UpstreamAlgorithm::RoundRobin
    );
    assert_eq!(
        draft.upstream_pools[0].servers[0].endpoint.to_string(),
        "127.0.0.1:3000"
    );
    assert_eq!(
        draft.upstream_pools[1].algorithm,
        UpstreamAlgorithm::LeastConnections
    );
    assert_eq!(
        draft.upstream_pools[1].servers[0].endpoint.to_string(),
        "127.0.0.1:4100"
    );

    for (format, source) in sources {
        let resolved = resolve_source_with_format(Path::new("cfg"), source.as_bytes(), format)
            .unwrap_or_else(|err| panic!("template resolution failed for {format:?}: {err}"));
        assert!(resolved.compositional, "{format:?} must be compositional");
        assert_eq!(
            resolved.config, baseline.config,
            "config mismatch for {format:?}"
        );
        assert_eq!(
            resolved.canonical_kdl, baseline.canonical_kdl,
            "KDL mismatch for {format:?}"
        );
    }
}

const SUGAR_OXI: &str = r"
edge
  http 0.0.0.0:80 -> https
  https [::]:443
  stats 127.0.0.1:8404/stats

site app
  hosts app.example.com *.app.example.com
  cert certbot
  proxy 127.0.0.1:3001 127.0.0.1:3002
  gzip on
  route =/health -> 200 ok
  route /v1 -> 127.0.0.1:3010

site docs
  host docs.app.example.com
  cert app
  static /var/www/docs

site legacy
  host legacy.app.example.com
  cert app
  redirect
    status 301
    to https://app.example.com$request_uri
";

const SUGAR_YAML: &str = r#"
edge: { http: "0.0.0.0:80", redirect_https: true, https: "[::]:443", stats: "127.0.0.1:8404/stats" }
sites:
  app:
    hosts: ["app.example.com", "*.app.example.com"]
    cert: "certbot"
    proxy: ["127.0.0.1:3001", "127.0.0.1:3002"]
    gzip: true
    routes:
      - { path: "=/health", respond: { status: 200, body: "ok" } }
      - { path: "/v1", proxy: "127.0.0.1:3010" }
  docs: { host: "docs.app.example.com", cert: "app", static: "/var/www/docs" }
  legacy: { host: "legacy.app.example.com", cert: "app", redirect: { status: 301, to: "https://app.example.com$request_uri" } }
"#;

const SUGAR_TOML: &str = r#"
[edge]
http = "0.0.0.0:80"
redirect_https = true
https = "[::]:443"
stats = "127.0.0.1:8404/stats"
[sites.app]
hosts = ["app.example.com", "*.app.example.com"]
cert = "certbot"
proxy = ["127.0.0.1:3001", "127.0.0.1:3002"]
gzip = true
routes = [
  { path = "=/health", respond = { status = 200, body = "ok" } },
  { path = "/v1", proxy = "127.0.0.1:3010" },
]
[sites.docs]
host = "docs.app.example.com"
cert = "app"
static = "/var/www/docs"
[sites.legacy]
host = "legacy.app.example.com"
cert = "app"
redirect = { status = 301, to = "https://app.example.com$request_uri" }
"#;

const SUGAR_LUA: &str = r#"
return {
  edge = { http = "0.0.0.0:80", redirect_https = true, https = "[::]:443", stats = "127.0.0.1:8404/stats" },
  sites = {
    app = {
      hosts = { "app.example.com", "*.app.example.com" },
      cert = "certbot",
      proxy = { "127.0.0.1:3001", "127.0.0.1:3002" },
      gzip = true,
      routes = {
        { path = "=/health", respond = { status = 200, body = "ok" } },
        { path = "/v1", proxy = "127.0.0.1:3010" },
      },
    },
    docs = { host = "docs.app.example.com", cert = "app", static = "/var/www/docs" },
    legacy = { host = "legacy.app.example.com", cert = "app", redirect = { status = 301, to = "https://app.example.com$request_uri" } },
  },
}
"#;

const SUGAR_HOCON: &str = r#"
edge { http = "0.0.0.0:80", redirect_https = true, https = "[::]:443", stats = "127.0.0.1:8404/stats" }
sites {
  app {
    hosts = ["app.example.com", "*.app.example.com"]
    cert = "certbot"
    proxy = ["127.0.0.1:3001", "127.0.0.1:3002"]
    gzip = true
    routes = [
      { path = "=/health", respond = { status = 200, body = "ok" } },
      { path = "/v1", proxy = "127.0.0.1:3010" }
    ]
  }
  docs { host = "docs.app.example.com", cert = "app", static = "/var/www/docs" }
  legacy { host = "legacy.app.example.com", cert = "app", redirect = { status = 301, to = "https://app.example.com$request_uri" } }
}
"#;

const SUGAR_KDL: &str = r#"
(object)edge { http "0.0.0.0:80"; redirect_https #true; https "[::]:443"; stats "127.0.0.1:8404/stats" }
(object)sites {
  (object)app {
    (array)hosts { - "app.example.com"; - "*.app.example.com" }
    cert "certbot"
    (array)proxy { - "127.0.0.1:3001"; - "127.0.0.1:3002" }
    gzip #true
    (array)routes {
      (object)- { path "=/health"; (object)respond { status 200; body "ok" } }
      (object)- { path "/v1"; proxy "127.0.0.1:3010" }
    }
  }
  (object)docs { host "docs.app.example.com"; cert "app"; static "/var/www/docs" }
  (object)legacy { host "legacy.app.example.com"; cert "app"; (object)redirect { status 301; to "https://app.example.com$request_uri" } }
}
"#;

#[test]
fn edge_and_sites_sugar_produces_identical_config_across_all_7_formats() {
    let uci_source = encode_uci_json(
        &decode_value(ConfigFormat::Yaml, SUGAR_YAML.as_bytes()).expect("decode YAML sugar"),
    );
    let sources: [(ConfigFormat, &str); 7] = [
        (ConfigFormat::Oxi, SUGAR_OXI),
        (ConfigFormat::Yaml, SUGAR_YAML),
        (ConfigFormat::Toml, SUGAR_TOML),
        (ConfigFormat::Lua, SUGAR_LUA),
        (ConfigFormat::Hocon, SUGAR_HOCON),
        (ConfigFormat::Kdl, SUGAR_KDL),
        (ConfigFormat::Uci, &uci_source),
    ];

    let baseline =
        resolve_source_with_format(Path::new("edge"), SUGAR_OXI.as_bytes(), ConfigFormat::Oxi)
            .expect("baseline OXI sugar resolves");

    let cfg = baseline.config.as_draft();
    assert_eq!(cfg.certificates.len(), 1);
    assert!(matches!(
        cfg.certificates[0].source,
        CertificateSource::Certbot { .. }
    ));
    assert_eq!(cfg.tls_profiles.len(), 1);
    assert_eq!(cfg.tls_profiles[0].certificates, vec!["app"]);
    let app_pool = cfg
        .upstream_pools
        .iter()
        .find(|p| p.name == "site-app")
        .expect("site-app pool");
    assert_eq!(app_pool.servers.len(), 2);
    assert!(cfg.stats.is_some());
    assert!(
        cfg.http_services
            .iter()
            .any(|svc| svc.name == "edge-http-redirect")
    );
    let https_svc = cfg
        .http_services
        .iter()
        .find(|svc| svc.name == "edge-https")
        .expect("edge-https service");
    assert!(https_svc.gzip.is_some());
    assert!(
        https_svc
            .routes
            .iter()
            .any(|r| matches!(r.action, HttpRouteAction::FixedResponse { status: 200, .. }))
    );
    assert!(
        https_svc
            .routes
            .iter()
            .any(|r| matches!(r.action, HttpRouteAction::StaticFiles { .. }))
    );
    assert!(
        https_svc
            .routes
            .iter()
            .any(|r| matches!(r.action, HttpRouteAction::Redirect { status: 301, .. }))
    );

    for (format, source) in sources {
        let resolved = resolve_source_with_format(Path::new("edge"), source.as_bytes(), format)
            .unwrap_or_else(|err| panic!("sugar resolution failed for {format:?}: {err}"));
        assert!(resolved.compositional, "{format:?} must be compositional");
        assert_eq!(
            resolved.config, baseline.config,
            "sugar config mismatch for {format:?}"
        );
        assert_eq!(
            resolved.canonical_kdl, baseline.canonical_kdl,
            "sugar KDL mismatch for {format:?}"
        );
    }
}

const OXI_FULL_STACK_ROOT: &str = r#"
vars
  domain example.dev
  bind_host 127.0.0.1
  workers 4
listener_workers lua(vars.workers)
template svc_site
  params port=3000
  cert main
  gzip on
  proxy {{bind_host}}:{{port}}
edge
  http 0.0.0.0:80 -> https
  https [::]:443
  stats 127.0.0.1:8404/stats
site main
  hosts example.dev *.example.dev
  cert certbot
  proxy 127.0.0.1:3000
  gzip on
  route =/health -> 200 ok
  route /api -> 127.0.0.1:3050
include sites/extra.oxi
include data/extra_site.yaml
lua
  local generated = {}
  for _, line in ipairs(read_lines("microservices.txt")) do
    local parts = str.split(line, ":")
    local name, port = parts[1], tonumber(parts[2])
    generated["svc_" .. name] = {
      host = name .. "." .. vars.domain,
      cert = "main",
      proxy = net.join_host_port(vars.bind_host, port),
    }
  end
  return { sites = generated }
"#;

#[test]
fn oxi_full_stack_showcase_combines_vars_templates_edge_sites_lua_and_includes() {
    let dir = tempdir().expect("create tempdir");
    let root_path = dir.path().join("oxiroute.oxi");
    let microservices_txt = write_file(
        &dir.path().join("microservices.txt"),
        "# name:port list\nauth:4101\nbilling:4102\n",
    );
    let extra_oxi = write_file(
        &dir.path().join("sites/extra.oxi"),
        "site blog\n  use svc_site port=3200\n  host blog.{{domain}}\n  route =/ping -> 200 pong\n",
    );
    let extra_yaml = write_file(
        &dir.path().join("data/extra_site.yaml"),
        "sites:\n  portal:\n    host: \"portal.{{domain}}\"\n    cert: \"main\"\n    static: \"/var/www/portal\"\n",
    );

    let resolved = resolve_source(&root_path, OXI_FULL_STACK_ROOT.as_bytes())
        .expect("full-stack OXI resolves");
    assert_eq!(resolved.format, ConfigFormat::Oxi);
    assert!(resolved.compositional);

    let cfg = resolved.config.as_draft();
    assert_eq!(cfg.listener_workers, Some(4));
    assert_eq!(cfg.certificates.len(), 1);
    assert_eq!(cfg.certificates[0].name, "main");
    assert_eq!(cfg.tls_profiles.len(), 1);
    assert_eq!(cfg.tls_profiles[0].name, "edge-tls");
    assert!(cfg.stats.is_some());
    assert_eq!(cfg.http_services.len(), 2);
    assert_eq!(cfg.listeners.len(), 2);

    for pool in [
        "site-main",
        "site-main-route-2",
        "site-blog",
        "site-svc_auth",
        "site-svc_billing",
    ] {
        assert!(
            cfg.upstream_pools.iter().any(|p| p.name == pool),
            "missing pool `{pool}`"
        );
    }

    let https_svc = cfg
        .http_services
        .iter()
        .find(|svc| svc.name == "edge-https")
        .expect("edge-https service");
    for host in [
        "example.dev",
        "blog.example.dev",
        "portal.example.dev",
        "auth.example.dev",
        "billing.example.dev",
    ] {
        assert!(
            https_svc.routes.iter().any(|route| {
                route.host
                    == Some(HttpHostSelector::NormalizedHost {
                        value: host.to_owned(),
                    })
            }),
            "missing route for host `{host}`"
        );
    }
    assert!(https_svc.routes.iter().any(|route| {
        route.path
            == HttpPathSelector::Exact {
                value: "/ping".to_owned(),
            }
    }));

    for dep in [&extra_oxi, &extra_yaml, &microservices_txt] {
        assert!(
            resolved.dependencies.contains(dep),
            "missing dependency: {}",
            dep.display()
        );
    }
}

#[test]
fn explicit_null_overrides_preserved_in_six_formats_and_rejected_fail_closed_in_toml() {
    let draft: ConfigDraft = serde_json::from_str(
        r#"{
          "version": 1,
          "http_services": [{
            "name": "unbounded",
            "max_request_body_bytes": null,
            "routes": [{
              "path": { "kind": "segment_prefix", "value": "/" },
              "policy": { "max_request_body_bytes": null },
              "action": { "type": "fixed_response", "status": 200, "body": "ok" }
            }]
          }],
          "listeners": [{
            "name": "http",
            "bind": { "type": "socket", "address": "127.0.0.1:8080" },
            "protocol": "http",
            "service": "unbounded"
          }]
        }"#,
    )
    .expect("unbounded draft deserializes");
    let cfg = draft.validate().expect("unbounded draft validates");
    assert_eq!(cfg.as_draft().http_services[0].max_request_body_bytes, None);
    assert_eq!(
        cfg.as_draft().http_services[0].routes[0]
            .policy
            .max_request_body_bytes,
        None
    );

    for format in [
        ConfigFormat::Kdl,
        ConfigFormat::Lua,
        ConfigFormat::Uci,
        ConfigFormat::Hocon,
        ConfigFormat::Yaml,
        ConfigFormat::Oxi,
    ] {
        let rendered = render_config(format, &cfg)
            .unwrap_or_else(|err| panic!("render failed for {format:?}: {err}"));
        let resolved =
            resolve_source_with_format(Path::new("unbounded"), rendered.as_bytes(), format)
                .unwrap_or_else(|err| panic!("resolve failed for {format:?}: {err}"));
        assert_eq!(resolved.config, cfg, "null override lost in {format:?}");
        assert_eq!(
            resolved.config.as_draft().http_services[0].max_request_body_bytes,
            None
        );
        assert_eq!(
            resolved.config.as_draft().http_services[0].routes[0]
                .policy
                .max_request_body_bytes,
            None
        );
    }

    let toml_err = render_config(ConfigFormat::Toml, &cfg)
        .expect_err("TOML must reject explicit null override of non-null default");
    assert!(
        matches!(toml_err, ConfigSourceError::Render { format: "TOML", .. }),
        "expected TOML render error, got {toml_err:?}"
    );
}

#[test]
fn all_7_formats_enforce_source_size_and_utf8_bounds() {
    let oversized = vec![b' '; MAX_SOURCE_BYTES + 1];
    let invalid_utf8: &[u8] = &[0xFF, 0xFE, 0xFD];

    for format in ALL_FORMATS {
        assert!(matches!(
            decode_value(format, &oversized),
            Err(ConfigSourceError::SourceTooLarge)
        ));
        assert!(matches!(
            resolve_source_with_format(Path::new("config"), &oversized, format),
            Err(ConfigSourceError::SourceTooLarge)
        ));
        assert!(matches!(
            decode_value(format, invalid_utf8),
            Err(ConfigSourceError::Utf8(_))
        ));
        assert!(matches!(
            resolve_source_with_format(Path::new("config"), invalid_utf8, format),
            Err(ConfigSourceError::Utf8(_))
        ));
    }
}
