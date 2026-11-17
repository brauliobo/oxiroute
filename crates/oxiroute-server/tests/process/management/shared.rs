use std::path::Path;

use oxiroute_config::{
    ConfigDraft, DnsResolutionPolicy, HttpVersionPolicy, Listener, ListenerBind, Management,
    Protocol, RtmpApplication, RtmpRecorderStart, RtmpService, UpstreamAlgorithm,
    UpstreamConnectionReuse, UpstreamEndpoint, UpstreamPool, UpstreamServer,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
};

use crate::config_support::{empty_config, rtmp_recorder_with_queue_bytes, socket_bind};
use crate::http_support::{self};
use crate::process_support::reserve_tcp_address;

pub(super) fn public_pool() -> UpstreamPool {
    UpstreamPool {
        name: "public".into(),
        servers: vec![UpstreamServer {
            name: "origin-a".into(),
            endpoint: UpstreamEndpoint::Socket {
                address: "127.0.0.1:8080".parse().expect("upstream"),
            },
            max_connections: None,
            dns_resolution: DnsResolutionPolicy::OnConnect,
        }],
        endpoints: Vec::new(),
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: None,
        passive_health: None,
        tls: None,
        http_versions: HttpVersionPolicy::default(),
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: UpstreamConnectionReuse::Safe,
        registration: None,
        canary: None,
        resilience: oxiroute_config::PoolResilience::default(),
        queue: None,
    }
}

pub(super) const TOKEN: &str = "cdb85a91948758cfcb895216a3603c8fcd8aaf691f39f5fd82b5df15af14628e";

pub(super) fn management_config(
    management_address: std::net::SocketAddr,
    ui_dir: Option<std::path::PathBuf>,
) -> ConfigDraft {
    ConfigDraft {
        listener_workers: None,
        http_fast_lane: None,
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
        management: Some(Management {
            bind: management_address,
            ui_dir,
            token_file: None,
        }),
        registry: None,
        stats: None,
        ..empty_config()
    }
}

pub(super) fn recording_candidate(active: &ConfigDraft, root: &Path) -> ConfigDraft {
    let mut candidate = active.clone();
    candidate.listeners.push(Listener {
        name: "recording-wire".into(),
        bind: socket_bind(reserve_tcp_address()),
        protocol: Protocol::Rtmp,
        service: Some("recording-wire".into()),
        tls_profile: None,
        proxy_protocol: None,
        max_connections: Some(8),
        downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        h2_initial_stream_window: None,
        h2_initial_connection_window: None,
    });
    candidate.rtmp_services.push(RtmpService {
        name: "recording-wire".into(),
        outbound_chunk_size: 4_096,
        max_inbound_message_size: 8 * 1024 * 1024,
        ack_window_size: 5_000_000,
        access_log: None,
        outbound_policy: oxiroute_config::RtmpOutboundPolicy::default(),
        callbacks: oxiroute_config::RtmpCallbackConfig::default(),
        auto_push: oxiroute_config::RtmpAutoPushPolicy::default(),
        exec_profiles: Vec::new(),
        applications: vec![RtmpApplication {
            name: "live".into(),
            live: true,
            idle_streams: false,
            publish: oxiroute_config::RtmpAccessPolicy::default(),
            play: oxiroute_config::RtmpAccessPolicy::default(),
            limits: oxiroute_config::RtmpSessionCeilings::default(),
            push_targets: Vec::new(),
            pull_targets: Vec::new(),
            relay: oxiroute_config::RtmpRelayPolicy::default(),
            callbacks: oxiroute_config::RtmpCallbackConfig::default(),
            fanout: oxiroute_config::RtmpFanoutPolicy::default(),
            vod: None,
            hls: None,
            dash: None,
            recorders: vec![rtmp_recorder_with_queue_bytes(
                "archive",
                RtmpRecorderStart::Continuous,
                root,
                1024 * 1024,
            )],
        }],
    });
    candidate
}

pub(super) fn rtmp_listener_config(bind: ListenerBind) -> ConfigDraft {
    ConfigDraft {
        listener_workers: None,
        http_fast_lane: None,
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
        listeners: vec![Listener {
            name: "live".into(),
            bind,
            protocol: Protocol::Rtmp,
            service: Some("live".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: Some(8),
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
            h2_initial_stream_window: None,
            h2_initial_connection_window: None,
        }],
        rtmp_services: vec![RtmpService {
            name: "live".into(),
            outbound_chunk_size: 4_096,
            max_inbound_message_size: 8 * 1024 * 1024,
            ack_window_size: 5_000_000,
            access_log: None,
            outbound_policy: oxiroute_config::RtmpOutboundPolicy::default(),
            callbacks: oxiroute_config::RtmpCallbackConfig::default(),
            auto_push: oxiroute_config::RtmpAutoPushPolicy::default(),
            exec_profiles: Vec::new(),
            applications: vec![RtmpApplication {
                name: "live".into(),
                live: true,
                idle_streams: false,
                publish: oxiroute_config::RtmpAccessPolicy::default(),
                play: oxiroute_config::RtmpAccessPolicy::default(),
                limits: oxiroute_config::RtmpSessionCeilings::default(),
                push_targets: Vec::new(),
                pull_targets: Vec::new(),
                relay: oxiroute_config::RtmpRelayPolicy::default(),
                callbacks: oxiroute_config::RtmpCallbackConfig::default(),
                fanout: oxiroute_config::RtmpFanoutPolicy::default(),
                vod: None,
                hls: None,
                dash: None,
                recorders: Vec::new(),
            }],
        }],
        ..empty_config()
    }
}

pub(super) async fn persistent_request(
    stream: &mut TcpStream,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> http_support::HttpResponse {
    begin_persistent_request(stream, method, path, headers, body.len()).await;
    finish_persistent_request(stream, body).await
}

/// Sends the request head only, so that the request is in flight when the body follows.
pub(super) async fn begin_persistent_request(
    stream: &mut TcpStream,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body_length: usize,
) {
    use std::fmt::Write as _;

    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\n");
    for (name, value) in headers {
        writeln!(request, "{name}: {value}\r").expect("persistent request header");
    }
    if body_length > 0 || matches!(method, "POST" | "PUT") {
        writeln!(request, "Content-Length: {body_length}\r").expect("persistent content length");
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("persistent request head");
}

pub(super) async fn finish_persistent_request(
    stream: &mut TcpStream,
    body: &[u8],
) -> http_support::HttpResponse {
    stream
        .write_all(body)
        .await
        .expect("persistent request body");

    let mut response = Vec::new();
    let mut byte = [0_u8; 1];
    let (header_end, body_length) = loop {
        if let Some(header_end) = response.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&response[..header_end]).expect("response headers");
            let body_length = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            break (header_end + 4, body_length);
        }
        stream
            .read_exact(&mut byte)
            .await
            .expect("persistent response head");
        response.push(byte[0]);
    };
    while response.len() < header_end + body_length {
        stream
            .read_exact(&mut byte)
            .await
            .expect("persistent response body");
        response.push(byte[0]);
    }
    http_support::HttpResponse::parse(response)
}
