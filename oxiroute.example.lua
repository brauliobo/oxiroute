return {
  version = 1,
  listeners = {
    {
      name = "web",
      bind = "127.0.0.1:8080",
      protocol = "http",
      upstream = "127.0.0.1:3000",
    },
    {
      name = "postgres",
      bind = "127.0.0.1:15432",
      protocol = "tcp",
      upstream = "127.0.0.1:5432",
    },
  },
}
