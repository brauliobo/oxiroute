use std::os::fd::AsRawFd as _;

use bytes::Bytes;
use http::{Method, Request, StatusCode};
use oxiroute_server::{ObservedClients, ServiceSpec, observe_clients};
use pingora::protocols::{GetSocketDigest as _, SocketDigest};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::watch,
    task::JoinHandle,
};

use super::*;
use crate::tests::activate_test_generation;

struct Harness {
    metrics: RuntimeMetrics,
    listener: ListenerMetrics,
    spec: ServiceSpec,
    generation: Arc<RuntimeGeneration>,
    ingress: TcpListener,
    _directory: tempfile::TempDir,
}

impl Harness {
    async fn forward(protocol: &str, version: &str, ports: &[u16]) -> Self {
        let ports = ports
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        Self::start(|address| {
            format!(
                r#"return {{
  version = 1,
  listeners = {{ {{
    name = "forward",
    bind = {{ type = "socket", address = "{address}" }},
    protocol = "{protocol}",
    service = "forward",
  }} }},
  forward_proxy_services = {{ {{
    name = "forward",
    enabled_versions = {{ "{version}" }},
    allow_absolute_form = true,
    tls_required = false,
    connect = {{ enabled = true, allowed_ports = {{ {ports} }} }},
    destination_policy = {{ deny_private = false }},
  }} }},
}}"#
            )
        })
        .await
    }

    async fn relay(origin: std::net::SocketAddr) -> Self {
        Self::start(|address| {
            format!(
                r#"return {{
  version = 1,
  listeners = {{ {{
    name = "relay",
    bind = {{ type = "socket", address = "{address}" }},
    protocol = "tcp",
    service = "relay",
  }} }},
  upstream_pools = {{ {{
    name = "origin",
    servers = {{ {{ name = "origin-1", endpoint = {{ type = "socket", address = "{origin}" }} }} }},
    algorithm = "round_robin",
  }} }},
  l4_services = {{ {{
    name = "relay",
    upstream_pool = "origin",
    connect_timeout_ms = 1000,
    idle_timeout_ms = 5000,
  }} }},
}}"#
            )
        })
        .await
    }

    async fn start(render: impl FnOnce(std::net::SocketAddr) -> String) -> Self {
        let ingress = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("ingress bind");
        let address = ingress.local_addr().expect("ingress address");
        let config = oxiroute_config_source::load_lua(&render(address)).expect("test config");
        let directory = tempfile::tempdir().expect("generation directory");
        let mut draft = config.to_draft();
        draft.listeners.clear();
        let (_, _, generation) =
            activate_test_generation(&directory.path().join("oxiroute.kdl"), &draft);
        let spec = oxiroute_server::service_specs(&config)
            .expect("forward services")
            .remove(0);
        let metrics = RuntimeMetrics::new();
        let listener = metrics
            .register_configured_listener(
                &spec.name,
                spec.kind.protocol(),
                &spec.bind,
                spec.max_connections,
            )
            .expect("listener metrics");
        Self {
            metrics,
            listener,
            spec,
            generation,
            ingress,
            _directory: directory,
        }
    }

    async fn accept(&self) -> (TcpStream, Stream) {
        let address = self.ingress.local_addr().expect("ingress address");
        let client = TcpStream::connect(address).await.expect("client connect");
        let (accepted, _) = self.ingress.accept().await.expect("ingress accept");
        let mut socket = pingora::protocols::l4::stream::Stream::from(accepted);
        socket.set_socket_digest(SocketDigest::from_raw_fd(socket.as_raw_fd()));
        (client, Box::new(socket))
    }

    fn observed(&self) -> ObservedClients {
        observe_clients(&self.metrics)
    }

    fn serve_tcp(&self, downstream: Stream) -> (JoinHandle<Option<Stream>>, watch::Sender<bool>) {
        let ServiceKind::Tcp(plan) = self.spec.kind.clone() else {
            panic!("listener must compile as TCP");
        };
        let app = Arc::new(TcpRelay::new(
            plan,
            self.listener.clone(),
            None,
            Arc::clone(&self.generation),
        ));
        let (shutdown_tx, shutdown) = watch::channel(false);
        let task = tokio::spawn(async move { app.process_new(downstream, &shutdown).await });
        (task, shutdown_tx)
    }

    fn serve_h1(&self, downstream: Stream) -> (JoinHandle<Option<Stream>>, watch::Sender<bool>) {
        let ServiceKind::ForwardHttp1(plan) = self.spec.kind.clone() else {
            panic!("listener must compile as forward HTTP/1");
        };
        let app = Arc::new(ForwardHttp1App::new(
            plan,
            self.listener.clone(),
            None,
            false,
            oxiroute_acme::ChallengeStore::default(),
        ));
        let (shutdown_tx, shutdown) = watch::channel(false);
        let task = tokio::spawn(async move { app.process_new(downstream, &shutdown).await });
        (task, shutdown_tx)
    }

    fn serve_h2(&self, downstream: Stream) -> (JoinHandle<Option<Stream>>, watch::Sender<bool>) {
        let ServiceKind::ForwardHttp2(plan) = self.spec.kind.clone() else {
            panic!("listener must compile as forward HTTP/2");
        };
        let app = Arc::new(ForwardHttp2App::new(
            Arc::clone(&plan),
            HttpListenerApp::new(
                HttpDownstreamPolicyApp::new(
                    ForwardHttp2ServiceApp::new(plan, self.spec.h2_windows),
                    self.spec.downstream_timeouts,
                ),
                None,
            )
            .with_generation(Arc::clone(&self.generation)),
            self.listener.clone(),
            Arc::clone(&self.generation),
        ));
        let (shutdown_tx, shutdown) = watch::channel(false);
        let task = tokio::spawn(async move { app.process_new(downstream, &shutdown).await });
        (task, shutdown_tx)
    }
}

