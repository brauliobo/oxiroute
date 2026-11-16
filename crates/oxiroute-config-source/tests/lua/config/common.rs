use oxiroute_config::ConfigError;

use crate::lua_support::{load_lua, load_lua_error};

pub(super) const VALID_CONFIG: &str = r#"
return {
  version = 1,
  management = {
    bind = "127.0.0.1:9080",
    ui_dir = "./ui/dist",
  },
  certificates = {
    {
      name = "web-certificate",
      dns_names = { "WWW.EXAMPLE.TEST", "*.EXAMPLE.TEST" },
      source = {
        type = "files",
        certificate_chain_path = "/etc/oxiroute/web-chain.pem",
        private_key_path = "/etc/oxiroute/web-key.pem",
      },
    },
  },
  tls_profiles = {
    {
      name = "web-tls",
      certificates = { "web-certificate" },
      default_certificate = "web-certificate",
      min_version = "1.3",
      alpn = { "h2", "http/1.1" },
    },
  },
  listeners = {
    {
      name = "web",
      bind = { type = "socket", address = "127.0.0.1:8080" },
      protocol = "http",
      service = "web",
      tls_profile = "web-tls",
      max_connections = 5000,
    },
    {
      name = "database",
      bind = { type = "socket", address = "127.0.0.1:15432" },
      protocol = "tcp",
      service = "database",
      max_connections = 1000,
    },
    {
      name = "live",
      bind = { type = "socket", address = "127.0.0.1:1935" },
      protocol = "rtmp",
      service = "live",
      max_connections = 500,
    },
  },
  upstream_pools = {
    {
      name = "web-backends",
      endpoints = {
        { type = "socket", address = "127.0.0.1:3000" },
        { type = "socket", address = "127.0.0.1:3001" },
      },
      algorithm = "round_robin",
    },
    {
      name = "database-backends",
      endpoints = { { type = "socket", address = "192.0.2.58:5432" } },
      algorithm = "round_robin",
    },
  },
  http_services = {
    {
      name = "web",
      routes = {
        {
          host = { kind = "normalized_host", value = "example.com" },
          path = { kind = "segment_prefix", value = "/api" },
          methods = { "GET", "POST" },
          action = {
            type = "proxy",
            upstream_pool = "web-backends",
            policy = {},
          },
        },
      },
      upstream_io_timeout_ms = 15000,
      max_request_body_bytes = 2097152,
    },
  },
  rtmp_services = {
    {
      name = "live",
      applications = {
        { name = "live", live = true, idle_streams = true },
      },
    },
  },
  l4_services = {
    {
      name = "database",
      upstream_pool = "database-backends",
      connect_timeout_ms = 5000,
      idle_timeout_ms = 120000,
      lifetime_timeout_ms = 600000,
    },
  },
}
"#;

pub(super) fn changed(from: &str, to: &str) -> String {
    assert_eq!(
        VALID_CONFIG.matches(from).count(),
        1,
        "fixture fragment must occur exactly once: {from}"
    );
    VALID_CONFIG.replacen(from, to, 1)
}

pub(super) fn error_from(source: &str) -> ConfigError {
    load_lua(source).expect_err("configuration must be rejected")
}

pub(super) fn lua_error_from(source: &str) -> oxiroute_config_source::LuaConfigError {
    load_lua_error(source)
}

pub(super) fn with_web_pool_fields(fields: &str) -> String {
    let pool = r#"      endpoints = {
        { type = "socket", address = "127.0.0.1:3000" },
        { type = "socket", address = "127.0.0.1:3001" },
      },
      algorithm = "round_robin","#;
    changed(pool, &format!("{pool}\n{fields}"))
}
