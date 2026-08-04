return {
  version = 1,
  management = {
    bind = "127.0.0.1:9080",
    ui_dir = "./ui/dist",
  },
  certificates = {},
  tls_profiles = {},
  listeners = {
    {
      name = "web",
      bind = { type = "socket", address = "127.0.0.1:8080" },
      protocol = "http",
      service = "web",
    },
    {
      name = "postgres",
      bind = { type = "socket", address = "127.0.0.1:15432" },
      protocol = "tcp",
      service = "postgres",
    },
    {
      name = "live",
      bind = { type = "socket", address = "127.0.0.1:1935" },
      protocol = "rtmp",
      service = "live",
    },
  },
  cache_stores = {},
  upstream_pools = {
    {
      name = "web",
      endpoints = { { type = "socket", address = "127.0.0.1:3000" } },
      health_check = {
        type = "http",
        interval_ms = 5000,
        timeout_ms = 1000,
        healthy_threshold = 1,
        unhealthy_threshold = 3,
        host = "127.0.0.1:3000",
        path = "/healthz",
      },
    },
    {
      name = "postgres",
      endpoints = { { type = "socket", address = "127.0.0.1:5432" } },
    },
  },
  http_services = {
    {
      name = "web",
      routes = {
        {
          path = { kind = "segment_prefix", value = "/" },
          methods = {},
          access_policy = nil,
          action = {
            type = "proxy",
            upstream_pool = "web",
            policy = {
              upstream_host = { type = "preserve_incoming" },
              upstream_path_rewrite = nil,
              request_headers = nil,
              response_headers = nil,
              response_cookie_path_rewrites = nil,
              retry = {
                max_retries = 0,
                triggers = { "connect_failure", "connect_timeout", "refused_stream" },
                method_safety = "get_head",
                body_safety = "empty",
              },
              cache = nil,
            },
          },
        },
      },
    },
  },
  forward_proxy_services = {},
  rtmp_services = {
    {
      name = "live",
      applications = {
        {
          name = "live",
          live = true,
          idle_streams = true,
          recorders = {},
        },
      },
    },
  },
  l4_services = {
    {
      name = "postgres",
      upstream_pool = "postgres",
    },
  },
}