async fn read_head(stream: &mut TcpStream) -> Vec<u8> {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        head.push(stream.read_u8().await.expect("head byte"));
    }
    head
}

async fn origin_replying(response: &'static [u8]) -> (std::net::SocketAddr, JoinHandle<()>) {
    let origin = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let address = origin.local_addr().expect("origin address");
    let task = tokio::spawn(async move {
        let (mut stream, _) = origin.accept().await.expect("origin accept");
        read_head(&mut stream).await;
        stream.write_all(response).await.expect("origin response");
        stream.shutdown().await.expect("origin close");
    });
    (address, task)
}

async fn echoing_origin() -> (std::net::SocketAddr, JoinHandle<()>) {
    let origin = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let address = origin.local_addr().expect("origin address");
    let task = tokio::spawn(async move {
        let (mut stream, _) = origin.accept().await.expect("origin accept");
        let mut ping = [0; 4];
        stream.read_exact(&mut ping).await.expect("tunnel payload");
        assert_eq!(&ping, b"ping");
        stream.write_all(b"pong").await.expect("tunnel reply");
        let mut rest = Vec::new();
        stream.read_to_end(&mut rest).await.expect("tunnel EOF");
    });
    (address, task)
}

#[tokio::test]
async fn forward_http1_requests_count_their_bytes_and_complete_with_the_connection() {
    let (origin, origin_task) =
        origin_replying(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello")
            .await;
    let harness = Harness::forward("forward_http1", "h1", &[origin.port()]).await;
    let (mut client, downstream) = harness.accept().await;
    let (task, _shutdown) = harness.serve_h1(downstream);
    let request =
        format!("GET http://{origin}/page HTTP/1.1\r\nHost: {origin}\r\nConnection: close\r\n\r\n");

    client
        .write_all(request.as_bytes())
        .await
        .expect("client request");
    let mut response = Vec::new();
    client
        .read_to_end(&mut response)
        .await
        .expect("client response");
    task.await.expect("forward connection");
    origin_task.await.expect("origin task");

    assert!(response.starts_with(b"HTTP/1.1 200"));
    assert_eq!(
        harness.observed(),
        ObservedClients {
            live_protocols: Vec::new(),
            connections_total: 1,
            requests: 1,
            errors: 0,
            bytes_in: request.len() as u64,
            bytes_out: response.len() as u64,
        }
    );
}

#[tokio::test]
async fn a_forward_http1_connect_tunnel_is_a_tcp_connection_while_it_relays() {
    let (origin, origin_task) = echoing_origin().await;
    let harness = Harness::forward("forward_http1", "h1", &[origin.port()]).await;
    let (mut client, downstream) = harness.accept().await;
    let (task, _shutdown) = harness.serve_h1(downstream);
    let connect = format!("CONNECT {origin} HTTP/1.1\r\nHost: {origin}\r\n\r\n");

    client
        .write_all(connect.as_bytes())
        .await
        .expect("CONNECT request");
    let head = read_head(&mut client).await;
    assert!(head.starts_with(b"HTTP/1.1 200"));
    client.write_all(b"ping").await.expect("tunnel payload");
    let mut pong = [0; 4];
    client.read_exact(&mut pong).await.expect("tunnel reply");
    assert_eq!(&pong, b"pong");

    let open = harness.observed();
    assert_eq!(open.live_protocols, ["tcp"]);
    assert_eq!(open.bytes_in, connect.len() as u64 + 4);

    client.shutdown().await.expect("client half-close");
    let mut rest = Vec::new();
    client.read_to_end(&mut rest).await.expect("tunnel EOF");
    task.await.expect("forward connection");
    origin_task.await.expect("origin task");

    assert_eq!(
        harness.observed(),
        ObservedClients {
            live_protocols: Vec::new(),
            connections_total: 1,
            requests: 1,
            errors: 0,
            bytes_in: connect.len() as u64 + 4,
            bytes_out: head.len() as u64 + 4,
        }
    );
}

#[tokio::test]
async fn a_forward_http1_connect_to_a_dead_origin_counts_as_a_failed_request() {
    let dead = crate::test_ports::unused_tcp_address();
    let harness = Harness::forward("forward_http1", "h1", &[dead.port()]).await;
    let (mut client, downstream) = harness.accept().await;
    let (task, _shutdown) = harness.serve_h1(downstream);
    let connect = format!("CONNECT {dead} HTTP/1.1\r\nHost: {dead}\r\nConnection: close\r\n\r\n");

    client
        .write_all(connect.as_bytes())
        .await
        .expect("CONNECT request");
    let mut response = Vec::new();
    client
        .read_to_end(&mut response)
        .await
        .expect("client response");
    task.await.expect("forward connection");

    assert!(response.starts_with(b"HTTP/1.1 502"));
    let observed = harness.observed();
    assert_eq!((observed.requests, observed.errors), (1, 1));
    assert_eq!(observed.bytes_in, connect.len() as u64);
    assert_eq!(observed.bytes_out, response.len() as u64);
}

#[tokio::test]
async fn a_forward_http2_connect_stream_counts_its_payload_on_an_h2_connection() {
    let (origin, origin_task) = echoing_origin().await;
    let harness = Harness::forward("forward_http2", "h2", &[origin.port()]).await;
    let (client, downstream) = harness.accept().await;
    let (task, _shutdown) = harness.serve_h2(downstream);
    let (mut sender, connection) = h2::client::handshake(client).await.expect("H2 handshake");
    let driver = tokio::spawn(connection);

    let request = Request::builder()
        .method(Method::CONNECT)
        .uri(origin.to_string())
        .body(())
        .expect("CONNECT request");
    let (response, mut stream) = sender.send_request(request, false).expect("send CONNECT");
    let response = response.await.expect("CONNECT response");
    assert_eq!(response.status(), StatusCode::OK);
    let mut incoming = response.into_body();
    stream
        .send_data(Bytes::from_static(b"ping"), false)
        .expect("tunnel payload");
    let pong = incoming
        .data()
        .await
        .expect("tunnel reply")
        .expect("reply data");
    assert_eq!(&pong[..], b"pong");

    let open = harness.observed();
    assert_eq!(open.live_protocols, ["h2"]);
    assert_eq!(open.bytes_in, 4);

    stream.send_data(Bytes::new(), true).expect("end of tunnel");
    while incoming.data().await.is_some() {}
    drop((sender, stream, incoming));
    driver.abort();
    task.await.expect("forward connection");
    origin_task.await.expect("origin task");

    assert_eq!(
        harness.observed(),
        ObservedClients {
            live_protocols: Vec::new(),
            connections_total: 1,
            requests: 1,
            errors: 0,
            bytes_in: 4,
            bytes_out: 4,
        }
    );
}

#[tokio::test]
async fn a_tcp_relay_registers_the_physical_peer_and_counts_its_bytes() {
    let (origin, origin_task) = echoing_origin().await;
    let harness = Harness::relay(origin).await;
    let (mut client, downstream) = harness.accept().await;
    let (task, _shutdown) = harness.serve_tcp(downstream);

    client.write_all(b"ping").await.expect("relay payload");
    let mut pong = [0; 4];
    client.read_exact(&mut pong).await.expect("relay reply");
    assert_eq!(&pong, b"pong");
    let open = harness.observed();
    assert_eq!(open.live_protocols, ["tcp"]);
    assert_eq!(open.bytes_in, 4);

    client.shutdown().await.expect("client half-close");
    let mut rest = Vec::new();
    client.read_to_end(&mut rest).await.expect("relay EOF");
    task.await.expect("relay connection");
    origin_task.await.expect("origin task");

    assert_eq!(
        harness.observed(),
        ObservedClients {
            live_protocols: Vec::new(),
            connections_total: 1,
            requests: 1,
            errors: 0,
            bytes_in: 4,
            bytes_out: 4,
        }
    );
}
