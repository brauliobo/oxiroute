return {
  version = 1,
  management = {
    bind = "127.0.0.1:9080",
    ui_dir = "./ui/dist",
  },
  certificates = {
    {
      name = "forward-proxy",
      dns_names = { "proxy.example.test" },
      source = {
        type = "files",
        certificate_chain_path = "/etc/oxiroute/forward-proxy-chain.pem",
        private_key_path = "/etc/oxiroute/forward-proxy-key.pem",
      },
    },
  },
  tls_profiles = {
    {
      name = "forward-http3",
      certificates = { "forward-proxy" },
      default_certificate = "forward-proxy",
      min_version = "1.3",
      alpn = { "h3" },
    },
  },
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
    {
      name = "forward-http1",
      bind = { type = "socket", address = "127.0.0.1:3128" },
      protocol = "forward_http1",
      service = "egress",
    },
    {
      name = "forward-http2",
      bind = { type = "socket", address = "127.0.0.1:3129" },
      protocol = "forward_http2",
      service = "egress",
    },
    {
      name = "forward-http3",
      bind = { type = "udp", address = "127.0.0.1:8443" },
      protocol = "forward_http3",
      service = "egress",
      tls_profile = "forward-http3",
    },
  },
  cache_stores = {
    {
      name = "web-memory",
      type = "memory",
      max_bytes = 268435456,
      max_entries = 100000,
      max_object_bytes = 16777216,
    },
  },
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
              request_headers = nil,
              response_headers = nil,
              response_cookie_path_rewrites = nil,
              retry = {
                max_retries = 0,
                triggers = { "connect_failure", "connect_timeout", "refused_stream" },
                method_safety = "get_head",
                body_safety = "empty",
              },
              cache = {
                store = "web-memory",
                methods = { "GET", "HEAD" },
                key_components = {
                  { type = "scheme" },
                  { type = "normalized_host" },
                  { type = "path_and_query" },
                },
                default_ttl_ms = 60000,
                grace_ms = 30000,
                keep_ms = 300000,
                stale_on = { "connect_failure", "connect_timeout", "origin_503" },
                purge_authorization = {
                  type = "bearer_token_file",
                  token_file_path = "/run/secrets/cache-purge-token",
                },
              },
            },
          },
        },
      },
    },
  },
  forward_proxy_services = {
    {
      name = "egress",
      enabled_versions = { "h1", "h2", "h3" },
      tls_required = false,
      connect = {
        enabled = true,
        allowed_ports = { 443, 8443 },
      },
      auth = {
        type = "bearer_token_file",
        token_file_path = "/run/secrets/forward-proxy-token",
      },
      destination_policy = {
        allow_domains = { "example.com", "*.example.net" },
        deny_private = true,
      },
      resolver = {
        max_cache_entries = 4096,
        max_concurrent_queries = 256,
        max_addresses_per_name = 16,
        min_ttl_ms = 1000,
        max_ttl_ms = 300000,
        negative_ttl_ms = 30000,
        revalidate_on_connect = true,
      },
      audit_mode = "metadata",
    },
  },
  rtmp_services = {
    {
      name = "live",
      applications = {
        {
          name = "live",
          live = true,
          idle_streams = true,
          recorders = {
            {
              name = "archive",
              start = "continuous",
              root_directory = "/var/lib/oxiroute/recordings",
              suffix_template = "-%Y-%m-%dT%H-%M-%S.flv",
              append_unix_seconds = false,
              rotation_interval_ms = null,
              max_queue_messages = 256,
              max_queue_bytes = 8388608,
              shutdown_timeout_ms = 5000,
              max_storage_bytes = 10737418240,
              max_storage_files = 10000,
              max_active_recorders = 8,
            },
          },
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
