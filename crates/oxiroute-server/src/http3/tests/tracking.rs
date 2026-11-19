use std::{
    fs,
    net::{Ipv4Addr, SocketAddr, UdpSocket as StdUdpSocket},
    os::unix::fs::PermissionsExt as _,
    sync::Arc,
    time::{Duration, Instant as StdInstant},
};

use bytes::{Buf as _, Bytes};
use http::{Method, Request, StatusCode};
use oxiroute_config::{
    AlpnProtocol, Certificate, CertificateSource, ConfigDraft, DownstreamTimeoutPolicy,
    ForwardAuditMode, ForwardConnectPolicy, ForwardDestinationPolicy, ForwardHeaderPolicy,
    ForwardHttpVersion, ForwardPeerPolicy, ForwardProxyService, ForwardResolverPolicy,
    HttpPathSelector, HttpProxyPolicy, HttpRoute, HttpRouteAction, HttpRoutePolicy, HttpService,
    HttpVersionPolicy, Listener, ListenerBind, Protocol, TlsProfile, TlsVersion, UpstreamAlgorithm,
    UpstreamConnectionReuse, UpstreamEndpoint, UpstreamPool,
};
use oxiroute_config_source::ConfigFormat;
use quinn::crypto::rustls::QuicClientConfig;
use rustls::pki_types::{CertificateDer, pem::PemObject as _};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::watch,
    time::{sleep, timeout},
};

use crate::{
    GenerationManager, Http3Runtime, RuntimeGeneration, ServiceKind,
    clients::{ClientQuery, ClientRow, ConnectionQuery, ConnectionRow, Sort},
    config::coordinator::{AuthoredRevision, EffectiveRevision, ResolvedConfigDocument},
};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
const SERVER_NAME: &str = "proxy.example.test";
const ORIGIN_BODY: &[u8] = b"hello";

type Sender = h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>;

struct Key {
    path: std::path::PathBuf,
    _directory: tempfile::TempDir,
}

impl Key {
    fn new() -> Self {
        let directory = tempfile::TempDir::new().expect("key directory");
        let path = directory.path().join("proxy-a-key.pem");
        fs::copy(format!("{FIXTURES}/proxy-a-key.pem"), &path).expect("copy key");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("key mode");
        Self {
            path,
            _directory: directory,
        }
    }
}

struct Harness {
    _key: Key,
    generation: Arc<RuntimeGeneration>,
    listener: SocketAddr,
    shutdown: watch::Sender<bool>,
    runtime: Http3Runtime,
}

impl Harness {
    fn start(config: ConfigDraft, key: Key, listener: SocketAddr, name: &str) -> Self {
        let transport = config.effective_http3_transport();
        let manager = GenerationManager::new();
        let candidate = manager
            .prepare(document(config))
            .expect("prepare HTTP/3 generation");
        let generation = manager
            .activate(&candidate)
            .expect("activate HTTP/3 generation");
        generation
            .metrics()
            .register_configured_listener(
                name,
                "http3",
                &ListenerBind::Udp { address: listener },
                Some(8),
            )
            .expect("register HTTP/3 listener metrics");
        let spec = generation
            .services()
            .iter()
            .find(|spec| spec.name == name)
            .cloned()
            .expect("planned HTTP/3 listener");
        let tls = spec.tls.clone().expect("HTTP/3 TLS profile");
        let reservation = generation
            .reservations()
            .get(name)
            .cloned()
            .expect("HTTP/3 reservation");
        let metrics = generation
            .metrics()
            .listener(name)
            .expect("listener metrics registry")
            .expect("HTTP/3 listener metrics");
        let (shutdown, shutdown_receiver) = watch::channel(false);
        let runtime = match &spec.kind {
            ServiceKind::Http3(service) => Http3Runtime::start_reverse(
                name,
                &reservation,
                service,
                &tls,
                &generation,
                &metrics,
                DownstreamTimeoutPolicy::default(),
                &shutdown_receiver,
                1,
                transport,
            ),
            ServiceKind::ForwardHttp3(service) => Http3Runtime::start_forward(
                name.into(),
                reservation,
                Arc::clone(service),
                tls,
                Arc::clone(&generation),
                metrics,
                shutdown_receiver,
                transport,
            ),
            _ => panic!("test service must be HTTP/3"),
        }
        .expect("start HTTP/3 runtime");
        Self {
            _key: key,
            generation,
            listener,
            shutdown,
            runtime,
        }
    }

    fn stop(self) {
        self.shutdown.send(true).expect("shutdown HTTP/3 runtime");
        self.runtime.join().expect("join HTTP/3 runtime");
    }

