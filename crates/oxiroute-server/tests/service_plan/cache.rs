use oxiroute_config::{
    CacheAuthorizationPolicy, CachePurgeAuthorization, CacheStore, CacheSurrogateTags, ConfigDraft,
    HttpRouteAction,
};
use oxiroute_server::ServicePlanError;
use tempfile::TempDir;

use crate::config_support::{load_lua, runtime_plan};
use crate::fixture_support::write_secure_token;

#[test]
fn compiles_an_active_memory_cache_policy() {
    let config = active_memory_cache_config();
    runtime_plan(&config.clone().validate().unwrap()).expect("active memory cache runtime");
}

#[test]
fn compiles_an_active_persistent_cache_policy() {
    let temp = TempDir::new().expect("cache root parent");
    let mut config = active_memory_cache_config();
    config.cache_stores[0] = CacheStore::Disk {
        name: "memory".into(),
        root_directory: temp.path().join("cache"),
        max_bytes: 1_048_576,
        max_files: 128,
        max_object_bytes: 65_536,
        max_header_bytes: 8_192,
        max_key_bytes: 4_096,
        max_tag_bytes: 256,
        max_tags_per_object: 64,
        max_in_flight_fills: 16,
        max_followers_per_fill: 16,
    };

    let first =
        runtime_plan(&config.clone().validate().unwrap()).expect("active disk cache runtime");
    runtime_plan(&config.clone().validate().unwrap())
        .expect("disk cache survives overlapping generation preparation");
    drop(first);
}

#[test]
fn compiles_surrogate_tag_and_authenticated_purge_cache_policy() {
    let temp = TempDir::new().expect("purge token parent");
    let token_file = write_secure_token(temp.path(), "purge.token", &"x".repeat(32));
    let mut config = active_memory_cache_config();
    let route = &mut config.http_services[0].routes[0];
    route.methods = vec!["GET".into(), "HEAD".into(), "PURGE".into()];
    let HttpRouteAction::Proxy { policy, .. } = &mut route.action else {
        panic!("active cache route is not a proxy");
    };
    let cache = policy.cache.as_mut().expect("cache policy");
    cache.surrogate_tags = Some(CacheSurrogateTags {
        response_header: "Surrogate-Key".into(),
        max_tags: 4,
        max_tag_bytes: 64,
    });
    cache.purge_authorization = Some(CachePurgeAuthorization::BearerTokenFile {
        token_file_path: token_file,
    });

    runtime_plan(&config.clone().validate().unwrap()).expect("tagged purge cache runtime");
}

#[test]
fn rejects_advanced_authorized_cache_reuse_at_runtime() {
    let mut config = active_memory_cache_config();
    let HttpRouteAction::Proxy { policy, .. } = &mut config.http_services[0].routes[0].action
    else {
        panic!("active cache route is not a proxy");
    };
    policy
        .cache
        .as_mut()
        .expect("cache policy")
        .authorization_policy = CacheAuthorizationPolicy::Cache;

    assert!(matches!(
        runtime_plan(&config.clone().validate().unwrap()),
        Err(ServicePlanError::RuntimePolicyUnavailable {
            policy: "http_services[].routes[].action.policy.cache.authorization_policy"
        })
    ));
}

fn active_memory_cache_config() -> ConfigDraft {
    load_lua(
        r#"
return {
  version = 1,
  listeners = {
    {
      name = "web",
      bind = { type = "socket", address = "127.0.0.1:8080" },
      protocol = "http",
      service = "web",
    },
  },
  cache_stores = {
    {
      name = "memory",
      type = "memory",
      max_bytes = 1048576,
      max_entries = 128,
      max_object_bytes = 65536,
    },
  },
  upstream_pools = {
    {
      name = "origin",
      endpoints = { { type = "socket", address = "127.0.0.1:3000" } },
    },
  },
  http_services = {
    {
      name = "web",
      routes = {
        {
          path = { kind = "segment_prefix", value = "/" },
          action = {
            type = "proxy",
            upstream_pool = "origin",
            policy = { cache = { store = "memory" } },
          },
        },
      },
    },
  },
}

"#,
    )
    .expect("canonical cache configuration")
    .to_draft()
}
