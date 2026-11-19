use crate::fixtures::{
    complete_config, minimal_config, normalized_round_trip, test_upstream_pools,
};
use crate::lua_support::{load_lua, render_lua};
use oxiroute_config::{
    ConfigDraft, ConnectionScheduling, HttpVersion, HttpVersionPolicy, RtmpExecEnvironment,
    RtmpExecFilesystemPolicy, RtmpExecMode, RtmpExecNetworkPolicy, RtmpExecProfile,
    RtmpExecTrigger, WorkerScaling,
};

#[test]
fn renders_and_round_trips_dynamic_worker_scaling() {
    let mut config = minimal_config();
    config.listener_workers = Some(6);
    config.connection_scheduling = Some(ConnectionScheduling::AgedPerWorker);
    config.worker_scaling = Some(WorkerScaling::Dynamic);
    config.min_active_workers = Some(2);
    config.target_busy_percent = Some(65);
    config.scale_down_delay_ms = Some(1_500);

    let (_, source) = normalized_round_trip(config);
    assert!(source.contains("worker_scaling = \"dynamic\""));
    assert!(source.contains("min_active_workers = 2"));
    assert!(source.contains("target_busy_percent = 65"));
    assert!(source.contains("scale_down_delay_ms = 1500"));
}

#[test]
fn renders_and_round_trips_the_exact_http3_upstream_policy() {
    let mut pool = test_upstream_pools().remove(0);
    pool.http_versions = HttpVersionPolicy {
        min: HttpVersion::Http3,
        max: HttpVersion::Http3,
    };
    let mut config = minimal_config();
    config.upstream_pools.push(pool);

    let (_, source) = normalized_round_trip(config);
    assert!(source.contains("min = \"3\""));
    assert!(source.contains("max = \"3\""));
}

#[test]
fn round_trips_rtmp_exec_profiles_as_data_only_lua() {
    let mut config = complete_config();
    config.rtmp_services[0].exec_profiles = vec![RtmpExecProfile {
        name: "capture".into(),
        application: "live".into(),
        mode: RtmpExecMode::Transcode,
        trigger: RtmpExecTrigger::Publisher,
        executable: "/usr/bin/cat".into(),
        arguments: vec!["--raw".into()],
        environment: vec![RtmpExecEnvironment {
            name: "CAPTURE_MODE".into(),
            value: "raw".into(),
        }],
        working_directory: "/var/empty".into(),
        filesystem: RtmpExecFilesystemPolicy::WorkingDirectory,
        network: RtmpExecNetworkPolicy::Inherited,
        timeout_ms: 60_000,
        shutdown_timeout_ms: 5_000,
        max_processes: 2,
        max_queue_messages: 256,
        max_queue_bytes: 8 * 1024 * 1024,
        max_stdout_bytes: 64 * 1024,
        max_stderr_bytes: 64 * 1024,
        respawn: true,
        respawn_delay_ms: 250,
        max_respawns: 3,
    }];
    let config = config.validate().expect("valid exec profile").to_draft();
    let source = render_lua(&config).expect("exec profile render");
    assert!(source.contains("exec_profiles"));
    assert!(source.contains("CAPTURE_MODE"));
    assert_eq!(load_lua(&source).expect("exec profile reload"), config);
}

#[test]
fn automatic_response_headers_defaults_true_and_round_trips_both_values() {
    let mut value = serde_json::to_value(complete_config()).expect("canonical JSON value");
    value["http_services"][0]
        .as_object_mut()
        .expect("HTTP service object")
        .remove("automatic_response_headers");
    let mut config: ConfigDraft = serde_json::from_value(value).expect("omitted header policy");
    assert!(config.http_services[0].automatic_response_headers);

    let default_source = render_lua(&config).expect("default header policy render");
    assert!(default_source.contains("automatic_response_headers = true,"));
    assert!(
        load_lua(&default_source)
            .expect("default header policy reload")
            .http_services[0]
            .automatic_response_headers
    );

    config.http_services[0].automatic_response_headers = false;
    let disabled_source = render_lua(&config).expect("disabled header policy render");
    assert!(disabled_source.contains("automatic_response_headers = false,"));
    assert!(
        !load_lua(&disabled_source)
            .expect("disabled header policy reload")
            .http_services[0]
            .automatic_response_headers
    );
}

#[test]
fn emits_defaults_empty_vectors_and_options_explicitly_as_data_only_lua() {
    let source = render_lua(&minimal_config()).expect("minimal render");

    assert_eq!(
        source,
        r"return {
  version = 1,
  max_connections = null,
  retirement = nil,
  management = nil,
  audit = nil,
  config_history = nil,
  registry = nil,
  stats = nil,
  certificates = {
  },
  tls_profiles = {
  },
  listeners = {
  },
  cache_stores = {
  },
  upstream_pools = {
  },
  http_services = {
  },
  forward_proxy_services = {
  },
  rtmp_services = {
  },
  l4_services = {
  },
}
"
    );
    for unsupported in [
        "function", "local ", "require", "--", "[[", "]]", "(", ")", ";",
    ] {
        assert!(!source.contains(unsupported), "found `{unsupported}`");
    }
    assert_eq!(
        load_lua(&source).expect("strict loader acceptance"),
        minimal_config()
    );
}