    fn live(&self) -> Vec<ConnectionRow> {
        self.generation
            .metrics()
            .clients()
            .connections(
                &ConnectionQuery {
                    limit: 10,
                    ip_prefix: None,
                    listener: None,
                },
                StdInstant::now(),
            )
            .connections
    }

    fn clients(&self) -> Vec<ClientRow> {
        self.generation
            .metrics()
            .clients()
            .clients(
                &ClientQuery {
                    limit: 10,
                    sort: Sort::Bytes,
                    ip_prefix: None,
                },
                StdInstant::now(),
            )
            .clients
    }

    async fn open_with(&self, probe: impl Fn(&ConnectionRow) -> bool) -> ConnectionRow {
        wait_for(|| self.live().into_iter().find(&probe)).await
    }

    async fn closed(&self) -> ClientRow {
        wait_for(|| {
            let clients = self.clients();
            (self.live().is_empty() && clients.len() == 1).then(|| clients[0].clone())
        })
        .await
    }
}

async fn wait_for<T>(mut probe: impl FnMut() -> Option<T>) -> T {
    timeout(Duration::from_secs(10), async {
        loop {
            if let Some(value) = probe() {
                break value;
            }
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("tracked HTTP/3 state timeout")
}

fn document(config: ConfigDraft) -> ResolvedConfigDocument {
    ResolvedConfigDocument {
        authored_revision: AuthoredRevision::from_bytes(b"h3-test"),
        effective_revision: EffectiveRevision::from_bytes(b"h3-test"),
        validated_config: config.validate().expect("valid HTTP/3 test config"),
        format: ConfigFormat::Lua,
        compositional: false,
        dependencies: Vec::new(),
        config_preview: String::new(),
        diagnostics: Vec::new(),
    }
}

fn free_udp_address() -> SocketAddr {
    StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("listener probe")
        .local_addr()
        .expect("listener address")
}

fn base_config(key: &Key, listener: SocketAddr, protocol: Protocol, name: &str) -> ConfigDraft {
    let mut config =
        serde_json::from_value::<ConfigDraft>(serde_json::json!({"version": 1, "listeners": []}))
            .expect("base config");
    config.certificates.push(Certificate {
        name: "downstream".into(),
        dns_names: vec![SERVER_NAME.into()],
        source: CertificateSource::Files {
            certificate_chain_path: format!("{FIXTURES}/proxy-a.pem").into(),
            private_key_path: key.path.clone(),
        },
    });
    config.tls_profiles.push(TlsProfile {
        name: "downstream".into(),
        certificates: vec!["downstream".into()],
        default_certificate: "downstream".into(),
        min_version: TlsVersion::Tls13,
        alpn: vec![AlpnProtocol::H3],
        policy: oxiroute_config::TlsPolicy::default(),
    });
    config.listeners.push(Listener {
        name: name.into(),
        bind: ListenerBind::Udp { address: listener },
        protocol,
        service: Some(name.into()),
        tls_profile: Some("downstream".into()),
        proxy_protocol: None,
        max_connections: Some(8),
        downstream_timeouts: DownstreamTimeoutPolicy::default(),
        h2_initial_stream_window: None,
        h2_initial_connection_window: None,
    });
    config
}

fn reverse_config(key: &Key, listener: SocketAddr, origin: SocketAddr) -> ConfigDraft {
    let mut config = base_config(key, listener, Protocol::Http3, "reverse");
    config.upstream_pools.push(UpstreamPool {
        name: "origin".into(),
        servers: Vec::new(),
        endpoints: vec![UpstreamEndpoint::Socket { address: origin }],
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: None,
        passive_health: None,
        tls: None,
        http_versions: HttpVersionPolicy::default(),
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: UpstreamConnectionReuse::default(),
        registration: None,
        canary: None,
        resilience: oxiroute_config::PoolResilience::default(),
        queue: None,
    });
    let route = |path: HttpPathSelector, action: HttpRouteAction| HttpRoute {
        host: None,
        path,
        methods: Vec::new(),
        access_policy: None,
        policy: HttpRoutePolicy {
            max_request_body_bytes: Some(64 * 1024),
            request_buffering: true,
            ..HttpRoutePolicy::default()
        },
        action,
    };
    config.http_services.push(HttpService {
        drain_deadline_ms: None,
        name: "reverse".into(),
        routes: vec![
            route(
                HttpPathSelector::Exact {
                    value: "/fixed".into(),
                },
                HttpRouteAction::FixedResponse {
                    status: 200,
                    body: "ok".into(),
                    headers: Vec::new(),
                },
            ),
            route(
                HttpPathSelector::SegmentPrefix { value: "/".into() },
                HttpRouteAction::Proxy {
                    upstream_pool: "origin".into(),
                    policy: HttpProxyPolicy::default().into(),
                },
            ),
        ],
        automatic_response_headers: true,
        upstream_io_timeout_ms: 5_000,
        max_request_body_bytes: Some(64 * 1024),
        gzip: None,
        access_log: None,
    });
    config
}

fn forward_config(key: &Key, listener: SocketAddr, origin: SocketAddr) -> ConfigDraft {
    let mut config = base_config(key, listener, Protocol::ForwardHttp3, "forward");
    config.forward_proxy_services.push(ForwardProxyService {
        drain_deadline_ms: None,
        name: "forward".into(),
        enabled_versions: vec![ForwardHttpVersion::H3],
        allow_absolute_form: true,
        tls_required: true,
        connect: ForwardConnectPolicy {
            enabled: true,
            allowed_ports: vec![origin.port()],
        },
        connect_udp: ForwardConnectPolicy::default(),
        peer_policy: ForwardPeerPolicy::default(),
        auth: None,
        access_policy: None,
        destination_policy: ForwardDestinationPolicy {
            deny_private: false,
            ..ForwardDestinationPolicy::default()
        },
        header_policy: ForwardHeaderPolicy::default(),
        connect_timeout_ms: 1_000,
        idle_timeout_ms: 5_000,
        lifetime_timeout_ms: 10_000,
        max_request_body_bytes: Some(64 * 1024),
        max_header_bytes: 8_192,
        max_connections: 4,
        resolver: ForwardResolverPolicy::default(),
        audit_mode: ForwardAuditMode::Off,
    });
    config
}

async fn origin() -> SocketAddr {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("origin bind");
    let address = listener.local_addr().expect("origin address");
    tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.expect("origin accept");
            tokio::spawn(serve_origin(socket));
        }
    });
    address
}

