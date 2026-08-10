use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use bytes::Bytes;
use oxiroute_rtmp::{
    LiveHub, LiveHubLimits, RtmpApplication, RtmpCapabilities, RtmpDestinationResolver,
    RtmpDnsResolver, RtmpOutboundPolicy, RtmpPullTarget, RtmpRegistry, RtmpRelayConfig,
    RtmpServiceRuntime, RtmpSession, RtmpSessionPolicy, StreamMetadata,
};
use rml_rtmp::{
    handshake::{Handshake, HandshakeProcessResult, PeerType},
    sessions::{ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult},
    time::RtmpTimestamp,
};

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "the pull rotation test keeps source, runtime, and reconnect assertions together"
)]
fn pull_reconnects_to_a_rotated_loopback_address_and_keeps_the_local_publisher_bounded() {
    let first_listener = TcpListener::bind("127.0.0.1:0").expect("first source listener");
    let first_address = first_listener.local_addr().expect("first source address");
    let second_address: SocketAddr = format!("127.0.0.2:{}", first_address.port())
        .parse()
        .expect("second source address");
    let second_listener = TcpListener::bind(second_address).expect("second source listener");

    let resolver = Arc::new(SequenceResolver::new([
        Ok(vec![first_address]),
        Ok(vec![second_address]),
    ]));
    let destination_resolver = RtmpDestinationResolver::from_startup_with_resolver(
        "rotating.example",
        first_address.port(),
        oxiroute_rtmp::RtmpTransport::Rtmp,
        [first_address],
        RtmpOutboundPolicy {
            deny_private: false,
            ..RtmpOutboundPolicy::default()
        },
        [],
        Duration::from_millis(50),
        resolver.clone(),
    )
    .expect("startup destination resolver");

    let local_registry = Arc::new(RtmpRegistry::new(capabilities()));
    let target = RtmpPullTarget {
        address: first_address,
        host: "rotating.example".into(),
        transport: oxiroute_rtmp::RtmpTransport::Rtmp,
        source_application: "source".into(),
        source_stream_name: "camera".into(),
        local_application: "local".into(),
        local_stream_name: "camera".into(),
        options: oxiroute_rtmp::RtmpClientOptions::default(),
        config: RtmpRelayConfig {
            connect_timeout: Duration::from_millis(30),
            handshake_timeout: Duration::from_secs(1),
            reconnect_interval: Duration::from_millis(20),
            dns_resolver: Some(Arc::new(destination_resolver)),
            ..RtmpRelayConfig::default()
        },
    };
    let local_runtime = RtmpServiceRuntime::new(
        "local",
        Arc::clone(&local_registry),
        LiveHub::new(LiveHubLimits::default()),
        RtmpSessionPolicy::new([
            RtmpApplication::new("local", true, true).with_pull_targets([target])
        ]),
    );

    let first_source_runtime = RtmpServiceRuntime::new(
        "source",
        Arc::new(RtmpRegistry::new(capabilities())),
        LiveHub::new(LiveHubLimits::default()),
        RtmpSessionPolicy::new([RtmpApplication::new("source", true, true)]),
    );
    let second_source_runtime = RtmpServiceRuntime::new(
        "source",
        Arc::new(RtmpRegistry::new(capabilities())),
        LiveHub::new(LiveHubLimits::default()),
        RtmpSessionPolicy::new([RtmpApplication::new("source", true, true)]),
    );
    let connections = Arc::new(AtomicUsize::new(0));
    let first_server = spawn_source(
        first_listener,
        first_source_runtime,
        Arc::clone(&connections),
    );
    let second_server = spawn_source(
        second_listener,
        second_source_runtime,
        Arc::clone(&connections),
    );

    let mut observed_media = None;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        observed_media = local_registry
            .snapshot()
            .streams
            .first()
            .filter(|stream| {
                stream.publisher.is_some() && stream.media.video.payload_bytes_received > 0
            })
            .map(|stream| stream.media)
            .or(observed_media);
        if connections.load(Ordering::Acquire) >= 2
            && resolver.calls() >= 2
            && observed_media.is_some()
        {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    let snapshot = local_registry.snapshot();
    let state = snapshot.streams.first().map(|stream| {
        (
            stream.publisher.is_some(),
            stream.media.video.payload_bytes_received,
            stream.media.audio.payload_bytes_received,
        )
    });
    assert!(
        connections.load(Ordering::Acquire) >= 2
            && resolver.calls() >= 2
            && observed_media.is_some(),
        "connections={}, resolver_calls={}, observed_media={observed_media:?}, state={state:?}",
        connections.load(Ordering::Acquire),
        resolver.calls(),
    );
    let media = observed_media.expect("pull media snapshot");
    assert_eq!(media.video.flv_codec_id, Some(7));
    assert_eq!(
        media.video.video_codec,
        Some(oxiroute_rtmp::VideoCodecIdentifier::Flv(7))
    );
    assert!(media.video.last_observed_at_unix_ms.is_some());
    assert_eq!(media.fanout_payload_bytes_queued, 0);
    local_runtime.close_admission();
    first_server.join().expect("first source server");
    second_server.join().expect("second source server");
}

fn capabilities() -> RtmpCapabilities {
    RtmpCapabilities {
        live_ingest: true,
        manual_recording: false,
    }
}

fn spawn_source(
    listener: TcpListener,
    runtime: RtmpServiceRuntime,
    connections: Arc<AtomicUsize>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut publisher = SessionClient::connect(&runtime, "source");
        publisher.publish("camera");
        publisher.metadata();
        publisher.video(1, &[0x17, 0x00, 0, 0, 0, 0x01]);
        publisher.video(2, &[0x17, 0x01, 0, 0, 0, 0x22]);
        assert!(
            runtime
                .registry()
                .snapshot()
                .streams
                .first()
                .is_some_and(|stream| { stream.media.video.payload_bytes_received > 0 })
        );

        let (mut stream, _) = listener.accept().expect("accept pull client");
        connections.fetch_add(1, Ordering::AcqRel);
        let mut session = runtime.session();
        let mut buffer = [0; 16 * 1024];
        let mut now = 1_u64;
        'connection: loop {
            let count = stream.read(&mut buffer).expect("read pull client");
            if count == 0 {
                break;
            }
            now += 1;
            let packets = session
                .receive(&buffer[..count], now)
                .expect("process pull client");
            for packet in packets {
                if stream.write_all(&packet).is_err() {
                    break 'connection;
                }
            }
            if stream.flush().is_err() {
                break 'connection;
            }
            if session.is_playback_active() {
                publisher.video(100, &[0x17, 0x01, 0, 0, 0, 0x33]);
                thread::sleep(Duration::from_millis(100));
                let media = session.drain_playback(32).expect("drain pull media");
                for packet in media {
                    if stream.write_all(&packet).is_err() {
                        break 'connection;
                    }
                }
                let _ = stream.flush();
                thread::sleep(Duration::from_millis(100));
                break;
            }
        }
        session.close(now).expect("close pull session");
        publisher.server.close(now).expect("close source publisher");
    })
}

