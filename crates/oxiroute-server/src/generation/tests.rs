use std::{
    fs,
    sync::{
        Arc, Barrier,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
};

use oxiroute_config::{ConfigDraft, Listener, ListenerBind, Management, Protocol, Stats};
use tempfile::TempDir;

use crate::{
    config::coordinator::{CanonicalConfigCoordinator, ConfigLoadOutcome},
    test_ports::unused_tcp_address,
};

use super::*;

fn document() -> ResolvedConfigDocument {
    document_with_max_connections(None)
}

fn disk_cache_document(root: &std::path::Path) -> ResolvedConfigDocument {
    let source = format!(
        r#"
return {{
  version = 1,
  listeners = {{}},
  cache_stores = {{
    {{
      name = "disk",
      type = "disk",
      root_directory = "{}",
      max_bytes = 1048576,
      max_files = 128,
      max_object_bytes = 65536,
    }},
  }},
  upstream_pools = {{
    {{
      name = "origin",
      endpoints = {{ {{ type = "socket", address = "127.0.0.1:3000" }} }},
    }},
  }},
  http_services = {{
    {{
      name = "web",
      routes = {{
        {{
          path = {{ kind = "segment_prefix", value = "/" }},
          action = {{
            type = "proxy",
            upstream_pool = "origin",
            policy = {{ cache = {{ store = "disk" }} }},
          }},
        }},
      }},
    }},
  }},
}}
"#,
        root.display()
    );
    let config = oxiroute_config_source::load_lua(&source)
        .expect("disk cache configuration")
        .to_draft();
    document_for(&config)
}

fn document_with_max_connections(max_connections: Option<u64>) -> ResolvedConfigDocument {
    document_for(&ConfigDraft {
        listener_workers: None,
        http_fast_lane: None,
        connection_scheduling: None,
        connection_age_ms: None,
        worker_scaling: None,
        min_active_workers: None,
        target_busy_percent: None,
        scale_down_delay_ms: None,
        tls_handshake_threads: None,
        retirement: None,
        version: 1,
        max_connections,
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
        log_filter: None,
    })
}

fn document_for(config: &ConfigDraft) -> ResolvedConfigDocument {
    let directory = TempDir::new().expect("directory");
    let path = directory.path().join("oxiroute.lua");
    let config = config.clone().validate().expect("valid config");
    fs::write(
        &path,
        oxiroute_config_source::render_config(oxiroute_config_source::ConfigFormat::Lua, &config)
            .expect("render"),
    )
    .expect("write");
    let ConfigLoadOutcome::Loaded(document) = CanonicalConfigCoordinator::new(path)
        .expect("coordinator")
        .load()
    else {
        panic!("load")
    };
    *document
}

fn colliding_listener_document() -> ResolvedConfigDocument {
    let traffic_management = unused_tcp_address();
    let traffic_stats = unused_tcp_address();
    let management = unused_tcp_address();
    let stats = unused_tcp_address();
    let mut config: ConfigDraft = serde_json::from_value(serde_json::json!({
        "version": 1,
        "listeners": [],
        "upstream_pools": [{
            "name": "origin",
            "endpoints": [{"type": "socket", "address": "127.0.0.1:9000"}]
        }],
        "l4_services": [{"name": "relay", "upstream_pool": "origin"}]
    }))
    .expect("collision config");
    config.listeners = vec![
        Listener {
            name: "@management".into(),
            bind: ListenerBind::Socket {
                address: traffic_management,
            },
            protocol: Protocol::Tcp,
            service: Some("relay".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: None,
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        },
        Listener {
            name: "@stats-0".into(),
            bind: ListenerBind::Socket {
                address: traffic_stats,
            },
            protocol: Protocol::Tcp,
            service: Some("relay".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: None,
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        },
    ];
    config.management = Some(Management {
        bind: management,
        ui_dir: None,
        token_file: None,
    });
    config.stats = Some(Stats {
        binds: vec![stats],
        admin_token_file: None,
        pages: Vec::new(),
    });
    document_for(&config)
}

fn rtmp_failure_document(root: &std::path::Path, auto_push: bool) -> ResolvedConfigDocument {
    let listener = unused_tcp_address();
    let mut config: ConfigDraft = serde_json::from_value(serde_json::json!({
        "version": 1,
        "listeners": [{
            "name": "ingest",
            "bind": {"type": "socket", "address": listener},
            "protocol": "rtmp",
            "service": "live"
        }],
        "rtmp_services": [{
            "name": "live",
            "outbound_chunk_size": 4096,
            "max_inbound_message_size": 8_388_608,
            "ack_window_size": 5_000_000,
            "applications": [{
                "name": "live",
                "live": true,
                "recorders": [{"name": "archive", "root_directory": root}]
            }]
        }]
    }))
    .expect("RTMP failure config");
    if auto_push {
        config.rtmp_services[0].auto_push.enabled = true;
        config.rtmp_services[0].auto_push.socket_dir = root.join("auto-push");
    }
    document_for(&config)
}

#[cfg(target_os = "linux")]
fn adopted_rtmp_fixture(
    root: &std::path::Path,
    address: std::net::SocketAddr,
    listener: std::net::TcpListener,
    auto_push: bool,
) -> (ResolvedConfigDocument, DescriptorSet) {
    use std::os::fd::OwnedFd;

    use oxiroute_supervision_unix::{
        BindIdentity, DescriptorKind, DescriptorManifest, DescriptorRole, DescriptorSlot, SlotId,
    };

    let document = rtmp_failure_document_for(root, address, auto_push);
    let manifest = DescriptorManifest::new(vec![DescriptorSlot {
        id: SlotId(0),
        role: DescriptorRole::Traffic("ingest".into()),
        kind: DescriptorKind::TcpListener,
        bind: Some(BindIdentity::Tcp(address)),
        mode: None,
    }])
    .expect("RTMP descriptor manifest");
    let descriptors =
        DescriptorSet::new(&manifest, vec![OwnedFd::from(listener)]).expect("RTMP descriptor set");
    (document, descriptors)
}

#[cfg(target_os = "linux")]
fn adopted_planning_failure_fixture(
    address: std::net::SocketAddr,
    listener: std::net::TcpListener,
) -> (ResolvedConfigDocument, DescriptorSet) {
    use std::os::fd::OwnedFd;

    use oxiroute_supervision_unix::{
        BindIdentity, DescriptorKind, DescriptorManifest, DescriptorRole, DescriptorSlot, SlotId,
    };

    let config: ConfigDraft = serde_json::from_value(serde_json::json!({
        "version": 1,
        "listeners": [{
            "name": "web",
            "bind": {"type": "socket", "address": address},
            "protocol": "http",
            "service": "web"
        }],
        "http_services": [{
            "name": "web",
            "routes": [{
                "path": {"kind": "segment_prefix", "value": "/"},
                "policy": {
                    "request_buffering": true,
                    "max_request_body_bytes": null
                },
                "action": {"type": "fixed_response", "status": 200}
            }]
        }]
    }))
    .expect("planning-failure config");
    let manifest = DescriptorManifest::new(vec![DescriptorSlot {
        id: SlotId(0),
        role: DescriptorRole::Traffic("web".into()),
        kind: DescriptorKind::TcpListener,
        bind: Some(BindIdentity::Tcp(address)),
        mode: None,
    }])
    .expect("planning-failure descriptor manifest");
    let descriptors = DescriptorSet::new(&manifest, vec![OwnedFd::from(listener)])
        .expect("planning-failure descriptor set");
    (document_for(&config), descriptors)
}

#[cfg(target_os = "linux")]
fn rtmp_failure_document_for(
    root: &std::path::Path,
    address: std::net::SocketAddr,
    auto_push: bool,
) -> ResolvedConfigDocument {
    let mut config: ConfigDraft = serde_json::from_value(serde_json::json!({
        "version": 1,
        "listeners": [{
            "name": "ingest",
            "bind": {"type": "socket", "address": address},
            "protocol": "rtmp",
            "service": "live"
        }],
        "rtmp_services": [{
            "name": "live",
            "outbound_chunk_size": 4096,
            "max_inbound_message_size": 8_388_608,
            "ack_window_size": 5_000_000,
            "applications": [{
                "name": "live",
                "live": true,
                "recorders": [{"name": "archive", "root_directory": root}]
            }]
        }]
    }))
    .expect("adopted RTMP config");
    if auto_push {
        config.rtmp_services[0].auto_push.enabled = true;
        config.rtmp_services[0].auto_push.socket_dir = root.join("auto-push");
    }
    document_for(&config)
}

fn claim_and_mark_runtime_started(startup: &mut GenerationStartup) -> Arc<RuntimeGeneration> {
    let generation = startup.claim_runtime_start().expect("runtime start claim");
    assert!(generation.mark_runtime_started());
    generation
}

fn pause_activation_at(
    manager: &GenerationManager,
    point: ActivationHookPoint,
) -> (mpsc::Receiver<()>, Arc<Barrier>) {
    let (paused, paused_rx) = mpsc::sync_channel(1);
    let release = Arc::new(Barrier::new(2));
    let hook_release = Arc::clone(&release);
    let notified = Arc::new(AtomicBool::new(false));
    manager.set_activation_hook(Arc::new(move |observed| {
        if observed == point && !notified.swap(true, Ordering::AcqRel) {
            paused.send(()).expect("activation pause receiver");
            hook_release.wait();
        }
    }));
    (paused_rx, release)
}

mod activation;
mod deadline;
mod listener;
mod publication;
mod rtmp;
mod shutdown;
mod startup;
mod transaction;
