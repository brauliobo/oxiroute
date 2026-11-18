mod edge;
mod pools;
mod services;

pub(crate) use edge::{test_certificates, test_listeners, test_tls_profiles};
pub(crate) use pools::{socket, test_retry_policy, test_upstream_pools};
pub(crate) use services::{test_http_services, test_l4_services, test_rtmp_services};

use std::path::PathBuf;

use crate::lua_support::{load_lua, render_lua};
use oxiroute_config::{ConfigDraft, Management};

pub(crate) const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

pub(crate) fn complete_config() -> ConfigDraft {
    ConfigDraft {
        listener_workers: None,
        http_fast_lane: None,
        http_data_plane: None,
        connection_scheduling: None,
        connection_age_ms: None,
        worker_scaling: None,
        min_active_workers: None,
        target_busy_percent: None,
        scale_down_delay_ms: None,
        tls_handshake_threads: None,
        http3_congestion_control: None,
        http3_segmentation_offload: None,
        retirement: None,
        version: 1,
        max_connections: Some(MAX_SAFE_INTEGER),
        management: Some(Management {
            bind: socket("127.0.0.1:9080"),
            ui_dir: Some(PathBuf::from("./ui/dist")),
            token_file: None,
        }),
        registry: None,
        stats: None,
        certificates: test_certificates(),
        tls_profiles: test_tls_profiles(),
        listeners: test_listeners(),
        cache_stores: Vec::new(),
        upstream_pools: test_upstream_pools(),
        http_services: test_http_services(),
        forward_proxy_services: Vec::new(),
        rtmp_services: test_rtmp_services(),
        l4_services: test_l4_services(),
        audit: None,
        config_history: None,
        log_filter: None,
    }
}

pub(crate) fn minimal_config() -> ConfigDraft {
    ConfigDraft {
        listener_workers: None,
        http_fast_lane: None,
        http_data_plane: None,
        connection_scheduling: None,
        connection_age_ms: None,
        worker_scaling: None,
        min_active_workers: None,
        target_busy_percent: None,
        scale_down_delay_ms: None,
        tls_handshake_threads: None,
        http3_congestion_control: None,
        http3_segmentation_offload: None,
        retirement: None,
        version: 1,
        max_connections: None,
        management: None,
        registry: None,
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
        audit: None,
        config_history: None,
        log_filter: None,
    }
}

pub(crate) fn normalized_round_trip(config: ConfigDraft) -> (ConfigDraft, String) {
    let config = config.validate().expect("valid test configuration");
    let config = config.to_draft();
    let source = render_lua(&config).expect("rendered configuration");
    let loaded = load_lua(&source).expect("rendered Lua must load");
    assert_eq!(loaded, config);
    (config, source)
}
