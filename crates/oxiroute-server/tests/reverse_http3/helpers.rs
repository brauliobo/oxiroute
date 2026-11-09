#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn reserve_udp_address() -> SocketAddr {
    process_support::ports::unused_address()
}

pub(super) fn reverse_config(
    listener_address: SocketAddr,
    key_path: &Path,
    service: HttpService,
    upstream_pools: Vec<UpstreamPool>,
    max_connections: Option<u64>,
) -> ConfigDraft {
    let mut config = support::empty_config();
    config.certificates.push(Certificate {
        name: "downstream".into(),
        dns_names: vec![support::PROXY_SERVER_NAME.into()],
        source: CertificateSource::Files {
            certificate_chain_path: fixture_support::fixture("proxy-a.pem"),
            private_key_path: key_path.to_path_buf(),
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
    config.upstream_pools = upstream_pools;
    config.http_services.push(service);
    config.listeners.push(Listener {
        name: "reverse".into(),
        bind: ListenerBind::Udp {
            address: listener_address,
        },
        protocol: Protocol::Http3,
        service: Some("web".into()),
        tls_profile: Some("downstream".into()),
        proxy_protocol: None,
        max_connections,
        downstream_timeouts: DownstreamTimeoutPolicy::default(),
    });
    config
}

pub(super) fn fixed_service() -> HttpService {
    reverse_service(HttpRouteAction::FixedResponse {
        status: 200,
        body: "ok".into(),
        headers: Vec::new(),
    })
}

pub(super) fn proxy_service(max_request_body_bytes: u64) -> HttpService {
    let mut service = reverse_service(HttpRouteAction::Proxy {
        upstream_pool: "origin".into(),
        policy: HttpProxyPolicy::default().into(),
    });
    service.max_request_body_bytes = Some(max_request_body_bytes);
    service.routes[0].policy.max_request_body_bytes = Some(max_request_body_bytes);
    service
}

pub(super) fn reverse_service(action: HttpRouteAction) -> HttpService {
    HttpService {
        drain_deadline_ms: None,
        name: "web".into(),
        routes: vec![HttpRoute {
            host: None,
            path: HttpPathSelector::SegmentPrefix { value: "/".into() },
            methods: Vec::new(),
            access_policy: None,
            policy: HttpRoutePolicy {
                max_request_body_bytes: Some(64 * 1024),
                request_buffering: true,
                ..HttpRoutePolicy::default()
            },
            action,
        }],
        automatic_response_headers: true,
        upstream_io_timeout_ms: 5_000,
        max_request_body_bytes: Some(64 * 1024),
        gzip: None,
        access_log: None,
    }
}

pub(super) fn static_service(root: PathBuf) -> HttpService {
    reverse_service(HttpRouteAction::StaticFiles {
        root_directory: root,
        path_mapping: HttpStaticPathMapping::Root,
        index_files: vec!["index.html".into()],
        internal_index_redirects: true,
        directory_redirects: true,
        spa_fallback: None,
        try_files: Vec::new(),
        autoindex: false,
        autoindex_exact_size: true,
        autoindex_local_time: false,
        etag: true,
        mime: HttpStaticMimePolicy {
            default_type: Some("application/octet-stream".into()),
            types: Vec::new(),
        },
        headers: Vec::new(),
        error_responses: Vec::new(),
    })
}

pub(super) fn h3_pool(address: SocketAddr) -> UpstreamPool {
    UpstreamPool {
        name: "origin".into(),
        servers: Vec::new(),
        endpoints: vec![support::socket_endpoint(address)],
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: None,
        passive_health: None,
        tls: Some(UpstreamTls {
            server_name: support::ORIGIN_SERVER_NAME.into(),
            ca_certificate_path: Some(fixture_support::fixture("ca-a.pem")),
        }),
        http_versions: HttpVersionPolicy {
            min: HttpVersion::Http3,
            max: HttpVersion::Http3,
        },
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
        registration: None,
        canary: None,
        resilience: oxiroute_config::PoolResilience::default(),
        queue: None,
    }
}

pub(super) fn fixed_request(path: &str) -> Request<()> {
    Request::builder()
        .method(Method::GET)
        .uri(format!("https://example.test{path}"))
        .body(())
        .expect("fixed H3 request")
}

pub(super) async fn connect_h3(
    endpoint: &quinn::Endpoint,
    listener_address: SocketAddr,
) -> quinn::Connection {
    timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(connecting) = endpoint.connect(listener_address, support::PROXY_SERVER_NAME)
                && let Ok(connection) = connecting.await
            {
                break connection;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("H3 daemon connection timeout")
}

pub(super) fn client_endpoint() -> std::io::Result<quinn::Endpoint> {
    let mut roots = rustls::RootCertStore::empty();
    let ca = fs::read(fixture_support::fixture("ca-a.pem"))?;
    for certificate in CertificateDer::pem_slice_iter(&ca) {
        roots
            .add(certificate.map_err(std::io::Error::other)?)
            .map_err(std::io::Error::other)?;
    }
    let mut crypto = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    crypto.alpn_protocols = vec![H3_ALPN.to_vec()];
    let config = quinn::ClientConfig::new(Arc::new(
        QuicClientConfig::try_from(crypto).map_err(std::io::Error::other)?,
    ));
    let mut endpoint = quinn::Endpoint::client((Ipv4Addr::LOCALHOST, 0).into())?;
    endpoint.set_default_client_config(config);
    Ok(endpoint)
}

pub(super) async fn recv_chunk<S>(stream: &mut RequestStream<S, Bytes>) -> Bytes
where
    S: h3::quic::BidiStream<Bytes>,
{
    let mut chunk = stream
        .recv_data()
        .await
        .expect("H3 response body")
        .expect("H3 response data");
    chunk.copy_to_bytes(chunk.remaining())
}

pub(super) async fn recv_body<S>(stream: &mut RequestStream<S, Bytes>) -> Bytes
where
    S: h3::quic::RecvStream,
{
    let mut body = Vec::new();
    while let Some(mut chunk) = stream.recv_data().await.expect("H3 response body") {
        let length = chunk.remaining();
        body.extend_from_slice(&chunk.copy_to_bytes(length));
    }
    Bytes::from(body)
}

pub(super) fn drive_client<C>(
    mut driver: h3::client::Connection<C, Bytes>,
) -> tokio::task::JoinHandle<()>
where
    C: h3::quic::Connection<Bytes> + Send + 'static,
    C::SendStream: Send,
    C::RecvStream: Send,
{
    tokio::spawn(async move {
        let _ = std::future::poll_fn(|context| driver.poll_close(context)).await;
    })
}
