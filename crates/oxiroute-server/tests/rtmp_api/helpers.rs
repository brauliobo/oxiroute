#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn send_raw_command(
    server: &mut RtmpSession,
    serializer: &mut ChunkSerializer,
    command_name: &str,
    message_stream_id: u32,
    additional_arguments: Vec<Amf0Value>,
    at_unix_ms: u64,
) {
    let message = RtmpMessage::Amf0Command {
        command_name: command_name.to_owned(),
        transaction_id: 0.0,
        command_object: Amf0Value::Null,
        additional_arguments,
    };
    let payload = message
        .into_message_payload(RtmpTimestamp::new(0), message_stream_id)
        .expect("raw command payload");
    let packet = serializer
        .serialize(&payload, false, false)
        .expect("raw command packet");
    server
        .receive(&packet.bytes, at_unix_ms)
        .expect("raw command");
}

pub(super) fn empty_control() -> RtmpControlHandle {
    PreparedRtmpRuntimeSet::prepare(
        [],
        &RtmpPrepareContext::new(RtmpPrepareMode::Activation, []),
        Instant::now() + Duration::from_secs(1),
    )
    .expect("empty RTMP preparation")
    .start(Instant::now() + Duration::from_secs(1))
    .expect("empty RTMP runtime")
    .control()
}

pub(super) fn management_api(
    control: RtmpControlHandle,
    metrics: RuntimeMetrics,
) -> RtmpManagementApi {
    RtmpManagementApi::new(control, metrics, empty_topology())
}

pub(super) fn runtime_for_config(config: ConfigDraft) -> RtmpRuntimeSet {
    let config = config.validate().expect("valid RTMP test config");
    let services = oxiroute_server::service_specs(&config).expect("RTMP test services");
    let plans = services.iter().filter_map(|service| match &service.kind {
        ServiceKind::Rtmp(service) => Some(service.value_plan()),
        _ => None,
    });
    PreparedRtmpRuntimeSet::prepare(
        plans,
        &RtmpPrepareContext::new(RtmpPrepareMode::Activation, []),
        Instant::now() + Duration::from_secs(1),
    )
    .expect("RTMP test preparation")
    .start(Instant::now() + Duration::from_secs(1))
    .expect("RTMP test runtime")
}

pub(super) fn basic_runtime() -> RtmpRuntimeSet {
    runtime_for_config(candidate_config(&editable_config(), "live"))
}

pub(super) fn empty_topology() -> Arc<TopologySnapshot> {
    runtime_plan(&empty_config().validate().unwrap())
        .expect("empty runtime plan")
        .topology
}

pub(super) fn editable_config() -> ConfigDraft {
    static PRIVATE_KEY: OnceLock<fixture_support::PrivateKeyFixture> = OnceLock::new();
    let private_key =
        PRIVATE_KEY.get_or_init(|| fixture_support::private_key_fixture("proxy-a-key.pem"));

    ConfigDraft {
        retirement: None,
        certificates: vec![Certificate {
            name: "test-certificate".into(),
            dns_names: vec!["proxy.example.test".into()],
            source: CertificateSource::Files {
                certificate_chain_path: fixture("proxy-a.pem"),
                private_key_path: private_key.path().to_path_buf(),
            },
        }],
        ..empty_config()
    }
}

pub(super) fn candidate_config(active: &ConfigDraft, listener_name: &str) -> ConfigDraft {
    let mut candidate = active.clone();
    candidate.listeners.push(Listener {
        name: listener_name.into(),
        bind: socket_bind(loopback_address(1935)),
        protocol: Protocol::Rtmp,
        service: Some("live".into()),
        tls_profile: None,
        proxy_protocol: None,
        max_connections: Some(100),
        downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
    });
    candidate.rtmp_services.push(RtmpService {
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
            name: "broadcast".into(),
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
    });
    candidate
}

pub(super) async fn read_response_head(stream: &mut TcpStream) -> Vec<u8> {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        stream
            .read_exact(&mut byte)
            .await
            .expect("read event stream response headers");
        head.push(byte[0]);
    }
    head
}

pub(super) async fn read_chunk(stream: &mut TcpStream) -> Vec<u8> {
    let mut line = Vec::new();
    let mut byte = [0_u8; 1];
    while !line.ends_with(b"\r\n") {
        stream
            .read_exact(&mut byte)
            .await
            .expect("read SSE chunk size");
        line.push(byte[0]);
    }
    let size = usize::from_str_radix(
        std::str::from_utf8(&line[..line.len() - 2])
            .expect("SSE chunk size")
            .trim(),
        16,
    )
    .expect("SSE chunk length");
    let mut body = vec![0_u8; size];
    stream.read_exact(&mut body).await.expect("read SSE chunk");
    let mut terminator = [0_u8; 2];
    stream
        .read_exact(&mut terminator)
        .await
        .expect("read SSE chunk terminator");
    assert_eq!(terminator, *b"\r\n");
    body
}

pub(super) async fn read_ready_cursor(stream: &mut TcpStream) -> u64 {
    let frame = String::from_utf8(read_chunk(stream).await).expect("ready SSE frame");
    let data = frame
        .strip_prefix("event: ready\ndata: ")
        .and_then(|value| value.strip_suffix("\n\n"))
        .expect("ready frame shape");
    serde_json::from_str::<Value>(data).expect("ready frame JSON")["cursor"]
        .as_u64()
        .expect("ready cursor")
}

pub(super) fn certificate_event<'a>(page: &'a Value, certificate: &str) -> &'a Value {
    page["events"]
        .as_array()
        .expect("event page array")
        .iter()
        .find(|event| event["certificate"] == certificate)
        .expect("certificate activation event")
}
