use oxiroute_config::{load_lua, render_lua};

fn proxy_config(stores: &str, cache: &str) -> String {
    format!(
        r#"return {{
  version = 1,
  listeners = {{}},
  cache_stores = {{ {stores} }},
  upstream_pools = {{
    {{ name = "origin", endpoints = {{ {{ type = "socket", address = "127.0.0.1:3000" }} }} }},
  }},
  http_services = {{
    {{ name = "web", routes = {{
      {{
        path = {{ kind = "segment_prefix", value = "/" }},
        action = {{ type = "proxy", upstream_pool = "origin", policy = {{ cache = {cache} }} }},
      }},
    }} }},
  }},
}}"#
    )
}

fn forward_config(service: &str, listener: &str, tls: &str) -> String {
    format!(
        r#"return {{
  version = 1,
  certificates = {{
    {{
      name = "forward-cert",
      dns_names = {{ "proxy.example.test" }},
      source = {{
        type = "files",
        certificate_chain_path = "/etc/oxiroute/forward-chain.pem",
        private_key_path = "/etc/oxiroute/forward-key.pem",
      }},
    }},
  }},
  tls_profiles = {{ {tls} }},
  listeners = {{ {listener} }},
  forward_proxy_services = {{ {service} }},
}}"#
    )
}

fn error(source: &str) -> String {
    load_lua(source)
        .expect_err("configuration must be rejected")
        .to_string()
}

#[test]
fn applies_finite_cache_store_and_policy_defaults() {
    let source = proxy_config(
        r#"{ name = "memory", type = "memory" }"#,
        r#"{ store = "memory" }"#,
    );
    let config = load_lua(&source).expect("default cache policy");
    let value = serde_json::to_value(config).expect("serialized cache config");
    let store = &value["cache_stores"][0];
    let cache = &value["http_services"][0]["routes"][0]["action"]["policy"]["cache"];

    assert_eq!(store["type"], "memory");
    assert_eq!(store["max_bytes"], 268_435_456_u64);
    assert_eq!(store["max_entries"], 100_000_u64);
    assert_eq!(store["max_object_bytes"], 16_777_216_u64);
    assert_eq!(store["max_header_bytes"], 65_536_u64);
    assert_eq!(store["max_key_bytes"], 4_096_u64);
    assert_eq!(store["max_tag_bytes"], 256_u64);
    assert_eq!(store["max_tags_per_object"], 64_u64);
    assert_eq!(store["max_in_flight_fills"], 1_024_u64);
    assert_eq!(store["max_followers_per_fill"], 128_u64);
    assert_eq!(cache["methods"], serde_json::json!(["GET", "HEAD"]));
    assert_eq!(
        cache["key_components"],
        serde_json::json!([
            {"type": "scheme"},
            {"type": "normalized_host"},
            {"type": "path_and_query"}
        ])
    );
    assert_eq!(cache["use_origin_cache_control"], true);
    assert_eq!(cache["default_ttl_ms"], 60_000_u64);
    assert_eq!(cache["grace_ms"], 30_000_u64);
    assert_eq!(cache["keep_ms"], 300_000_u64);
    assert_eq!(cache["revalidate"], true);
    assert_eq!(cache["collapsed_forwarding"], true);
    assert_eq!(cache["set_cookie_policy"], "bypass");
    assert_eq!(cache["authorization_policy"], "bypass");
    assert_eq!(cache["vary_policy"], "respect");
    assert_eq!(cache["purge_authorization"], serde_json::Value::Null);
}