struct SequenceResolver {
    answers: Mutex<VecDeque<Result<Vec<SocketAddr>, io::ErrorKind>>>,
    calls: AtomicUsize,
}

impl SequenceResolver {
    fn new(answers: impl IntoIterator<Item = Result<Vec<SocketAddr>, io::ErrorKind>>) -> Self {
        Self {
            answers: Mutex::new(answers.into_iter().collect()),
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Acquire)
    }
}

impl RtmpDnsResolver for SequenceResolver {
    fn resolve(&self, _host: &str, _port: u16) -> io::Result<Vec<SocketAddr>> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        match self
            .answers
            .lock()
            .expect("sequence resolver mutex poisoned")
            .pop_front()
            .unwrap_or_else(|| Ok(Vec::new()))
        {
            Ok(addresses) => Ok(addresses),
            Err(kind) => Err(io::Error::from(kind)),
        }
    }
}

struct SessionClient {
    server: RtmpSession,
    client: ClientSession,
}

impl SessionClient {
    fn connect(runtime: &RtmpServiceRuntime, application: &str) -> Self {
        let mut server = runtime.session();
        let mut handshake = Handshake::new(PeerType::Client);
        let hello = handshake
            .generate_outbound_p0_and_p1()
            .expect("client hello");
        let response = server.receive(&hello, 1).expect("server hello");
        let finish = match handshake
            .process_bytes(&response.concat())
            .expect("handshake response")
        {
            HandshakeProcessResult::Completed { response_bytes, .. } => response_bytes,
            HandshakeProcessResult::InProgress { .. } => panic!("incomplete handshake"),
        };
        let startup = server.receive(&finish, 2).expect("handshake completion");
        let (mut client, _) =
            ClientSession::new(ClientSessionConfig::new()).expect("client session");
        feed_server(&mut client, startup);
        let connect = client
            .request_connection(application.to_owned())
            .expect("connect request");
        let events = exchange(&mut client, &mut server, vec![connect], 3);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ClientSessionEvent::ConnectionRequestAccepted))
        );
        Self { server, client }
    }

    fn publish(&mut self, stream_name: &str) {
        let publish = self
            .client
            .request_publishing(
                stream_name.to_owned(),
                rml_rtmp::sessions::PublishRequestType::Live,
            )
            .expect("publish request");
        let events = exchange(&mut self.client, &mut self.server, vec![publish], 4);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ClientSessionEvent::PublishRequestAccepted))
        );
    }

    fn metadata(&mut self) {
        let mut metadata = StreamMetadata::new();
        metadata.video_codec_id = Some(7);
        let packet = self
            .client
            .publish_metadata(&metadata)
            .expect("publish metadata");
        exchange(&mut self.client, &mut self.server, vec![packet], 5);
    }

    fn video(&mut self, timestamp: u32, payload: &[u8]) {
        let packet = self
            .client
            .publish_video_data(
                Bytes::copy_from_slice(payload),
                RtmpTimestamp::new(timestamp),
                false,
            )
            .expect("publish video");
        exchange(&mut self.client, &mut self.server, vec![packet], 6);
    }
}

