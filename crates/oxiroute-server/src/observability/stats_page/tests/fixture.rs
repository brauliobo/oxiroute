//! An active generation with two HTTP routes, a TCP relay and three servers, built from a real
//! configuration file. Pool names are unique to these tests because upstream counters are
//! process-wide.

use std::{fs, sync::Arc};

use oxiroute_config::{DownstreamTimeoutPolicy, StatsPage, StatsPageAdminPolicy};
use tempfile::TempDir;

use crate::{
    GenerationManager, RoundRobinPool, RuntimeGeneration,
    config::coordinator::{CanonicalConfigCoordinator, ConfigLoadOutcome},
    observability::stats_page::HaproxyStatsPage,
    test_ports::unused_tcp_address,
};

pub(super) const WEB: &str = "sp-web";
pub(super) const API: &str = "sp-api";

const CONFIG: &str = r#"
return {
  version = 1,
  listeners = {
    { name = "front", bind = { type = "socket", address = "@WEB_BIND@" }, protocol = "http", service = "site" },
    { name = "db", bind = { type = "socket", address = "@DB_BIND@" }, protocol = "tcp", service = "relay" },
  },
  upstream_pools = {
    {
      name = "sp-web",
      servers = {
        { name = "origin-a", endpoint = { type = "socket", address = "192.0.2.10:3000" } },
        { name = "origin-b", endpoint = { type = "socket", address = "192.0.2.11:3000" }, max_connections = 50 },
      },
      queue_timeout_ms = 2000,
    },
    {
      name = "sp-api",
      servers = { { name = "origin-c", endpoint = { type = "socket", address = "192.0.2.12:3000" } } },
    },
  },
  http_services = {
    {
      name = "site",
      routes = {
        {
          path = { kind = "segment_prefix", value = "/" },
          action = {
            type = "proxy",
            upstream_pool = "sp-web",
            policy = { retry = { max_retries = 2, triggers = {}, response_statuses = { 502 } } },
          },
        },
        {
          host = { kind = "exact_authority", value = "api.example.com" },
          path = { kind = "exact", value = "/v1" },
          methods = { "GET" },
          action = { type = "proxy", upstream_pool = "sp-api", policy = {} },
        },
        {
          path = { kind = "exact", value = "/gone" },
          action = { type = "fixed_response", status = 410 },
        },
      },
    },
  },
  l4_services = { { name = "relay", upstream_pool = "sp-api" } },
}
"#;

pub(super) struct Fixture {
    pub(super) generations: GenerationManager,
    pub(super) generation: Arc<RuntimeGeneration>,
    pub(super) revision: String,
    _directory: TempDir,
}

impl Fixture {
    pub(super) fn pool(&self, name: &str) -> &Arc<RoundRobinPool> {
        self.generation
            .pools()
            .iter()
            .find(|pool| pool.health_snapshot().name == name)
            .expect("fixture pool")
    }
}

pub(super) fn fixture() -> Fixture {
    fixture_from(CONFIG)
}

pub(super) fn fixture_from(source: &str) -> Fixture {
    let directory = TempDir::new().expect("directory");
    let path = directory.path().join("oxiroute.lua");
    let source = source
        .replace("@WEB_BIND@", &unused_tcp_address().to_string())
        .replace("@DB_BIND@", &unused_tcp_address().to_string());
    fs::write(&path, source).expect("config");
    let coordinator = CanonicalConfigCoordinator::new(path).expect("coordinator");
    let document = match coordinator.load() {
        ConfigLoadOutcome::Loaded(document) => document,
        ConfigLoadOutcome::Rejected(rejection) => panic!("fixture configuration: {rejection:?}"),
    };
    let generations = GenerationManager::new();
    let candidate = generations.prepare(*document).expect("prepare");
    let generation = generations.activate(&candidate).expect("activate");
    let revision = generation.revision().candidate.as_str().to_owned();
    Fixture {
        generations,
        generation,
        revision,
        _directory: directory,
    }
}

pub(super) fn page_config(refresh_ms: u64, admin: StatsPageAdminPolicy) -> StatsPage {
    StatsPage {
        bind: unused_tcp_address(),
        uri_prefix: "/haproxy".into(),
        refresh_ms,
        admin,
        max_connections: None,
        downstream_timeouts: DownstreamTimeoutPolicy::default(),
    }
}

pub(super) fn page(fixture: &Fixture, admin: StatsPageAdminPolicy) -> HaproxyStatsPage {
    HaproxyStatsPage::new(&page_config(10_000, admin), fixture.generations.clone())
}
