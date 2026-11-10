//! The process parses the system CA bundle once, however many health checks the config defines.
//! Runs alone in its process so `/proc/self/io` only sees this test's reads.
#[path = "support/config.rs"]
mod config_support;

use oxiroute_config::{
    ConfigDraft, HealthCheck, HealthCheckType, HttpVersionPolicy, UpstreamAlgorithm, UpstreamPool,
};

use config_support::{empty_config, loopback_endpoint, runtime_plan};

const POOLS: usize = 12;

fn bytes_read() -> u64 {
    std::fs::read_to_string("/proc/self/io")
        .expect("process io accounting")
        .lines()
        .find_map(|line| line.strip_prefix("rchar: "))
        .expect("rchar line")
        .parse()
        .expect("rchar value")
}

fn http_health_pool(index: usize) -> UpstreamPool {
    UpstreamPool {
        name: format!("pool-{index}"),
        servers: Vec::new(),
        endpoints: vec![loopback_endpoint(20_000 + u16::try_from(index).unwrap())],
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: Some(HealthCheck {
            kind: HealthCheckType::Http,
            interval_ms: 5_000,
            timeout_ms: 1_000,
            healthy_threshold: 1,
            unhealthy_threshold: 1,
            startup: oxiroute_config::HealthStartup::default(),
            fast_interval_ms: None,
            down_interval_ms: None,
            host: Some("backend.internal".into()),
            path: Some("/healthz".into()),
            expected_status: None,
            http_version: None,
        }),
        passive_health: None,
        tls: None,
        http_versions: HttpVersionPolicy::default(),
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
        registration: None,
        canary: None,
        resilience: oxiroute_config::PoolResilience::default(),
        queue: None,
    }
}

/// Runs in a child process that sees `SSL_CERT_FILE` from its start and no other test's reads.
#[test]
#[ignore = "run by health_checked_pools_share_one_system_ca_parse"]
fn child_counts_ca_bundle_parses() {
    let bundle_bytes = std::fs::metadata(std::env::var_os("SSL_CERT_FILE").unwrap())
        .expect("bundle size")
        .len();
    let draft = ConfigDraft {
        listener_workers: None,
        connection_scheduling: None,
        connection_age_ms: None,
        retirement: None,
        upstream_pools: (0..POOLS).map(http_health_pool).collect(),
        ..empty_config()
    };
    let config = draft.validate().expect("valid config");
    let before = bytes_read();
    runtime_plan(&config).expect("runtime plan");
    let parses = (bytes_read() - before) / bundle_bytes;

    assert!(
        parses <= 1,
        "the CA bundle was parsed {parses} times for {POOLS} pools"
    );
}

#[test]
fn health_checked_pools_share_one_system_ca_parse() {
    let bundle = tempfile::NamedTempFile::new().expect("bundle file");
    let ca = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/ca-a.pem"
    ))
    .expect("CA fixture");
    std::fs::write(bundle.path(), ca.repeat(1500)).expect("write bundle");
    let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args(["--ignored", "--exact", "child_counts_ca_bundle_parses"])
        .env("SSL_CERT_FILE", bundle.path())
        .output()
        .expect("child test");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
