use oxiroute_config::{ConfigError, ProxyProtocolVersion, load_lua, render_lua};

const CONFIG: &str = r#"
return {
  version = 1,
  listeners = {
    {
      name = "edge",
      bind = { type = "socket", address = "127.0.0.1:15432" },
      protocol = "tcp",
      service = "database",
      proxy_protocol = { version = "auto", timeout_ms = 250 },
    },
  },
  upstream_pools = {
    {
      name = "database-pool",
      endpoints = { { type = "socket", address = "127.0.0.1:5432" } },
    },
  },
  l4_services = {
    {
      name = "database",
      upstream_pool = "database-pool",
      proxy_protocol = { version = "v2", timeout_ms = 250 },
    },
  },
}
"#;

#[test]
fn proxy_protocol_policy_round_trips_and_renders_canonically() {
    let config = load_lua(CONFIG).expect("PROXY protocol configuration");
    assert_eq!(
        config.listeners[0]
            .proxy_protocol
            .expect("listener policy")
            .version,
        ProxyProtocolVersion::Auto
    );
    assert_eq!(
        config.l4_services[0]
            .proxy_protocol
            .expect("service policy")
            .version,
        ProxyProtocolVersion::V2
    );
    let rendered = render_lua(&config).expect("render policy");
    assert!(rendered.contains("proxy_protocol = {"));
    assert_eq!(load_lua(&rendered).expect("reload rendered policy"), config);
}

#[test]
fn proxy_protocol_bounds_and_transport_mismatches_fail_closed() {
    let timeout = CONFIG.replace("timeout_ms = 250", "timeout_ms = 0");
    assert!(matches!(
        load_lua(&timeout),
        Err(ConfigError::InvalidProxyProtocolPolicy { .. })
    ));

    let udp_v1 = CONFIG
        .replace(
            "type = \"socket\", address = \"127.0.0.1:15432\"",
            "type = \"udp\", address = \"127.0.0.1:15432\"",
        )
        .replace("protocol = \"tcp\"", "protocol = \"udp\"")
        .replace("version = \"auto\"", "version = \"v1\"");
    assert!(matches!(
        load_lua(&udp_v1),
        Err(ConfigError::InvalidProxyProtocolPolicy { .. })
    ));
}

#[test]
fn udp_proxy_protocol_reserves_wire_space_for_the_address_header() {
    let udp = CONFIG
        .replace(
            "type = \"socket\", address = \"127.0.0.1:15432\"",
            "type = \"udp\", address = \"127.0.0.1:15432\"",
        )
        .replace("protocol = \"tcp\"", "protocol = \"udp\"")
        .replace(
            "proxy_protocol = { version = \"v2\", timeout_ms = 250 },",
            "proxy_protocol = { version = \"v2\", timeout_ms = 250 },\n       udp = { max_datagram_bytes = 65456 },",
        );
    assert!(matches!(
        load_lua(&udp),
        Err(ConfigError::InvalidL4UdpPolicy { field, .. })
            if field == "udp.max_datagram_bytes"
    ));

    let bounded = udp.replace("max_datagram_bytes = 65456", "max_datagram_bytes = 65455");
    load_lua(&bounded).expect("bounded UDP PROXY configuration");
}
