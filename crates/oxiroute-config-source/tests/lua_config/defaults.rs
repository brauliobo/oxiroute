use oxiroute_config::{
    AlpnProtocol, CertificateSource, ConfigError, HttpVersion, ListenerBind, Protocol, TlsVersion,
    UpstreamAlgorithm, UpstreamEndpoint,
};

use crate::common::{VALID_CONFIG, changed, lua_error_from};
use crate::lua_support::load_lua;

#[test]
fn loads_the_canonical_configuration() {
    let config = load_lua(VALID_CONFIG).expect("valid canonical configuration");

    assert_eq!(config.version, 1);
    assert_eq!(
        config.management.as_ref().expect("management").bind.port(),
        9080
    );
    assert_eq!(config.certificates.len(), 1);
    assert_eq!(
        config.certificates[0].dns_names,
        ["www.example.test", "*.example.test"]
    );
    assert!(matches!(
        config.certificates[0].source,
        CertificateSource::Files { .. }
    ));
    assert_eq!(config.tls_profiles.len(), 1);
    assert_eq!(config.tls_profiles[0].certificates, ["web-certificate"]);
    assert_eq!(
        config.tls_profiles[0].default_certificate,
        "web-certificate"
    );
    assert_eq!(config.tls_profiles[0].min_version, TlsVersion::Tls13);
    assert_eq!(
        config.tls_profiles[0].alpn,
        [AlpnProtocol::H2, AlpnProtocol::Http11]
    );
    assert_eq!(config.listeners.len(), 3);
    assert!(config.cache_stores.is_empty());
    assert_eq!(
        config.listeners[0].bind,
        ListenerBind::Socket {
            address: "127.0.0.1:8080".parse().expect("socket address"),
            ipv6_only: false
        }
    );
    assert_eq!(config.listeners[0].protocol, Protocol::Http);
    assert_eq!(config.listeners[0].service.as_deref(), Some("web"));
    assert_eq!(config.listeners[0].tls_profile.as_deref(), Some("web-tls"));
    assert_eq!(config.listeners[1].protocol, Protocol::Tcp);
    assert_eq!(config.listeners[2].protocol, Protocol::Rtmp);
    assert_eq!(config.listeners[2].service.as_deref(), Some("live"));
    assert_eq!(config.upstream_pools.len(), 2);
    assert_eq!(
        config.upstream_pools[0].servers[0].endpoint,
        UpstreamEndpoint::Socket {
            address: "127.0.0.1:3000".parse().expect("socket address")
        }
    );
    assert_eq!(
        config.upstream_pools[0].algorithm,
        UpstreamAlgorithm::RoundRobin
    );
    assert_eq!(config.upstream_pools[0].tls, None);
    assert_eq!(
        config.upstream_pools[0].http_versions.min,
        HttpVersion::Http11
    );
    assert_eq!(
        config.upstream_pools[0].http_versions.max,
        HttpVersion::Http11
    );
    assert_eq!(
        serde_json::to_value(&config.http_services[0].routes[0]).expect("serialized route")["host"]
            ["value"],
        "example.com"
    );
    assert_eq!(config.l4_services[0].lifetime_timeout_ms, Some(600_000));
    assert_eq!(config.rtmp_services[0].applications[0].name, "live");
    assert!(config.rtmp_services[0].applications[0].live);
    assert!(config.rtmp_services[0].applications[0].idle_streams);
    assert!(config.rtmp_services[0].applications[0].recorders.is_empty());
}

