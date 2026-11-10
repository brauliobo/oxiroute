#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn management(address: SocketAddr) -> Management {
    Management {
        bind: address,
        ui_dir: None,
    }
}

pub(super) fn management_config_only(address: SocketAddr) -> ConfigDraft {
    ConfigDraft {
        listener_workers: None,
        connection_scheduling: None,
        connection_age_ms: None,
        retirement: None,
        management: Some(management(address)),
        ..empty_config()
    }
}

pub(super) fn http_config(
    management_address: SocketAddr,
    listener_address: SocketAddr,
    body: &str,
) -> ConfigDraft {
    ConfigDraft {
        listener_workers: None,
        connection_scheduling: None,
        connection_age_ms: None,
        retirement: None,
        management: Some(management(management_address)),
        listeners: vec![Listener {
            name: "http".into(),
            bind: socket_bind(listener_address),
            protocol: Protocol::Http,
            service: Some("web".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: Some(8),
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
        }],
        http_services: vec![HttpService {
            drain_deadline_ms: None,
            name: "web".into(),
            routes: vec![HttpRoute {
                host: None,
                path: HttpPathSelector::SegmentPrefix { value: "/".into() },
                methods: Vec::new(),
                access_policy: None,
                policy: HttpRoutePolicy::default(),
                action: HttpRouteAction::FixedResponse {
                    status: 200,
                    body: body.into(),
                    headers: Vec::new(),
                },
            }],
            automatic_response_headers: true,
            upstream_io_timeout_ms: 1_000,
            max_request_body_bytes: Some(1_024),
            gzip: None,
            access_log: None,
        }],
        ..empty_config()
    }
}

pub(super) fn forward_h2_config(
    management_address: SocketAddr,
    listener_address: SocketAddr,
    origin_address: SocketAddr,
    private_key_path: &Path,
) -> ConfigDraft {
    ConfigDraft {
        listener_workers: None,
        connection_scheduling: None,
        connection_age_ms: None,
        retirement: None,
        management: Some(management(management_address)),
        certificates: vec![Certificate {
            name: "downstream".into(),
            dns_names: vec![wire_support::PROXY_SERVER_NAME.into()],
            source: CertificateSource::Files {
                certificate_chain_path: fixture_support::fixture("proxy-a.pem"),
                private_key_path: private_key_path.to_path_buf(),
            },
        }],
        tls_profiles: vec![TlsProfile {
            name: "downstream".into(),
            certificates: vec!["downstream".into()],
            default_certificate: "downstream".into(),
            min_version: TlsVersion::Tls12,
            alpn: vec![AlpnProtocol::H2],
            policy: oxiroute_config::TlsPolicy::default(),
        }],
        forward_proxy_services: vec![ForwardProxyService {
            drain_deadline_ms: None,
            name: "forward".into(),
            enabled_versions: vec![ForwardHttpVersion::H2],
            allow_absolute_form: false,
            tls_required: true,
            connect: ForwardConnectPolicy {
                enabled: true,
                allowed_ports: vec![origin_address.port()],
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
            idle_timeout_ms: 1_000,
            lifetime_timeout_ms: 5_000,
            max_request_body_bytes: Some(64 * 1024),
            max_header_bytes: 8_192,
            max_connections: 4,
            resolver: ForwardResolverPolicy::default(),
            audit_mode: ForwardAuditMode::Off,
        }],
        listeners: vec![Listener {
            name: "forward".into(),
            bind: socket_bind(listener_address),
            protocol: Protocol::ForwardHttp2,
            service: Some("forward".into()),
            tls_profile: Some("downstream".into()),
            proxy_protocol: None,
            max_connections: None,
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
        }],
        ..empty_config()
    }
}

pub(super) fn tcp_config(
    management_address: SocketAddr,
    listener_address: SocketAddr,
    upstream_address: SocketAddr,
) -> ConfigDraft {
    ConfigDraft {
        listener_workers: None,
        connection_scheduling: None,
        connection_age_ms: None,
        retirement: None,
        management: Some(management(management_address)),
        listeners: vec![Listener {
            name: "tcp".into(),
            bind: socket_bind(listener_address),
            protocol: Protocol::Tcp,
            service: Some("relay".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: Some(8),
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
        }],
        upstream_pools: vec![UpstreamPool {
            name: "upstream".into(),
            servers: Vec::new(),
            endpoints: vec![socket_endpoint(upstream_address)],
            algorithm: UpstreamAlgorithm::RoundRobin,
            health_check: None,
            passive_health: None,
            tls: None,
            http_versions: HttpVersionPolicy::default(),
            queue_timeout_ms: None,
            connect_timeout_ms: Some(1_000),
            server_timeout_ms: None,
            connection_reuse: UpstreamConnectionReuse::default(),
            registration: None,
            canary: None,
            resilience: oxiroute_config::PoolResilience::default(),
            queue: None,
        }],
        l4_services: vec![L4Service {
            drain_deadline_ms: None,
            name: "relay".into(),
            upstream_pool: "upstream".into(),
            connect_timeout_ms: 1_000,
            idle_timeout_ms: 30_000,
            lifetime_timeout_ms: None,
            proxy_protocol: None,
            udp: None,
        }],
        ..empty_config()
    }
}

pub(super) fn rtmp_config(
    management_address: SocketAddr,
    listener_address: SocketAddr,
) -> ConfigDraft {
    ConfigDraft {
        listener_workers: None,
        connection_scheduling: None,
        connection_age_ms: None,
        retirement: None,
        management: Some(management(management_address)),
        listeners: vec![Listener {
            name: "rtmp".into(),
            bind: socket_bind(listener_address),
            protocol: Protocol::Rtmp,
            service: Some("live".into()),
            tls_profile: None,
            proxy_protocol: None,
            max_connections: Some(8),
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
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
                recorders: Vec::new(),
            }],
        }],
        ..empty_config()
    }
}

pub(super) fn fixed_response_body(config: &mut ConfigDraft, body: &str) {
    let HttpRouteAction::FixedResponse { body: target, .. } =
        &mut config.http_services[0].routes[0].action
    else {
        panic!("test route is not a fixed response");
    };
    *target = body.into();
}

pub(super) async fn active_revision(address: SocketAddr, authorization: &str) -> String {
    http_request(
        address,
        "GET",
        "/api/v1/status",
        &[("Authorization", authorization)],
        &[],
    )
    .await
    .json()["activeRevision"]
        .as_str()
        .expect("active revision")
        .to_owned()
}

pub(super) async fn active_revision_from_persistent(stream: &mut TcpStream) -> String {
    let authorization = format!("Bearer {TOKEN}");
    persistent_json_request(
        stream,
        "GET",
        "/api/v1/status",
        &authorization,
        &Value::Null,
    )
    .await
    .json()["activeRevision"]
        .as_str()
        .expect("persistent active revision")
        .to_owned()
}

pub(super) async fn wait_for_new_revision(
    address: SocketAddr,
    authorization: &str,
    original: &str,
) {
    timeout(WIRE_TIMEOUT, async {
        loop {
            if active_revision(address, authorization).await != original {
                return;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("generation reload timed out");
}

pub(super) async fn generation_drain(
    address: SocketAddr,
    authorization: &str,
    revision: &str,
) -> HttpResponse {
    let body = serde_json::to_vec(&json!({
        "expectedActiveRevision": revision,
        "timeoutMs": 750,
    }))
    .expect("generation drain JSON");
    http_request(
        address,
        "POST",
        "/api/v1/generations/drain",
        &[
            ("Authorization", authorization),
            ("Content-Type", "application/json"),
        ],
        &body,
    )
    .await
}

pub(super) async fn persistent_request(
    stream: &mut TcpStream,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> HttpResponse {
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\n");
    for (name, value) in headers {
        writeln!(request, "{name}: {value}\r").expect("persistent request header");
    }
    if !body.is_empty() || matches!(method, "POST" | "PUT") {
        writeln!(request, "Content-Length: {}\r", body.len()).expect("persistent request length");
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("persistent request head");
    stream
        .write_all(body)
        .await
        .expect("persistent request body");
    read_content_length_response(stream).await
}

pub(super) async fn persistent_json_request(
    stream: &mut TcpStream,
    method: &str,
    path: &str,
    authorization: &str,
    body: &Value,
) -> HttpResponse {
    let body_bytes = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(body).expect("persistent JSON body")
    };
    persistent_request(
        stream,
        method,
        path,
        &[
            ("Authorization", authorization),
            ("Content-Type", "application/json"),
        ],
        &body_bytes,
    )
    .await
}

pub(super) async fn read_content_length_response(stream: &mut TcpStream) -> HttpResponse {
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
    HttpResponse::parse(response)
}

pub(super) async fn assert_no_http_response(address: SocketAddr) {
    let outcome = timeout(DRAIN_PROBE_TIMEOUT, async {
        let mut stream = TcpStream::connect(address).await?;
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        Ok::<_, io::Error>(response)
    })
    .await;
    if let Ok(Ok(response)) = outcome {
        assert!(
            !response
                .windows(b"HTTP/1.1 200".len())
                .any(|part| part == b"HTTP/1.1 200"),
            "new HTTP admission produced a response after drain: {}",
            String::from_utf8_lossy(&response)
        );
    }
}

pub(super) async fn start_echo_upstream() -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("echo upstream bind");
    let address = listener.local_addr().expect("echo upstream address");
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (mut stream, _) = accepted.expect("echo upstream accept");
                    connections.spawn(async move {
                        let mut buffer = [0_u8; 1024];
                        loop {
                            let count = stream.read(&mut buffer).await.expect("echo upstream read");
                            if count == 0 {
                                return;
                            }
                            stream
                                .write_all(&buffer[..count])
                                .await
                                .expect("echo upstream write");
                        }
                    });
                }
                Some(result) = connections.join_next(), if !connections.is_empty() => {
                    result.expect("echo upstream connection task");
                }
            }
        }
    });
    (address, task)
}

