use std::{collections::VecDeque, fs};

use bytes::Bytes;
use oxiroute_config::{
    ConfigDraft, HttpVersionPolicy, L4Service, Listener, ListenerBind, Protocol, RtmpApplication,
    RtmpRecorder, RtmpRecorderStart, RtmpService, UpstreamAlgorithm, UpstreamEndpoint,
    UpstreamPool,
};
use oxiroute_rtmp::RtmpSession;
use rml_rtmp::{
    handshake::{Handshake, HandshakeProcessResult, PeerType},
    sessions::{
        ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult,
        PublishRequestType,
    },
    time::RtmpTimestamp,
};
use rustix::fs::{FlockOperation, flock};
use tokio::{net::TcpListener, sync::watch};

use super::*;

pub(super) struct InjectedRuntimeJoin {
    pub(super) result: io::Result<()>,
}

impl GenerationRuntimeJoin for InjectedRuntimeJoin {
    fn join_runtime(self) -> io::Result<()> {
        self.result
    }
}

pub(super) fn activate_test_generation(
    path: &Path,
    config: &ConfigDraft,
) -> (
    CanonicalConfigCoordinator,
    GenerationManager,
    Arc<RuntimeGeneration>,
) {
    fs::write(
        path,
        oxiroute_config_source::render_config(
            oxiroute_config_source::ConfigFormat::Kdl,
            &config.clone().validate().expect("valid test config"),
        )
        .expect("render"),
    )
    .expect("config");
    let coordinator = CanonicalConfigCoordinator::new(path).expect("coordinator");
    let ConfigLoadOutcome::Loaded(document) = coordinator.load() else {
        panic!("load")
    };
    let manager = GenerationManager::new();
    let candidate = manager.prepare(*document).expect("candidate");
    let mut startup = manager
        .begin_candidate_start(&candidate)
        .expect("startup reservation");
    let generation = startup.claim_runtime_start().expect("runtime start claim");
    assert!(generation.mark_runtime_started());
    startup.activate().expect("active");
    (coordinator, manager, generation)
}

pub(super) fn publish_stalled_recorder(generation: &RuntimeGeneration, service: &str) {
    let mut server = generation
        .rtmp_service(service)
        .expect("RTMP runtime")
        .session();
    let mut client = connect_rtmp_session(&mut server, "live");
    let publish = client
        .request_publishing("camera".into(), PublishRequestType::Live)
        .expect("publish request");
    let events = exchange_rtmp(&mut client, &mut server, vec![publish], 1_000);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ClientSessionEvent::PublishRequestAccepted))
    );
    let audio = client
        .publish_audio_data(
            Bytes::from_static(&[0xaf, 0x01, 0x44]),
            RtmpTimestamp::new(1),
            false,
        )
        .expect("audio packet");
    exchange_rtmp(&mut client, &mut server, vec![audio], 1_001);
    thread::sleep(Duration::from_millis(50));
}

pub(super) fn recorder_runtime_config(
    listener_address: std::net::SocketAddr,
    recording_root: &Path,
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
        retirement: None,
        version: 1,
        max_connections: None,
        management: None,
        registry: None,
        stats: None,
        certificates: Vec::new(),
        tls_profiles: Vec::new(),
        listeners: vec![Listener {
            name: "live".into(),
            bind: ListenerBind::Socket {
                address: listener_address,
                ipv6_only: false,
            },
            protocol: Protocol::Rtmp,
            service: Some("live".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: Some(4),
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
            h2_initial_stream_window: None,
            h2_initial_connection_window: None,
        }],
        cache_stores: Vec::new(),
        upstream_pools: Vec::new(),
        http_services: Vec::new(),
        forward_proxy_services: Vec::new(),
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
                idle_streams: true,
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
                recorders: vec![RtmpRecorder {
                    name: "archive".into(),
                    start: RtmpRecorderStart::Continuous,
                    root_directory: recording_root.to_path_buf(),
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
                    max_queue_bytes: 1_024,
                    shutdown_timeout_ms: 1_000,
                    max_storage_bytes: Some(1024 * 1024),
                    max_storage_files: Some(32),
                    max_active_recorders: 4,
                }],
            }],
        }],
        l4_services: Vec::new(),
        audit: None,
        log_filter: None,
    }
}

pub(super) fn connect_rtmp_session(server: &mut RtmpSession, application: &str) -> ClientSession {
    let mut handshake = Handshake::new(PeerType::Client);
    let client_hello = handshake
        .generate_outbound_p0_and_p1()
        .expect("client hello");
    let server_hello = server.receive(&client_hello, 1_000).expect("server hello");
    let client_finish = match handshake
        .process_bytes(&server_hello.concat())
        .expect("client handshake response")
    {
        HandshakeProcessResult::Completed { response_bytes, .. } => response_bytes,
        result @ HandshakeProcessResult::InProgress { .. } => {
            panic!("client handshake did not complete: {result:?}");
        }
    };
    let startup = server
        .receive(&client_finish, 1_000)
        .expect("server handshake completion");
    let (mut client, initial) = ClientSession::new(ClientSessionConfig::new()).expect("client");
    assert!(initial.is_empty());
    assert!(feed_rtmp_client(&mut client, startup).0.is_empty());
    let request = client
        .request_connection(application.into())
        .expect("connection request");
    let events = exchange_rtmp(&mut client, server, vec![request], 1_000);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ClientSessionEvent::ConnectionRequestAccepted))
    );
    client
}

pub(super) fn exchange_rtmp(
    client: &mut ClientSession,
    server: &mut RtmpSession,
    initial: Vec<ClientSessionResult>,
    at_unix_ms: u64,
) -> Vec<ClientSessionEvent> {
    let mut packets = outbound_rtmp_packets(initial);
    let mut events = Vec::new();
    for _ in 0..8 {
        if packets.is_empty() {
            return events;
        }
        let mut responses = Vec::new();
        while let Some(packet) = packets.pop_front() {
            responses.extend(server.receive(&packet, at_unix_ms).expect("server input"));
        }
        let (next, mut raised) = feed_rtmp_client(client, responses);
        packets = next;
        events.append(&mut raised);
    }
    panic!("RTMP exchange did not settle");
}

pub(super) fn feed_rtmp_client(
    client: &mut ClientSession,
    server_packets: Vec<Vec<u8>>,
) -> (VecDeque<Vec<u8>>, Vec<ClientSessionEvent>) {
    let mut packets = VecDeque::new();
    let mut events = Vec::new();
    for packet in server_packets {
        for result in client.handle_input(&packet).expect("client input") {
            match result {
                ClientSessionResult::OutboundResponse(packet) => {
                    packets.push_back(packet.bytes);
                }
                ClientSessionResult::RaisedEvent(event) => events.push(event),
                ClientSessionResult::UnhandleableMessageReceived(_) => {}
            }
        }
    }
    (packets, events)
}

pub(super) fn outbound_rtmp_packets(results: Vec<ClientSessionResult>) -> VecDeque<Vec<u8>> {
    results
        .into_iter()
        .filter_map(|result| match result {
            ClientSessionResult::OutboundResponse(packet) => Some(packet.bytes),
            ClientSessionResult::RaisedEvent(_)
            | ClientSessionResult::UnhandleableMessageReceived(_) => None,
        })
        .collect()
}

#[path = "tests/generation.rs"]
mod generation;
#[path = "tests/listeners.rs"]
mod listeners;
#[path = "tests/rtmp.rs"]
mod rtmp;