#[test]
fn loads_a_bounded_udp_service_and_listener() {
    let source = changed(
        r#"      bind = { type = "socket", address = "127.0.0.1:15432" },
      protocol = "tcp","#,
        r#"      bind = { type = "udp", address = "127.0.0.1:15432" },
      protocol = "udp","#,
    )
    .replace(
        "      lifetime_timeout_ms = 600000,\n",
        "      lifetime_timeout_ms = 600000,\n      udp = { max_datagram_bytes = 1200, max_sessions = 16, max_session_bytes = 65536, max_queue_datagrams = 4, max_queue_bytes = 8192 },\n",
    );
    let config = load_lua(&source).expect("bounded UDP configuration");
    assert_eq!(config.listeners[1].protocol, Protocol::Udp);
    assert!(matches!(
        config.listeners[1].bind,
        ListenerBind::Udp { address } if address == "127.0.0.1:15432".parse().expect("UDP bind")
    ));
    let policy = config.l4_services[0].udp.expect("UDP policy");
    assert_eq!(policy.max_datagram_bytes, 1200);
    assert_eq!(policy.max_sessions, 16);
    assert_eq!(policy.max_session_bytes, 65_536);
    assert_eq!(policy.max_queue_datagrams, 4);
    assert_eq!(policy.max_queue_bytes, 8192);
}

#[test]
fn rejects_an_unbounded_udp_policy() {
    let source = changed(
        "      lifetime_timeout_ms = 600000,",
        "      lifetime_timeout_ms = 600000,\n      udp = { max_datagram_bytes = 0 },",
    );
    assert!(matches!(
        load_lua(&source),
        Err(ConfigError::InvalidL4UdpPolicy { field, .. })
            if field == "udp.max_datagram_bytes"
    ));
}

#[test]
fn loads_the_distributed_example_configuration() {
    let config = load_lua(include_str!("../../../../oxiroute.example.lua"))
        .expect("distributed example must remain valid");

    assert_eq!(config.listeners.len(), 3);
    assert!(config.cache_stores.is_empty());
    assert_eq!(config.upstream_pools.len(), 2);
    assert_eq!(config.http_services.len(), 1);
    assert!(config.forward_proxy_services.is_empty());
    assert_eq!(config.rtmp_services.len(), 1);
    assert!(config.rtmp_services[0].applications[0].recorders.is_empty());
    assert_eq!(config.l4_services.len(), 1);
}