fn exchange(
    client: &mut ClientSession,
    server: &mut RtmpSession,
    initial: Vec<ClientSessionResult>,
    at_unix_ms: u64,
) -> Vec<ClientSessionEvent> {
    let mut packets = outbound(initial);
    let mut events = Vec::new();
    for _ in 0..8 {
        if packets.is_empty() {
            return events;
        }
        let mut responses = Vec::new();
        while let Some(packet) = packets.pop_front() {
            responses.extend(server.receive(&packet, at_unix_ms).expect("server input"));
        }
        let (next, mut raised) = feed_server(client, responses);
        packets = next;
        events.append(&mut raised);
    }
    panic!("RTMP exchange did not settle");
}

fn feed_server(
    client: &mut ClientSession,
    packets: Vec<Vec<u8>>,
) -> (VecDeque<Vec<u8>>, Vec<ClientSessionEvent>) {
    let mut outbound = VecDeque::new();
    let mut events = Vec::new();
    for packet in packets {
        for result in client.handle_input(&packet).expect("client input") {
            match result {
                ClientSessionResult::OutboundResponse(packet) => outbound.push_back(packet.bytes),
                ClientSessionResult::RaisedEvent(event) => events.push(event),
                ClientSessionResult::UnhandleableMessageReceived(_) => {}
            }
        }
    }
    (outbound, events)
}

fn outbound(results: Vec<ClientSessionResult>) -> VecDeque<Vec<u8>> {
    results
        .into_iter()
        .filter_map(|result| match result {
            ClientSessionResult::OutboundResponse(packet) => Some(packet.bytes),
            ClientSessionResult::RaisedEvent(_)
            | ClientSessionResult::UnhandleableMessageReceived(_) => None,
        })
        .collect()
}