pub(super) async fn assert_echo(stream: &mut TcpStream, payload: &[u8]) {
    stream.write_all(payload).await.expect("TCP relay write");
    let mut response = vec![0_u8; payload.len()];
    timeout(WIRE_TIMEOUT, stream.read_exact(&mut response))
        .await
        .unwrap_or_else(|_| panic!("TCP relay response timeout for {payload:?}"))
        .expect("TCP relay response");
    assert_eq!(response, payload);
}

pub(super) async fn assert_no_tcp_echo(address: SocketAddr, payload: &[u8]) {
    let outcome = timeout(DRAIN_PROBE_TIMEOUT, async {
        let mut stream = TcpStream::connect(address).await?;
        stream.write_all(payload).await?;
        let mut response = vec![0_u8; payload.len()];
        stream.read_exact(&mut response).await
    })
    .await;
    assert!(
        !matches!(outcome, Ok(Ok(_))),
        "new TCP admission produced an echo after drain"
    );
}

pub(super) async fn assert_no_rtmp_handshake(address: SocketAddr) {
    let outcome = timeout(DRAIN_PROBE_TIMEOUT, async {
        let mut stream = TcpStream::connect(address).await?;
        let mut handshake = Handshake::new(PeerType::Client);
        let hello = handshake
            .generate_outbound_p0_and_p1()
            .map_err(io::Error::other)?;
        stream.write_all(&hello).await?;
        let mut response = vec![0_u8; 3_073];
        stream.read_exact(&mut response).await
    })
    .await;
    assert!(
        !matches!(outcome, Ok(Ok(_))),
        "new RTMP admission produced a handshake after drain"
    );
}

pub(super) fn assert_listener_released<const N: usize>(addresses: [SocketAddr; N]) {
    for address in addresses {
        let listener = std::net::TcpListener::bind(address)
            .unwrap_or_else(|error| panic!("listener {address} was not released: {error}"));
        drop(listener);
    }
}