#[test]
#[allow(clippy::too_many_lines)]
fn applies_all_collection_and_field_defaults() {
    let minimal = load_lua(
        r#"
return {
  version = 1,
  listeners = {
    {
      name = "live",
      bind = { type = "socket", address = "127.0.0.1:1935" },
      protocol = "rtmp",
      service = "live",
    },
  },
  rtmp_services = {
    {
      name = "live",
      applications = {
        { name = "live", live = true },
      },
    },
  },
}
"#,
    )
    .expect("minimal configuration");

    assert_eq!(minimal.management, None);
    assert_eq!(minimal.max_connections, None);
    assert!(minimal.certificates.is_empty());
    assert!(minimal.tls_profiles.is_empty());
    assert_eq!(minimal.listeners[0].max_connections, None);
    assert_eq!(
        minimal.listeners[0].downstream_timeouts,
        oxiroute_config::DownstreamTimeoutPolicy::default()
    );
    assert_eq!(minimal.listeners[0].tls_profile, None);
    assert!(minimal.upstream_pools.is_empty());
    assert!(minimal.http_services.is_empty());
    assert!(minimal.rtmp_services[0].applications[0].idle_streams);
    assert_eq!(minimal.rtmp_services[0].outbound_chunk_size, 4_096);
    assert_eq!(
        minimal.rtmp_services[0].max_inbound_message_size,
        8 * 1024 * 1024
    );
    assert_eq!(minimal.rtmp_services[0].ack_window_size, 5_000_000);
    assert_eq!(minimal.rtmp_services[0].access_log, None);
    assert_eq!(
        minimal.rtmp_services[0].applications[0]
            .fanout
            .max_subscribers,
        1_024
    );
    assert!(
        minimal.rtmp_services[0].applications[0]
            .recorders
            .is_empty()
    );
    assert!(minimal.l4_services.is_empty());

    let source = VALID_CONFIG
        .replace("      max_connections = 5000,\n", "")
        .replace("      max_connections = 1000,\n", "")
        .replace("      max_connections = 500,\n", "")
        .replace("      algorithm = \"round_robin\",\n", "")
        .replace(
            "          host = { kind = \"normalized_host\", value = \"example.com\" },\n",
            "",
        )
        .replace("          methods = { \"GET\", \"POST\" },\n", "")
        .replace("      upstream_io_timeout_ms = 15000,\n", "")
        .replace("      max_request_body_bytes = 2097152,\n", "")
        .replace("      connect_timeout_ms = 5000,\n", "")
        .replace("      idle_timeout_ms = 120000,\n", "")
        .replace("      lifetime_timeout_ms = 600000,\n", "");
    let source = source
        .replace("      min_version = \"1.3\",\n", "")
        .replace("      alpn = { \"h2\", \"http/1.1\" },\n", "");
    let config = load_lua(&source).expect("configuration using field defaults");
    let route = &config.http_services[0].routes[0];

    assert!(
        config
            .listeners
            .iter()
            .all(|listener| listener.max_connections.is_none())
    );
    assert!(
        config
            .upstream_pools
            .iter()
            .all(|pool| pool.algorithm == UpstreamAlgorithm::RoundRobin)
    );
    assert_eq!(route.host, None);
    assert_eq!(
        serde_json::to_value(route).expect("serialized route")["path"]["value"],
        "/api"
    );
    assert!(route.methods.is_empty());
    assert_eq!(route.policy.connect_timeout_ms, 30_000);
    assert_eq!(route.policy.read_timeout_ms, 30_000);
    assert_eq!(route.policy.write_timeout_ms, 30_000);
    assert!(!route.policy.request_buffering);
    assert!(!route.policy.response_buffering);
    assert_eq!(config.http_services[0].upstream_io_timeout_ms, 30_000);
    assert_eq!(
        serde_json::to_value(route).expect("serialized route")["action"]["policy"]["retry"]["max_retries"],
        0
    );
    assert_eq!(
        config.http_services[0].max_request_body_bytes,
        Some(10 * 1024 * 1024)
    );
    assert_eq!(config.l4_services[0].connect_timeout_ms, 10_000);
    assert_eq!(config.l4_services[0].idle_timeout_ms, 300_000);
    assert_eq!(config.l4_services[0].lifetime_timeout_ms, None);
    assert_eq!(config.tls_profiles[0].min_version, TlsVersion::Tls12);
    assert_eq!(config.tls_profiles[0].alpn, [AlpnProtocol::Http11]);
    assert_eq!(
        config.tls_profiles[0].policy,
        oxiroute_config::TlsPolicy::default()
    );
    assert!(config.upstream_pools.iter().all(|pool| {
        pool.tls.is_none()
            && pool.http_versions.min == HttpVersion::Http11
            && pool.http_versions.max == HttpVersion::Http11
            && pool.connection_reuse == oxiroute_config::UpstreamConnectionReuse::Safe
    }));
}

#[test]
fn applies_optional_admission_defaults_without_fabricating_limits() {
    let listener_omitted = load_lua(
        r#"
return {
  version = 1,
  listeners = {
    {
      name = "live",
      bind = { type = "socket", address = "127.0.0.1:1935" },
      protocol = "rtmp",
      service = "live",
    },
  },
  rtmp_services = {
    { name = "live", applications = { { name = "live" } } },
  },
}
"#,
    )
    .expect("omitted listener limit");
    assert_eq!(listener_omitted.listeners[0].max_connections, None);

    let listener_null = changed(
        "      max_connections = 5000,",
        "      max_connections = null,",
    );
    assert_eq!(
        load_lua(&listener_null)
            .expect("null listener limit")
            .listeners[0]
            .max_connections,
        None
    );
    assert_eq!(
        load_lua(VALID_CONFIG)
            .expect("numeric listener limit")
            .listeners[0]
            .max_connections,
        Some(5_000)
    );

    let body_omitted = changed("      max_request_body_bytes = 2097152,\n", "");
    assert_eq!(
        load_lua(&body_omitted)
            .expect("omitted body limit")
            .http_services[0]
            .max_request_body_bytes,
        Some(10 * 1024 * 1024)
    );
    let body_null = changed(
        "      max_request_body_bytes = 2097152,",
        "      max_request_body_bytes = null,",
    );
    assert_eq!(
        load_lua(&body_null).expect("null body limit").http_services[0].max_request_body_bytes,
        None
    );
    let body_nil = changed(
        "      max_request_body_bytes = 2097152,",
        "      max_request_body_bytes = nil,",
    );
    assert_eq!(
        load_lua(&body_nil)
            .expect("nil is an omitted Lua field")
            .http_services[0]
            .max_request_body_bytes,
        Some(10 * 1024 * 1024)
    );
    assert_eq!(
        load_lua(VALID_CONFIG)
            .expect("numeric body limit")
            .http_services[0]
            .max_request_body_bytes,
        Some(2_097_152)
    );
}