async fn serve_origin(mut socket: TcpStream) {
    let mut received = Vec::new();
    let mut buffer = [0_u8; 1024];
    let read = socket.read(&mut buffer).await.expect("origin read");
    received.extend_from_slice(&buffer[..read]);
    if !received.starts_with(b"GET ") && !received.starts_with(b"POST ") {
        while received.len() < 4 {
            let read = socket.read(&mut buffer).await.expect("tunnel read");
            received.extend_from_slice(&buffer[..read]);
        }
        socket.write_all(b"pong").await.expect("tunnel reply");
        return;
    }
    let head_end = loop {
        if let Some(position) = received.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        let read = socket.read(&mut buffer).await.expect("origin head");
        received.extend_from_slice(&buffer[..read]);
    };
    if received.starts_with(b"GET /broken") {
        return;
    }
    let head = String::from_utf8_lossy(&received[..head_end]).to_ascii_lowercase();
    let length = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .map_or(0, |value| value.trim().parse::<usize>().expect("length"));
    while received.len() < head_end + length {
        let read = socket.read(&mut buffer).await.expect("origin body");
        received.extend_from_slice(&buffer[..read]);
    }
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        ORIGIN_BODY.len()
    );
    socket
        .write_all(response.as_bytes())
        .await
        .expect("origin head reply");
    socket.write_all(ORIGIN_BODY).await.expect("origin reply");
}

struct Client {
    endpoint: quinn::Endpoint,
    connection: quinn::Connection,
    sender: Sender,
}

impl Client {
    async fn connect(listener: SocketAddr) -> Self {
        let mut roots = rustls::RootCertStore::empty();
        let ca = fs::read(format!("{FIXTURES}/ca-a.pem")).expect("CA fixture");
        for certificate in CertificateDer::pem_slice_iter(&ca) {
            roots.add(certificate.expect("CA certificate")).expect("CA");
        }
        let mut crypto = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("TLS versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
        crypto.alpn_protocols = vec![b"h3".to_vec()];
        let mut endpoint =
            quinn::Endpoint::client((Ipv4Addr::LOCALHOST, 0).into()).expect("client endpoint");
        endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(
            QuicClientConfig::try_from(crypto).expect("QUIC client config"),
        )));
        let connection = timeout(
            Duration::from_secs(10),
            endpoint.connect(listener, SERVER_NAME).expect("connect"),
        )
        .await
        .expect("QUIC handshake timeout")
        .expect("QUIC handshake");
        let (mut driver, sender) = h3::client::new(h3_quinn::Connection::new(connection.clone()))
            .await
            .expect("H3 client");
        tokio::spawn(async move {
            let _ = std::future::poll_fn(|context| driver.poll_close(context)).await;
        });
        Self {
            endpoint,
            connection,
            sender,
        }
    }

    async fn request(
        &mut self,
        method: Method,
        uri: &str,
        body: &'static [u8],
    ) -> (StatusCode, Vec<u8>) {
        let mut head = Request::builder().method(method).uri(uri);
        if !body.is_empty() {
            head = head.header("content-length", body.len());
        }
        let mut stream = self
            .sender
            .send_request(head.body(()).expect("request"))
            .await
            .expect("send request");
        if !body.is_empty() {
            stream
                .send_data(Bytes::from_static(body))
                .await
                .expect("request body");
        }
        let _ = stream.finish().await;
        let status = stream.recv_response().await.expect("response").status();
        let mut received = Vec::new();
        while let Some(mut chunk) = stream.recv_data().await.expect("response body") {
            received.extend_from_slice(&chunk.copy_to_bytes(chunk.remaining()));
        }
        (status, received)
    }

    fn close(self) {
        self.connection.close(0_u32.into(), b"done");
    }
}

