#![allow(dead_code)]

use std::{
    net::{Ipv4Addr, SocketAddr},
    path::Path,
};

use oxiroute_config::{Config, ListenerBind, RtmpRecorder, RtmpRecorderStart, UpstreamEndpoint};

pub fn empty_config() -> Config {
    Config {
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
