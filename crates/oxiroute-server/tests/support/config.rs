#![allow(dead_code)]

use std::{
    net::{Ipv4Addr, SocketAddr},
    path::Path,
    sync::Arc,
};

use oxiroute_config::{
    ConfigDraft, ListenerBind, RtmpRecorder, RtmpRecorderStart, UpstreamEndpoint, ValidatedConfig,
};
use oxiroute_server::{
    GenerationManager, PassiveFailurePolicy, RuntimeGeneration, RuntimePlan, ServicePlanError,
    ServiceSpec,
    config_coordinator::{AuthoredRevision, EffectiveRevision, ResolvedConfigDocument},
};

pub fn load_lua(source: &str) -> Result<ValidatedConfig, oxiroute_config_source::LuaConfigError> {
    oxiroute_config_source::load_lua(source)
}

pub fn render_lua(config: &ValidatedConfig) -> Result<String, String> {
    oxiroute_config_source::render_config(oxiroute_config_source::ConfigFormat::Lua, config)
        .map_err(|error| error.to_string())
}

pub fn runtime_plan(config: &ValidatedConfig) -> Result<RuntimePlan, ServicePlanError> {
    oxiroute_server::validate_runtime_plan(config)
}

pub fn runtime_plan_with_passive_failure_policy(
    config: &ValidatedConfig,
    passive_policy: PassiveFailurePolicy,
) -> Result<RuntimePlan, ServicePlanError> {
    oxiroute_server::runtime_plan_with_passive_failure_policy(config, passive_policy)
}

pub fn service_specs(config: &ValidatedConfig) -> Result<Vec<ServiceSpec>, ServicePlanError> {
    oxiroute_server::service_specs(config)
}

pub fn runtime_generation(
    config: &ValidatedConfig,
) -> Result<Arc<RuntimeGeneration>, oxiroute_server::GenerationError> {
    let revision = "0000000000000000000000000000000000000000000000000000000000000000";
    let manager = GenerationManager::new();
    let candidate = manager.prepare(ResolvedConfigDocument {
        authored_revision: revision.parse::<AuthoredRevision>().unwrap(),
        effective_revision: revision.parse::<EffectiveRevision>().unwrap(),
        validated_config: config.clone(),
        format: oxiroute_config_source::ConfigFormat::Lua,
        compositional: false,
        dependencies: Vec::new(),
        config_preview: String::new(),
        diagnostics: Vec::new(),
    })?;
    let mut startup = manager.begin_candidate_start(&candidate)?;
    let generation = startup.claim_runtime_start()?;
    assert!(generation.mark_runtime_started());
    startup.activate()
}

pub fn empty_config() -> ConfigDraft {
    ConfigDraft {
        version: 1,
        max_connections: None,
        management: None,
        stats: None,
        certificates: Vec::new(),
        tls_profiles: Vec::new(),
        listeners: Vec::new(),
        cache_stores: Vec::new(),
        upstream_pools: Vec::new(),
        http_services: Vec::new(),
        forward_proxy_services: Vec::new(),
        rtmp_services: Vec::new(),
        l4_services: Vec::new(),
    }
}

pub fn loopback_address(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

pub fn socket_bind(address: SocketAddr) -> ListenerBind {
    ListenerBind::Socket { address }
}

pub fn loopback_bind(port: u16) -> ListenerBind {
    socket_bind(loopback_address(port))
}

pub fn parsed_socket_bind(address: &str) -> ListenerBind {
    socket_bind(address.parse().expect("socket listener bind"))
}

pub fn socket_endpoint(address: SocketAddr) -> UpstreamEndpoint {
    UpstreamEndpoint::Socket { address }
}

pub fn loopback_endpoint(port: u16) -> UpstreamEndpoint {
    socket_endpoint(loopback_address(port))
}

pub fn parsed_socket_endpoint(address: &str) -> UpstreamEndpoint {
    socket_endpoint(address.parse().expect("socket upstream endpoint"))
}

pub fn rtmp_recorder(name: &str, start: RtmpRecorderStart, root_directory: &Path) -> RtmpRecorder {
    RtmpRecorder {
        name: name.into(),
        start,
        root_directory: root_directory.to_path_buf(),
        record_mask: oxiroute_config::RtmpRecordMask::default(),
        suffix_template: ".flv".into(),
        append_unix_seconds: false,
        append: false,
        lock: false,
        max_size: None,
        max_frames: None,
        notify: false,
        timezone: oxiroute_config::RtmpRecorderTimezone::default(),
        time_basis: oxiroute_config::RtmpRecorderTimeBasis::default(),
        segment_naming: oxiroute_config::RtmpRecorderSegmentNaming::default(),
        rotation_interval_ms: None,
        max_queue_messages: 32,
        max_queue_bytes: 1024,
        shutdown_timeout_ms: 1_000,
        max_storage_bytes: Some(1024 * 1024),
        max_storage_files: Some(32),
        max_active_recorders: 4,
    }
}

pub fn rtmp_recorder_with_queue_bytes(
    name: &str,
    start: RtmpRecorderStart,
    root_directory: &Path,
    max_queue_bytes: u64,
) -> RtmpRecorder {
    RtmpRecorder {
        max_queue_bytes,
        ..rtmp_recorder(name, start, root_directory)
    }
}