fn totals(row: &ClientRow) -> (u64, u64, u64, u64, u64, u64) {
    (
        row.connections,
        row.connections_total,
        row.requests,
        row.errors,
        row.bytes_in,
        row.bytes_out,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reverse_h3_connection_is_tracked_with_its_tls_traffic_and_requests() {
    let origin = origin().await;
    let listener = free_udp_address();
    let key = Key::new();
    let config = reverse_config(&key, listener, origin);
    let harness = Harness::start(config, key, listener, "reverse");
    let mut client = Client::connect(harness.listener).await;

    let (status, body) = client
        .request(
            Method::POST,
            "https://proxy.example.test/echo",
            b"ping-pong",
        )
        .await;
    assert_eq!((status, body.as_slice()), (StatusCode::OK, ORIGIN_BODY));
    let (status, _) = client
        .request(Method::GET, "https://proxy.example.test/fixed", b"")
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = client
        .request(Method::GET, "https://proxy.example.test/broken", b"")
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);

    let open = harness.open_with(|row| row.requests == 3).await;
    assert_eq!(open.protocol.as_str(), "h3");
    assert_eq!(&*open.listener, "reverse");
    assert_eq!(
        open.peer,
        client.endpoint.local_addr().expect("client address")
    );
    assert!(!open.handshaking);
    let tls = open.tls.expect("TLS facts of the QUIC connection");
    assert_eq!(
        (tls.version, tls.sni.as_deref(), tls.alpn.as_deref()),
        ("TLS1.3", Some(SERVER_NAME), Some("h3"))
    );
    assert_eq!((open.bytes_in, open.bytes_out), (9, 5));
    assert_eq!(open.upstream.as_deref(), Some("endpoint-1"));

    client.close();
    let row = harness.closed().await;
    assert_eq!(totals(&row), (0, 1, 3, 1, 9, 5));
    assert_eq!(row.protocols, ["h3"]);
    harness.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_forward_h3_connection_is_tracked_across_requests_and_tunnels() {
    let origin = origin().await;
    let listener = free_udp_address();
    let key = Key::new();
    let config = forward_config(&key, listener, origin);
    let harness = Harness::start(config, key, listener, "forward");
    let mut client = Client::connect(harness.listener).await;

    let uri = format!("http://{origin}/h3");
    let (status, body) = client.request(Method::POST, &uri, b"payload").await;
    assert_eq!((status, body.as_slice()), (StatusCode::OK, ORIGIN_BODY));
    let open = harness.open_with(|row| row.requests == 1).await;
    assert_eq!(open.protocol.as_str(), "h3");
    assert_eq!(&*open.listener, "forward");
    assert_eq!((open.bytes_in, open.bytes_out), (7, 5));
    assert_eq!(open.tls.expect("TLS facts").alpn.as_deref(), Some("h3"));

    let mut tunnel = client
        .sender
        .send_request(
            Request::builder()
                .method(Method::CONNECT)
                .uri(origin.to_string())
                .body(())
                .expect("CONNECT request"),
        )
        .await
        .expect("send CONNECT");
    assert_eq!(
        tunnel
            .recv_response()
            .await
            .expect("CONNECT response")
            .status(),
        StatusCode::OK
    );
    tunnel
        .send_data(Bytes::from_static(b"ping"))
        .await
        .expect("tunnel payload");
    let mut reply = tunnel
        .recv_data()
        .await
        .expect("tunnel reply")
        .expect("tunnel data");
    assert_eq!(reply.copy_to_bytes(reply.remaining()).as_ref(), b"pong");
    tunnel.finish().await.expect("finish tunnel");
    let open = harness.open_with(|row| row.requests == 2).await;
    assert_eq!((open.bytes_in, open.bytes_out), (11, 9));

    client.close();
    let row = harness.closed().await;
    assert_eq!(totals(&row), (0, 1, 2, 0, 11, 9));
    harness.stop();
}
