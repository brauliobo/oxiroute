#![allow(dead_code)]

use std::{collections::VecDeque, future::Future, net::SocketAddr, pin::Pin, time::Duration};

use bytes::Bytes;
use oxiroute_rtmp::{RtmpServiceRuntime, RtmpSession};
use rml_rtmp::{
    handshake::{Handshake, HandshakeProcessResult, PeerType},
    sessions::{
        ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult,
        PublishRequestType,
    },
    time::RtmpTimestamp,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
    time::{sleep, timeout},
};

const WIRE_TIMEOUT: Duration = Duration::from_secs(10);

pub struct RtmpWireClient {
    stream: TcpStream,
    session: ClientSession,
}

impl RtmpWireClient {
    pub async fn connect(address: SocketAddr, application: &str) -> Self {
        let stream = TcpStream::connect(address)
            .await
            .expect("connect RTMP client");
        Self::establish(stream, application).await
    }

    pub async fn connect_after(address: SocketAddr, application: &str, delay: Duration) -> Self {
        let stream = TcpStream::connect(address)
            .await
            .expect("connect delayed RTMP client");
        sleep(delay).await;
        Self::establish(stream, application).await
    }

    async fn establish(mut stream: TcpStream, application: &str) -> Self {
        let mut handshake = Handshake::new(PeerType::Client);
        let client_hello = handshake
            .generate_outbound_p0_and_p1()
            .expect("generate RTMP client hello");
        stream
            .write_all(&client_hello)
            .await
            .expect("write RTMP client hello");
        let mut server_hello = vec![0; 3_073];
        timeout(WIRE_TIMEOUT, stream.read_exact(&mut server_hello))
            .await
            .expect("RTMP handshake response timed out")
            .expect("read RTMP handshake response");
        let client_finish = match handshake
            .process_bytes(&server_hello)
            .expect("process RTMP handshake response")
        {
            HandshakeProcessResult::Completed { response_bytes, .. } => response_bytes,
            result @ HandshakeProcessResult::InProgress { .. } => {
                panic!("RTMP client handshake did not complete: {result:?}");
            }
        };
        stream
            .write_all(&client_finish)
            .await
            .expect("write RTMP handshake finish");
        let (session, initial) =
            ClientSession::new(ClientSessionConfig::new()).expect("create RTMP client session");
        let mut client = Self { stream, session };
        client.write_results(initial).await;
        let request = client
            .session
            .request_connection(application.into())
            .expect("request RTMP application connection");
        client
            .wait_for_event(vec![request], |event| {
                matches!(event, ClientSessionEvent::ConnectionRequestAccepted)
            })
            .await;
        client
    }

    pub async fn publish(&mut self, stream_name: &str) {
        let request = self
            .session
            .request_publishing(stream_name.into(), PublishRequestType::Live)
            .expect("request RTMP publishing");
        self.wait_for_event(vec![request], |event| {
            matches!(event, ClientSessionEvent::PublishRequestAccepted)
        })
        .await;
    }

    pub async fn play(&mut self, stream_name: &str) {
        let request = self
            .session
            .request_playback(stream_name.into())
            .expect("request RTMP playback");
        self.wait_for_event(vec![request], |event| {
            matches!(event, ClientSessionEvent::PlaybackRequestAccepted)
        })
        .await;
    }

    pub async fn publish_audio(&mut self, timestamp_ms: u32, payload: &[u8]) {
        let packet = self
            .session
            .publish_audio_data(
                Bytes::copy_from_slice(payload),
                RtmpTimestamp::new(timestamp_ms),
                false,
            )
            .expect("publish RTMP audio");
        self.write_results(vec![packet]).await;
    }