#[test]
fn validates_disk_roots_store_limits_and_references() {
    let disk = r#"{
      name = "disk",
      type = "disk",
      root_directory = "/var//cache///oxiroute",
    }"#;
    let config =
        load_lua(&proxy_config(disk, r#"{ store = "disk" }"#)).expect("normalized disk cache");
    let value = serde_json::to_value(config).expect("serialized cache");
    assert_eq!(
        value["cache_stores"][0]["root_directory"],
        "/var/cache/oxiroute"
    );
    assert_eq!(value["cache_stores"][0]["max_files"], 1_000_000_u64);

    let duplicate_roots = r#"
      { name = "first", type = "disk", root_directory = "/var/cache/oxiroute" },
      { name = "second", type = "disk", root_directory = "/var//cache///oxiroute" }
    "#;
    assert!(
        error(&proxy_config(duplicate_roots, r#"{ store = "first" }"#))
            .contains("unique across disk stores")
    );

    for root in ["var/cache", "/", "/var/../cache", "/var/cache/"] {
        let disk = format!(r#"{{ name = "disk", type = "disk", root_directory = "{root}" }}"#);
        assert!(error(&proxy_config(&disk, r#"{ store = "disk" }"#)).contains("root"));
    }
    assert!(
        error(&proxy_config(
            r#"{ name = "memory", type = "memory", max_bytes = 1024, max_object_bytes = 2048 }"#,
            r#"{ store = "memory" }"#,
        ))
        .contains("max_object_bytes")
    );
    assert!(
        error(&proxy_config(
            r#"{ name = "memory", type = "memory" }"#,
            r#"{ store = "missing" }"#,
        ))
        .contains("unknown cache store")
    );
}

#[test]
fn validates_cache_keys_retention_predicates_and_purge_secrets() {
    let store = r#"{ name = "memory", type = "memory" }"#;
    let cache = r#"{
      store = "memory",
      methods = { "head", "get" },
      key_components = {
        { type = "scheme" },
        { type = "header", name = "X-Tenant" },
        { type = "cookie", name = "tenant" },
      },
      status_ttls = { { status = 200, ttl_ms = 120000 } },
      grace_ms = 1000,
      keep_ms = 2000,
      stale_on = { "connect_timeout", "origin_503" },
      bypass_request = { { type = "header_present", name = "X-Bypass" } },
      no_store_request = { { type = "cookie_present", name = "session" } },
      no_store_response = { { type = "header_present", name = "X-No-Store" } },
      surrogate_tags = { response_header = "Surrogate-Key", max_tags = 32, max_tag_bytes = 128 },
      purge_authorization = {
        type = "bearer_token_file",
        token_file_path = "/run/secrets/cache-purge",
      },
    }"#;
    let loaded = load_lua(&proxy_config(store, cache)).expect("complete cache policy");
    let rendered = render_lua(&loaded).expect("rendered cache policy");
    assert_eq!(load_lua(&rendered).expect("cache reload"), loaded);

    for cache in [
        r#"{ store = "memory", methods = { "POST" } }"#,
        r#"{ store = "memory", key_components = { { type = "scheme" }, { type = "scheme" } } }"#,
        r#"{ store = "memory", grace_ms = 2000, keep_ms = 1000 }"#,
        r#"{ store = "memory", status_ttls = { { status = 200, ttl_ms = 1 }, { status = 200, ttl_ms = 2 } } }"#,
        r#"{ store = "memory", status_ttls = { { status = 199, ttl_ms = 1 } } }"#,
        r#"{ store = "memory", status_ttls = { { status = 206, ttl_ms = 1 } } }"#,
        r#"{ store = "memory", status_ttls = { { status = 304, ttl_ms = 1 } } }"#,
        r#"{ store = "memory", stale_on = { "connect_timeout", "connect_timeout" } }"#,
        r#"{ store = "memory", purge_authorization = { type = "bearer_token_file", token_file_path = "/run/token", token = "inline" } }"#,
    ] {
        assert!(!error(&proxy_config(store, cache)).is_empty());
    }
}

#[test]
fn applies_finite_forward_proxy_defaults() {
    let service = r#"{ name = "egress" }"#;
    let config = load_lua(&forward_config(service, "", "")).expect("default forward proxy");
    let value = serde_json::to_value(config).expect("serialized forward proxy");
    let service = &value["forward_proxy_services"][0];

    assert_eq!(service["enabled_versions"], serde_json::json!(["h1"]));
    assert_eq!(service["allow_absolute_form"], true);
    assert_eq!(service["tls_required"], true);
    assert_eq!(service["connect"]["enabled"], false);
    assert_eq!(
        service["connect"]["allowed_ports"],
        serde_json::json!([443])
    );
    assert_eq!(service["auth"], serde_json::Value::Null);
    assert_eq!(service["destination_policy"]["deny_private"], true);
    assert_eq!(
        service["destination_policy"]["allow_times"],
        serde_json::json!([])
    );
    assert_eq!(
        service["destination_policy"]["deny_times"],
        serde_json::json!([])
    );
    assert_eq!(service["connect_timeout_ms"], 10_000_u64);
    assert_eq!(service["idle_timeout_ms"], 300_000_u64);
    assert_eq!(service["lifetime_timeout_ms"], 3_600_000_u64);
    assert_eq!(service["max_request_body_bytes"], 10_485_760_u64);
    assert_eq!(service["max_header_bytes"], 65_536_u64);
    assert_eq!(service["max_connections"], 10_000_u64);
    assert_eq!(service["audit_mode"], "metadata");
    assert_eq!(service["resolver"]["max_cache_entries"], 4_096_u64);
    assert_eq!(service["resolver"]["max_concurrent_queries"], 256_u64);
    assert_eq!(service["resolver"]["revalidate_on_connect"], true);
}

#[test]
fn validates_forward_destinations_connect_auth_and_finite_limits() {
    let service = r#"{
      name = "egress",
      enabled_versions = { "h1", "h2" },
      connect = { enabled = true, allowed_ports = { 443, 8443 } },
      auth = { type = "bearer_token_file", token_file_path = "/run/secrets/proxy" },
      destination_policy = {
        allow_domains = { "example.com", "*.example.net" },
        deny_domains = { "blocked.example.com" },
        allow_cidrs = { "203.0.113.0/24", "2001:db8::/32" },
        deny_cidrs = { "203.0.113.128/25" },
        deny_private = true,
        allow_times = {
          { days = { "friday", "monday" }, start = "09:00", ["end"] = "17:00" },
        },
        deny_times = {
          { days = { "monday" }, start = "12:00", ["end"] = "13:00" },
        },
      },
    }"#;
    let loaded = load_lua(&forward_config(service, "", "")).expect("forward policy");
    let rendered = render_lua(&loaded).expect("rendered forward policy");
    assert_eq!(load_lua(&rendered).expect("forward reload"), loaded);

    assert!(error(&forward_config(
        r#"{ name = "egress", auth = { type = "mutual_tls", client_ca_file_path = "/run/client-ca.pem" } }"#,
        "",
        "",
    ))
    .contains("client-certificate verifier"));

    for service in [
        r#"{ name = "egress", enabled_versions = {} }"#,
        r#"{ name = "egress", enabled_versions = { "h1", "h1" } }"#,
        r#"{ name = "egress", connect = { enabled = true, allowed_ports = {} } }"#,
        r#"{ name = "egress", connect = { enabled = true, allowed_ports = { 0 } } }"#,
        r#"{ name = "egress", auth = { type = "bearer_token_file", token_file_path = "run/token" } }"#,
        r#"{ name = "egress", destination_policy = { allow_cidrs = { "10.0.0.0/99" } } }"#,
        r#"{ name = "egress", destination_policy = { allow_times = { { days = {}, start = "09:00", end = "17:00" } } } }"#,
        r#"{ name = "egress", destination_policy = { allow_times = { { days = { "monday" }, start = "17:00", end = "09:00" } } } }"#,
        r#"{ name = "egress", destination_policy = { allow_times = { { days = { "monday" }, start = "09:00", end = "17:01" } } } }"#,
        r#"{ name = "egress", max_connections = 0 }"#,
        r#"{ name = "egress", max_header_bytes = 0 }"#,
        r#"{ name = "egress", max_header_bytes = 8191 }"#,
    ] {
        assert!(!error(&forward_config(service, "", "")).is_empty());
    }
}

#[test]
fn enforces_forward_listener_version_transport_tls_and_exact_service_kind() {
    let service = r#"{ name = "egress", enabled_versions = { "h1", "h2", "h3" } }"#;
    let tls = r#"{
      name = "forward-h3",
      certificates = { "forward-cert" },
      default_certificate = "forward-cert",
      min_version = "1.3",
      alpn = { "h3" },
    }"#;
    let h3 = r#"{
      name = "forward-h3",
      bind = { type = "udp", address = "127.0.0.1:8443" },
      protocol = "forward_http3",
      service = "egress",
      tls_profile = "forward-h3",
    }"#;
    load_lua(&forward_config(service, h3, tls)).expect("H3 forward listener");

    let h1 = r#"{
      name = "forward-h1",
      bind = { type = "unix", path = "/run/oxiroute/forward.sock" },
      protocol = "forward_http1",
      service = "egress",
    }"#;
    let no_tls_service = service.replace(" }", ", tls_required = false }");
    load_lua(&forward_config(&no_tls_service, h1, tls)).expect("Unix H1 forward listener");

    let h1_tls = h1
        .replace(
            "type = \"unix\", path = \"/run/oxiroute/forward.sock\"",
            "type = \"socket\", address = \"127.0.0.1:3129\"",
        )
        .replace(
            "service = \"egress\",",
            "service = \"egress\", tls_profile = \"forward-h1\",",
        );
    let h1_profile = tls
        .replace("forward-h3", "forward-h1")
        .replace("min_version = \"1.3\"", "min_version = \"1.2\"")
        .replace("alpn = { \"h3\" }", "alpn = { \"http/1.1\" }");
    assert!(
        error(&forward_config(service, &h1_tls, &h1_profile))
            .contains("does not support downstream TLS")
    );

    for listener in [
        h3.replace("type = \"udp\"", "type = \"socket\""),
        h3.replace("tls_profile = \"forward-h3\",", ""),
        h3.replace("service = \"egress\"", "service = \"missing\""),
        h1.replace("protocol = \"forward_http1\"", "protocol = \"http\""),
        h1.replace(
            "service = \"egress\",",
            "service = \"egress\", downstream_timeouts = { client_timeout_ms = 1 },",
        ),
    ] {
        assert!(!error(&forward_config(service, &listener, tls)).is_empty());
    }
}

#[test]
fn validates_udp_overlap_separately_from_stream_binds() {
    let service = r#"{ name = "egress", enabled_versions = { "h1", "h3" }, tls_required = false }"#;
    let tls = r#"{
      name = "forward-h3",
      certificates = { "forward-cert" },
      default_certificate = "forward-cert",
      min_version = "1.3",
      alpn = { "h3" },
    }"#;
    let listeners = r#"
      { name = "stream", bind = { type = "socket", address = "127.0.0.1:8443" }, protocol = "forward_http1", service = "egress" },
      { name = "datagram", bind = { type = "udp", address = "127.0.0.1:8443" }, protocol = "forward_http3", service = "egress", tls_profile = "forward-h3" }
    "#;
    load_lua(&forward_config(service, listeners, tls)).expect("TCP and UDP may share a port");

    let duplicate_udp = format!(
        "{listeners}, {{ name = \"duplicate\", bind = {{ type = \"udp\", address = \"0.0.0.0:8443\" }}, protocol = \"forward_http3\", service = \"egress\", tls_profile = \"forward-h3\" }}"
    );
    assert!(error(&forward_config(service, &duplicate_udp, tls)).contains("overlap"));
}

#[test]
fn rejects_aliases_inline_secrets_and_oversized_sources() {
    assert!(
        !error(&proxy_config(
            r#"{ name = "memory", kind = "memory" }"#,
            r#"{ store = "memory" }"#,
        ))
        .is_empty()
    );
    assert!(
        !error(&proxy_config(
            r#"{ name = "memory", type = "memory" }"#,
            r#"{ cache_store = "memory" }"#,
        ))
        .is_empty()
    );
    assert!(
        !error(&forward_config(
            r#"{
          name = "egress",
          auth = { type = "bearer_token_file", token_file_path = "/run/token", token = "inline" },
        }"#,
            "",
            "",
        ))
        .is_empty()
    );

    let oversized = format!(
        "return {{ version = 1, listeners = {{}}, padding = {:?} }}",
        "x".repeat(1024 * 1024)
    );
    assert!(error(&oversized).contains("source limit"));
}
