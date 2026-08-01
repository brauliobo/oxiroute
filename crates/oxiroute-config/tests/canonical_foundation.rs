use oxiroute_config::{
    ConfigError, HttpGzipMinimumVersion, HttpHostSelector, HttpRequestHeaderValue, HttpRouteAction,
    RtmpRecorderSegmentNaming, RtmpRecorderTimeBasis, RtmpRecorderTimezone, UpstreamAlgorithm,
    load_lua, render_lua,
};

const COMPLETE: &str = r#"
return {
  version = 1,
  max_connections = 4096,
  listeners = {
    {
      name = "frontend",
      bind = { type = "unix", path = "/run/oxiroute/frontend.sock", mode = 432 },
      protocol = "http",
      service = "web",
      max_connections = 2000,
      downstream_timeouts = {
        client_timeout_ms = 50000,
        request_timeout_ms = 10000,
        keepalive_timeout_ms = 10000,
      },
    },
  },
  upstream_pools = {
    {
      name = "application",
      servers = {
        {
          name = "app01",
          endpoint = { type = "dns", host = "APP01.LAN", port = 3000 },
          max_connections = 500,
          dns_resolution = "startup",
        },
        {
          name = "app02",
          endpoint = { type = "dns", host = "app02.lan", port = 3000 },
          max_connections = null,
          dns_resolution = "on_connect",
        },
      },
      algorithm = "first",
      queue_timeout_ms = 5000,
      connect_timeout_ms = 5000,
      server_timeout_ms = 50000,
      connection_reuse = "safe",
      health_check = {
        type = "http",
        startup = "checking",
        interval_ms = 2000,
        fast_interval_ms = 1000,
        down_interval_ms = 10000,
        timeout_ms = 500,
        healthy_threshold = 2,
        unhealthy_threshold = 3,
        path = "/healthz",
        expected_status = 200,
        http_version = "1.0",
      },
    },
  },
  http_services = {
    {
      name = "web",
      gzip = { level = 6, content_types = { "text/plain", "application/json" } },
      access_log = { type = "file", path = "/var/log/oxiroute/web-access.log" },
      routes = {
        {
          host = { kind = "nginx_leading_wildcard", value = "EXAMPLE.LAN" },
          path = { kind = "segment_prefix", value = "/api" },
          access_policy = {
            type = "basic_htpasswd_file",
            htpasswd_file_path = "/etc/oxiroute/users.htpasswd",
            realm = "private",
          },
          policy = {
            max_request_body_bytes = 1048576,
            connect_timeout_ms = 5000,
            read_timeout_ms = 50000,
            write_timeout_ms = 50000,
            request_buffering = true,
            response_buffering = false,
          },
          action = {
            type = "proxy",
            upstream_pool = "application",
            policy = {
              request_headers = {
                { operation = "set", name = "X-Forwarded-For", value = { type = "appended_x_forwarded_for", max_bytes = 4096 } },
                { operation = "set", name = "X-Forwarded-Proto", value = { type = "downstream_scheme" } },
                { operation = "set", name = "X-Request-Id", value = { type = "incoming_header", name = "X-Request-Id", max_bytes = 256 } },
              },
              response_cookie_attributes = {
                { name = "session", secure = true, http_only = true, same_site = "lax" },
              },
            },
          },
        },
        {
          host = { kind = "nginx_leading_dot", value = "STATIC.EXAMPLE.LAN" },
          path = { kind = "segment_prefix", value = "/" },
          policy = {
            max_request_body_bytes = null,
            connect_timeout_ms = 30000,
            read_timeout_ms = 30000,
            write_timeout_ms = 30000,
          },
          action = {
            type = "static_files",
            root_directory = "/srv/www",
            path_mapping = "alias",
            index_files = { "index.html" },
            try_files = {
              { type = "request_path" },
              { type = "request_path_directory" },
              { type = "relative", path = "index.html" },
              { type = "status", status = 404 },
            },
            autoindex = true,
            autoindex_exact_size = false,
            autoindex_local_time = true,
            mime = {
              default_type = "application/octet-stream",
              types = {
                { extension = "html", content_type = "text/html" },
                { extension = "css", content_type = "text/css" },
              },
            },
            headers = { { name = "Cache-Control", value = "public, max-age=60" } },
            error_responses = {
              { statuses = { 500, 502, 503, 504 }, file = "50x.html" },
            },
          },
        },
      },
    },
  },
  rtmp_services = {
    {
      name = "live",
      outbound_chunk_size = 4096,
      access_log = { type = "disabled" },
      applications = {
        {
          name = "live",
          live = true,
          push_targets = {
            { host = "127.0.0.1", port = 1936, application = "$name" },
          },
          fanout = {
            max_subscribers = 500,
            max_queue_messages_per_subscriber = 128,
            max_queue_bytes_per_subscriber = 1048576,
          },
          recorders = {
            {
              name = "archive",
              root_directory = "/var/lib/oxiroute/recordings",
              suffix_template = "-%Y-%m-%d.flv",
              timezone = "America/Bahia",
              time_basis = "segment_end",
              segment_naming = "nginx_compatible",
            },
          },
        },
      },
    },
  },
}
"#;

