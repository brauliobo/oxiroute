use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::{SocketAddr, TcpListener},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use bytes::Bytes;
use oxiroute_rtmp::{
    LiveHub, LiveHubLimits, RtmpApplication, RtmpCapabilities, RtmpPushApplication, RtmpPushTarget,
    RtmpRegistry, RtmpRelayConfig, RtmpRelayFailure, RtmpRelayPhase, RtmpServiceRuntime,
    RtmpSession, RtmpSessionPolicy, StreamMetadata,
};
use rml_rtmp::{
    handshake::{Handshake, HandshakeProcessResult, PeerType},
    sessions::{
        ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult,
        PublishRequestType,
    },
    time::RtmpTimestamp,
};

#[test]
fn unavailable_push_retries_recovers_bootstraps_and_stays_isolated_in_a_bounded_soak() {
    let address: SocketAddr = "127.0.0.1:1936".parse().expect("fixed push address");
    drop(TcpListener::bind(address).expect("port 1936 must begin absent"));
    let source_registry = Arc::new(RtmpRegistry::new(capabilities()));
    let target = RtmpPushTarget {
        address,
        host: "127.0.0.1".into(),
        transport: oxiroute_rtmp::RtmpTransport::Rtmp,
        application: RtmpPushApplication::StreamName,
        stream_name: None,
        options: oxiroute_rtmp::RtmpClientOptions::default(),
        config: RtmpRelayConfig {
            max_queue_messages: 4,
            max_queue_bytes: 64,
            connect_timeout: Duration::from_millis(20),
            handshake_timeout: Duration::from_secs(1),
            reconnect_interval: Duration::from_millis(30),
            buffer_duration: Duration::from_secs(5),
            max_chain_depth: 4,
        },
    };
    let source = RtmpServiceRuntime::new(
        "source",
        Arc::clone(&source_registry),
        LiveHub::new(LiveHubLimits::default()),
        RtmpSessionPolicy::new([RtmpApplication::with_runtime(
            "live",
            true,
            true,
            LiveHub::new(LiveHubLimits::default()),
            [target],
            [],
        )]),
    );
    let mut publisher = SessionClient::connect(&source, "live");
    publisher.publish("camera");
    publisher.metadata();
    publisher.audio(0, &[0xaf, 0x00, 0x12]);
    publisher.video(1, &[0x17, 0x00, 0, 0, 0, 0x01]);
    publisher.video(2, &[0x17, 0x01, 0, 0, 0, 0x22]);
    for marker in 0..2_048 {
        publisher.audio(3 + marker, &[0xaf, 0x01, marker.to_le_bytes()[0]]);
    }

    wait_until(Duration::from_secs(1), || {
        relay_status(&source_registry).is_some_and(|status| {
            status.connection_attempts >= 2
                && status.phase == RtmpRelayPhase::Backoff
                && status.last_failure == Some(RtmpRelayFailure::Connect)
                && status.events_dropped > 0
                && status.queue_messages <= 4
                && status.queue_bytes <= 64
        })
    });
    assert_eq!(source.hub().stats().publishers, 1);

    let sink_registry = Arc::new(RtmpRegistry::new(capabilities()));
    let sink_runtime = RtmpServiceRuntime::new(
        "sink",
        Arc::clone(&sink_registry),
        LiveHub::new(LiveHubLimits::default()),
        RtmpSessionPolicy::new([RtmpApplication::new("camera", true, true)]),
    );
    let sink = spawn_fake_server(address, sink_runtime, Arc::clone(&sink_registry));
    wait_until(Duration::from_secs(2), || {
        relay_status(&source_registry).is_some_and(|status| {
            status.phase == RtmpRelayPhase::Publishing
                && status.connections >= 2
                && status.reconnects >= 1
        })
    });
    publisher.video(100, &[0x17, 0x01, 0, 0, 0, 0x33]);
    publisher.audio(101, &[0xaf, 0x01, 0x44]);

    wait_until(Duration::from_secs(2), || {
        sink_registry
            .snapshot()
            .streams
            .first()
            .is_some_and(|stream| {
                stream.key.application == "camera"
                    && stream.key.name == "camera"
                    && stream.media.audio.payload_bytes_received >= 3
                    && stream.media.video.payload_bytes_received >= 6
            })
    });
    let status = relay_status(&source_registry).expect("observable relay");
    assert!(status.connection_attempts >= 3);
    assert!(status.connections >= 2);
    assert!(status.reconnects >= 1);
    assert!(status.events_sent >= 2);
    assert_eq!(status.destination.application, "camera");
    assert_eq!(status.destination.stream_name, "camera");

    let started = Instant::now();
    publisher.server.close(1_000).expect("publisher close");
    drop(publisher);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(source_registry.snapshot().streams.is_empty());
    sink.join().expect("fake RTMP server");
}

fn capabilities() -> RtmpCapabilities {
    RtmpCapabilities {
        live_ingest: true,
        manual_recording: false,
    }
}

fn spawn_fake_server(
    address: SocketAddr,
    runtime: RtmpServiceRuntime,
    registry: Arc<RtmpRegistry>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let listener = TcpListener::bind(address).expect("bind fake RTMP server");
        for connection_index in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept relay");
            let mut session = runtime.session();
            let mut buffer = [0; 16 * 1024];
            let mut now = 1_u64;
            loop {
                let count = stream.read(&mut buffer).expect("read relay");
                if count == 0 {
                    break;
                }
                now += 1;
                let packets = session
                    .receive(&buffer[..count], now)
                    .expect("process relay protocol");
                for packet in packets {
                    stream.write_all(&packet).expect("write relay response");
                }
                stream.flush().expect("flush relay response");
                if connection_index == 0
                    && registry.snapshot().streams.first().is_some_and(|stream| {
                        stream.media.audio.payload_bytes_received > 0
                            || stream.media.video.payload_bytes_received > 0
                    })
                {
                    break;
                }
            }
            session.close(now).expect("close relay session");
        }
    })
}

fn relay_status(registry: &RtmpRegistry) -> Option<oxiroute_rtmp::RtmpRelayStatus> {
    registry
        .snapshot()
        .streams
        .first()?
        .relays
        .first()
        .map(|relay| relay.status.clone())
}

fn wait_until(timeout: Duration, predicate: impl Fn() -> bool) {
    let deadline = Instant::now() + timeout;
    while !predicate() {
        assert!(Instant::now() < deadline, "condition timeout");
        thread::sleep(Duration::from_millis(5));
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
            .request_publishing(stream_name.to_owned(), PublishRequestType::Live)
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
        metadata.audio_codec_id = Some(10);
        let packet = self
            .client
            .publish_metadata(&metadata)
            .expect("publish metadata");
        exchange(&mut self.client, &mut self.server, vec![packet], 5);
    }

    fn audio(&mut self, timestamp: u32, payload: &[u8]) {
        let packet = self
            .client
            .publish_audio_data(
                Bytes::copy_from_slice(payload),
                RtmpTimestamp::new(timestamp),
                false,
            )
            .expect("publish audio");
        exchange(&mut self.client, &mut self.server, vec![packet], 6);
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
        exchange(&mut self.client, &mut self.server, vec![packet], 7);
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