#[test]
fn applies_the_same_optional_admission_contract_to_json() {
    let base = serde_json::json!({
        "version": 1,
        "listeners": [{
            "name": "live",
            "bind": { "type": "socket", "address": "127.0.0.1:1935" },
            "protocol": "rtmp"
        }],
        "http_services": [{ "name": "web", "routes": [] }]
    });

    let omitted: oxiroute_config::ConfigDraft =
        serde_json::from_value(base.clone()).expect("omitted JSON limits");
    assert_eq!(omitted.max_connections, None);
    assert_eq!(omitted.listeners[0].max_connections, None);
    assert_eq!(
        omitted.http_services[0].max_request_body_bytes,
        Some(10 * 1024 * 1024)
    );

    let mut explicit_null = base.clone();
    explicit_null["listeners"][0]["max_connections"] = serde_json::Value::Null;
    explicit_null["max_connections"] = serde_json::Value::Null;
    explicit_null["http_services"][0]["max_request_body_bytes"] = serde_json::Value::Null;
    let explicit_null: oxiroute_config::ConfigDraft =
        serde_json::from_value(explicit_null).expect("null JSON limits");
    assert_eq!(explicit_null.listeners[0].max_connections, None);
    assert_eq!(explicit_null.max_connections, None);
    assert_eq!(explicit_null.http_services[0].max_request_body_bytes, None);

    let mut numeric = base;
    numeric["listeners"][0]["max_connections"] = 321.into();
    numeric["max_connections"] = 123.into();
    numeric["http_services"][0]["max_request_body_bytes"] = 654.into();
    let numeric: oxiroute_config::ConfigDraft =
        serde_json::from_value(numeric).expect("numeric JSON limits");
    assert_eq!(numeric.listeners[0].max_connections, Some(321));
    assert_eq!(numeric.max_connections, Some(123));
    assert_eq!(numeric.http_services[0].max_request_body_bytes, Some(654));
}

#[test]
fn requires_explicit_tagged_listener_and_upstream_objects() {
    let old_listener = changed(
        r#"      bind = { type = "socket", address = "127.0.0.1:8080" },"#,
        r#"      bind = "127.0.0.1:8080","#,
    );
    assert!(matches!(
        lua_error_from(&old_listener),
        oxiroute_config_source::LuaConfigError::Lua(_)
    ));

    let old_endpoint = changed(
        r#"        { type = "socket", address = "127.0.0.1:3000" },"#,
        r#"        "127.0.0.1:3000","#,
    );
    assert!(matches!(
        lua_error_from(&old_endpoint),
        oxiroute_config_source::LuaConfigError::Lua(_)
    ));

    for source in [
        changed(
            r#"      bind = { type = "socket", address = "127.0.0.1:8080" },"#,
            r#"      bind = { type = "socket", address = "127.0.0.1:8080", path = "/run/web.sock" },"#,
        ),
        changed(
            r#"        { type = "socket", address = "127.0.0.1:3000" },"#,
            r#"        { type = "socket", address = "127.0.0.1:3000", host = "backend.test" },"#,
        ),
    ] {
        let error = lua_error_from(&source);
        assert!(matches!(
            error,
            oxiroute_config_source::LuaConfigError::Lua(_)
        ));
        assert!(error.to_string().contains("unknown field"));
    }
}