#[test]
fn round_trips_the_complete_host_replacement_foundation() {
    let config = load_lua(COMPLETE).expect("complete canonical foundation");

    assert_eq!(config.max_connections, Some(4_096));
    assert_eq!(
        config.listeners[0].downstream_timeouts.client_timeout_ms,
        Some(50_000)
    );
    assert_eq!(config.upstream_pools[0].servers[0].name, "app01");
    assert_eq!(
        config.upstream_pools[0].servers[0].endpoint.to_string(),
        "app01.lan:3000"
    );
    assert_eq!(config.upstream_pools[0].algorithm, UpstreamAlgorithm::First);
    assert_eq!(
        config.upstream_pools[0]
            .health_check
            .as_ref()
            .expect("health")
            .host,
        None
    );
    assert!(matches!(
        config.http_services[0].routes[0].host,
        Some(HttpHostSelector::NginxLeadingWildcard { ref value }) if value == "example.lan"
    ));
    let HttpRouteAction::Proxy { policy, .. } = &config.http_services[0].routes[0].action else {
        panic!("proxy action");
    };
    assert!(matches!(
        policy.request_headers[2],
        oxiroute_config::HttpRequestHeaderMutation::Set {
            value: HttpRequestHeaderValue::IncomingHeader { max_bytes: 256, .. },
            ..
        }
    ));
    let recorder = &config.rtmp_services[0].applications[0].recorders[0];
    assert_eq!(
        recorder.timezone,
        RtmpRecorderTimezone::Iana("America/Bahia".into())
    );
    assert_eq!(recorder.time_basis, RtmpRecorderTimeBasis::SegmentEnd);
    assert_eq!(
        recorder.segment_naming,
        RtmpRecorderSegmentNaming::NginxCompatible
    );
    let gzip = config.http_services[0].gzip.as_ref().expect("gzip policy");
    assert_eq!(gzip.min_length_bytes, 20);
    assert_eq!(gzip.min_http_version, HttpGzipMinimumVersion::Http10);
    assert!(!gzip.disable_on_via);
    assert!(gzip.vary);

    let rendered = render_lua(&config).expect("rendered canonical foundation");
    assert_eq!(load_lua(&rendered).expect("rendered reload"), config);
    assert_eq!(render_lua(&config).expect("second render"), rendered);
    for field in [
        "downstream_timeouts",
        "servers",
        "dns_resolution",
        "queue_timeout_ms",
        "connection_reuse",
        "fast_interval_ms",
        "expected_status",
        "response_cookie_attributes",
        "min_length_bytes",
        "min_http_version",
        "disable_on_via",
        "vary",
        "path_mapping",
        "try_files",
        "autoindex_exact_size",
        "autoindex_local_time",
        "error_responses",
        "outbound_chunk_size",
        "push_targets",
        "fanout",
        "timezone",
        "time_basis",
        "segment_naming",
    ] {
        assert!(rendered.contains(&format!("{field} =")), "missing {field}");
    }
}

#[test]
fn persisted_minimal_gzip_policy_retains_legacy_runtime_defaults() {
    let config = load_lua(COMPLETE).expect("persisted minimal gzip policy");
    let gzip = config.http_services[0].gzip.as_ref().expect("gzip policy");

    assert_eq!(gzip.level, 6);
    assert_eq!(gzip.content_types, ["text/plain", "application/json"]);
    assert_eq!(gzip.min_length_bytes, 20);
    assert_eq!(gzip.min_http_version, HttpGzipMinimumVersion::Http10);
    assert!(!gzip.disable_on_via);
    assert!(gzip.vary);
}

#[test]
fn normalizes_legacy_anonymous_endpoints_to_named_canonical_servers() {
    let config = load_lua(
        r#"return {
  version = 1,
  listeners = {},
  upstream_pools = {
    { name = "legacy", endpoints = { { type = "socket", address = "127.0.0.1:3000" } } },
  },
}"#,
    )
    .expect("legacy endpoint syntax");

    assert!(config.upstream_pools[0].endpoints.is_empty());
    assert_eq!(config.upstream_pools[0].servers[0].name, "endpoint-1");
    let rendered = render_lua(&config).expect("canonical named server render");
    assert!(rendered.contains("servers = {"));
    assert!(!rendered.contains("endpoints = {"));
}

#[test]
fn rejects_invalid_foundation_policy_bounds_and_combinations() {
    let cases = [
        ("max_connections = 4096", "max_connections = 0"),
        (
            "max_connections = 4096",
            "max_connections = 9007199254740992",
        ),
        ("mode = 432", "mode = 0"),
        ("mode = 432", "mode = 512"),
        ("client_timeout_ms = 50000", "client_timeout_ms = 0"),
        ("name = \"app01\"", "name = \" \""),
        ("max_connections = 500", "max_connections = 0"),
        (
            "dns_resolution = \"startup\"",
            "dns_resolution = \"invalid\"",
        ),
        ("expected_status = 200", "expected_status = 199"),
        ("queue_timeout_ms = 5000", "queue_timeout_ms = 86400001"),
        (
            "connect_timeout_ms = 5000,\n            read_timeout_ms",
            "connect_timeout_ms = 0,\n            read_timeout_ms",
        ),
        ("max_bytes = 4096", "max_bytes = 8193"),
        (
            "htpasswd_file_path = \"/etc/oxiroute/users.htpasswd\"",
            "htpasswd_file_path = \"etc/users.htpasswd\"",
        ),
        ("status = 404", "status = 200"),
        ("level = 6", "level = 10"),
        ("outbound_chunk_size = 4096", "outbound_chunk_size = 0"),
        ("max_subscribers = 500", "max_subscribers = 0"),
        ("application = \"$name\"", "application = \"$other\""),
    ];

    for (from, to) in cases {
        let source = COMPLETE.replacen(from, to, 1);
        assert_ne!(source, COMPLETE, "test replacement must match: {from}");
        assert!(load_lua(&source).is_err(), "invalid policy accepted: {to}");
    }

    let mixed_servers = COMPLETE.replacen(
        "      servers = {",
        "      endpoints = { { type = \"socket\", address = \"127.0.0.1:3000\" } },\n      servers = {",
        1,
    );
    assert!(matches!(
        load_lua(&mixed_servers),
        Err(ConfigError::InvalidUpstreamServer {
            field: "servers",
            ..
        })
    ));
}
