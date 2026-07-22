return {
  version = 1,
  management = {
    bind = "127.0.0.1:9080",
    ui_dir = "./ui/dist",
  },
  listeners = {
    {
      name = "web",
      bind = "127.0.0.1:8080",
      protocol = "http",
      service = "web",
    },
    {
      name = "postgres",
      bind = "127.0.0.1:15432",
      protocol = "tcp",
      service = "postgres",
    },
    {
      name = "live",
      bind = "127.0.0.1:1935",
      protocol = "rtmp",
    },
  },
  upstream_pools = {
    {
      name = "web",
      endpoints = { "127.0.0.1:3000" },
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
      endpoints = { "127.0.0.1:5432" },
    },
  },
  http_services = {
    {
      name = "web",
      routes = {
        { path_prefix = "/", upstream_pool = "web" },
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
