#[allow(clippy::wildcard_imports)]
use super::*;
use crate::test_ports::unused_tcp_address;
use oxiroute_server::{ObservedClients, observe_clients};
use pingora::protocols::{GetSocketDigest as _, SocketDigest};
use std::os::fd::AsRawFd as _;
use tokio::net::TcpStream;

const RTMP_HANDSHAKE_SIZE: usize = 1 + 1536 + 1536;

struct TrackedRtmp {
    metrics: RuntimeMetrics,
    session: tokio::task::JoinHandle<Option<Stream>>,
    client: TcpStream,
    _shutdown: watch::Sender<bool>,
    _directory: tempfile::TempDir,
}

async fn open_rtmp_session() -> TrackedRtmp {
    let directory = tempfile::tempdir().expect("RTMP generation directory");
    let recording_root = directory.path().join("recordings");
    fs::create_dir(&recording_root).expect("RTMP recording root");
    let ingress = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("RTMP ingress bind");
    let address = ingress.local_addr().expect("RTMP ingress address");
    let config = recorder_runtime_config(unused_tcp_address(), &recording_root);
    let (_, _, generation) =
        activate_test_generation(&directory.path().join("oxiroute.kdl"), &config);
    let ServiceKind::Rtmp(service) = generation.services()[0].kind.clone() else {
        panic!("listener must compile as RTMP");
    };
    let app = Arc::new(RtmpIngest::new(
        "live".into(),
        generation
            .rtmp_service(service.service_id())
            .expect("RTMP service runtime"),
        generation.rtmp_control(),
        service,
        generation
            .traffic_listener_metrics("live")
            .expect("RTMP listener metrics"),
        Arc::clone(&generation),
    ));
    let client = TcpStream::connect(address).await.expect("RTMP client");
    let (accepted, _) = ingress.accept().await.expect("RTMP accept");
    let mut socket = pingora::protocols::l4::stream::Stream::from(accepted);
    socket.set_socket_digest(SocketDigest::from_raw_fd(socket.as_raw_fd()));
    let downstream: Stream = Box::new(socket);
    let (shutdown_sender, shutdown) = watch::channel(false);
    TrackedRtmp {
        metrics: generation.metrics().clone(),
        session: tokio::spawn(
            async move { Box::pin(app.process_new(downstream, &shutdown)).await },
        ),
        client,
        _shutdown: shutdown_sender,
        _directory: directory,
    }
}

async fn handshake_hello(client: &mut TcpStream) -> (Handshake, Vec<u8>) {
    let mut handshake = Handshake::new(PeerType::Client);
    let hello = handshake
        .generate_outbound_p0_and_p1()
        .expect("client hello");
    client.write_all(&hello).await.expect("write client hello");
    (handshake, hello)
}

#[tokio::test]
async fn rtmp_sessions_are_listed_while_open_and_persist_with_their_bytes_after_close() {
    let mut open = open_rtmp_session().await;
    let (mut handshake, hello) = handshake_hello(&mut open.client).await;
    let mut server_hello = vec![0; RTMP_HANDSHAKE_SIZE];
    open.client
        .read_exact(&mut server_hello)
        .await
        .expect("server hello");

    let live = observe_clients(&open.metrics);
    assert_eq!(live.live_protocols, ["rtmp"]);
    assert_eq!(live.bytes_in, hello.len() as u64);
    assert_eq!((live.requests, live.errors), (0, 0));

    let HandshakeProcessResult::Completed { response_bytes, .. } = handshake
        .process_bytes(&server_hello)
        .expect("client handshake response")
    else {
        panic!("client handshake did not complete");
    };
    open.client
        .write_all(&response_bytes)
        .await
        .expect("write client handshake completion");
    open.client.shutdown().await.expect("close client write");
    let mut trailer = Vec::new();
    open.client
        .read_to_end(&mut trailer)
        .await
        .expect("server EOF");
    assert!(open.session.await.expect("RTMP session task").is_none());

    assert_eq!(
        observe_clients(&open.metrics),
        ObservedClients {
            live_protocols: Vec::new(),
            connections_total: 1,
            requests: 1,
            errors: 0,
            bytes_in: (hello.len() + response_bytes.len()) as u64,
            bytes_out: (server_hello.len() + trailer.len()) as u64,
        }
    );
}

#[tokio::test]
async fn an_rtmp_session_that_ends_on_a_protocol_error_counts_as_failed() {
    let mut open = open_rtmp_session().await;
    let invalid_hello = vec![0xff; 1 + 1536];
    open.client
        .write_all(&invalid_hello)
        .await
        .expect("write invalid hello");
    let mut trailer = Vec::new();
    open.client
        .read_to_end(&mut trailer)
        .await
        .expect("server EOF");
    assert!(open.session.await.expect("RTMP session task").is_none());

    assert_eq!(
        observe_clients(&open.metrics),
        ObservedClients {
            live_protocols: Vec::new(),
            connections_total: 1,
            requests: 1,
            errors: 1,
            bytes_in: invalid_hello.len() as u64,
            bytes_out: trailer.len() as u64,
        }
    );
}
