//! Helpers shared by the HTTP route configuration tests.
#![allow(dead_code)]

use super::lua_support::load_lua;

pub fn config(routes: &str, endpoint: &str) -> String {
    format!(
        r#"return {{
  version = 1,
  listeners = {{}},
  upstream_pools = {{
    {{ name = "web", endpoints = {{ {endpoint} }} }},
  }},
  http_services = {{
    {{
      name = "web",
      routes = {{
{routes}
      }},
    }},
  }},
}}"#
    )
}

pub fn proxy_route(fields: &str, policy: &str) -> String {
    format!(
        r#"        {{
          path = {{ kind = "segment_prefix", value = "/" }},
{fields}
          action = {{
            type = "proxy",
            upstream_pool = "web",
            policy = {{
{policy}
            }},
          }},
        }},"#
    )
}

pub fn error(source: &str) -> String {
    oxiroute_config_source::load_lua(source)
        .expect_err("configuration must be rejected")
        .to_string()
}

pub fn first_route(source: &str) -> serde_json::Value {
    let config = load_lua(source).expect("HTTP route configuration");
    serde_json::to_value(config).expect("serialized configuration")["http_services"][0]["routes"][0]
        .clone()
}