    pub fn wait_for_event<'a, P>(
        &'a mut self,
        initial: Vec<ClientSessionResult>,
        predicate: P,
    ) -> Pin<Box<dyn Future<Output = ClientSessionEvent> + Send + 'a>>
    where
        P: Fn(&ClientSessionEvent) -> bool + Send + Sync + 'a,
    {
        Box::pin(async move {
            self.write_results(initial).await;
            timeout(WIRE_TIMEOUT, async {
                let mut buffer = vec![0; 16 * 1024];
                loop {
                    let count = self
                        .stream
                        .read(&mut buffer)
                        .await
                        .expect("read RTMP server packet");
                    assert_ne!(count, 0, "RTMP server closed before the expected event");
                    let results = self
                        .session
                        .handle_input(&buffer[..count])
                        .expect("process RTMP server packet");
                    let mut outbound = Vec::new();
                    for result in results {
                        match result {
                            ClientSessionResult::OutboundResponse(packet) => {
                                outbound.push(packet.bytes);
                            }
                            ClientSessionResult::RaisedEvent(event) if predicate(&event) => {
                                self.write_packets(outbound).await;
                                return event;
                            }
                            ClientSessionResult::RaisedEvent(_)
                            | ClientSessionResult::UnhandleableMessageReceived(_) => {}
                        }
                    }
                    self.write_packets(outbound).await;
                }
            })
            .await
            .expect("expected RTMP client event timed out")
        })
    }

    async fn write_results(&mut self, results: Vec<ClientSessionResult>) {
        let packets = outbound_packets(results).collect();
        self.write_packets(packets).await;
    }

    async fn write_packets(&mut self, packets: Vec<Vec<u8>>) {
        for packet in packets {
            self.stream
                .write_all(&packet)
                .await
                .expect("write RTMP client packet");
        }
        self.stream
            .flush()
            .await
            .expect("flush RTMP client packets");
    }

    pub async fn close(mut self) {
        self.stream.shutdown().await.expect("close RTMP client");
    }
}

pub struct RtmpSessionClient {
    pub server: RtmpSession,
    pub client: ClientSession,
}

impl RtmpSessionClient {
    pub fn connect(runtime: &RtmpServiceRuntime, application: &str) -> Self {
        let mut server = runtime.session();
        let client = connect_session(&mut server, application);
        Self { server, client }
    }

    pub fn publish(&mut self, stream_name: &str, at_unix_ms: u64) {
        let request = self
            .client
            .request_publishing(stream_name.into(), PublishRequestType::Live)
            .expect("publish request");
        let events = exchange(
            &mut self.client,
            &mut self.server,
            vec![request],
            at_unix_ms,
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ClientSessionEvent::PublishRequestAccepted))
        );
    }

    pub fn publish_audio(&mut self, timestamp: u32, payload: &[u8], at_unix_ms: u64) {
        let packet = self
            .client
            .publish_audio_data(
                Bytes::copy_from_slice(payload),
                RtmpTimestamp::new(timestamp),
                false,
            )
            .expect("audio packet");
        exchange(&mut self.client, &mut self.server, vec![packet], at_unix_ms);
    }
}

fn connect_session(server: &mut RtmpSession, application: &str) -> ClientSession {
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
    assert!(feed_server_packets(&mut client, startup).0.is_empty());
    let request = client
        .request_connection(application.into())
        .expect("connection request");
    let events = exchange(&mut client, server, vec![request], 1_000);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ClientSessionEvent::ConnectionRequestAccepted))
    );
    client
}

fn exchange(
    client: &mut ClientSession,
    server: &mut RtmpSession,
    initial: Vec<ClientSessionResult>,
    at_unix_ms: u64,
) -> Vec<ClientSessionEvent> {
    let mut packets = outbound_packets(initial).collect::<VecDeque<_>>();
    let mut events = Vec::new();
    for _ in 0..8 {
        if packets.is_empty() {
            return events;
        }
        let mut responses = Vec::new();
        while let Some(packet) = packets.pop_front() {
            responses.extend(server.receive(&packet, at_unix_ms).expect("server input"));
        }
        let (next, mut raised) = feed_server_packets(client, responses);
        packets = next;
        events.append(&mut raised);
    }
    panic!("RTMP exchange did not settle");
}

fn feed_server_packets(
    client: &mut ClientSession,
    server_packets: Vec<Vec<u8>>,
) -> (VecDeque<Vec<u8>>, Vec<ClientSessionEvent>) {
    let mut packets = VecDeque::new();
    let mut events = Vec::new();
    for packet in server_packets {
        for result in client.handle_input(&packet).expect("client input") {
            match result {
                ClientSessionResult::OutboundResponse(packet) => packets.push_back(packet.bytes),
                ClientSessionResult::RaisedEvent(event) => events.push(event),
                ClientSessionResult::UnhandleableMessageReceived(_) => {}
            }
        }
    }
    (packets, events)
}

fn outbound_packets(results: Vec<ClientSessionResult>) -> impl Iterator<Item = Vec<u8>> {
    results.into_iter().filter_map(|result| match result {
        ClientSessionResult::OutboundResponse(packet) => Some(packet.bytes),
        ClientSessionResult::RaisedEvent(_)
        | ClientSessionResult::UnhandleableMessageReceived(_) => None,
    })
}
