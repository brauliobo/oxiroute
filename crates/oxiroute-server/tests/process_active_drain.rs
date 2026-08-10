#![allow(dead_code, unused_imports, clippy::duplicate_mod)]

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/http.rs"]
mod http_support;
#[path = "support/process.rs"]
mod process_support;
#[path = "support/rtmp.rs"]
mod rtmp_support;
#[path = "support/sse.rs"]
mod sse_support;
#[path = "support/mod.rs"]
mod wire_support;

use std::{
    error::Error,
    fmt::Write as _,
    io,
    net::{Ipv4Addr, SocketAddr},
    path::Path,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use bytes::{Bytes, BytesMut};
use http::{Method, Request, StatusCode};
use oxiroute_config::{
    AlpnProtocol, Certificate, CertificateSource, Config, DownstreamTimeoutPolicy,
    ForwardAuditMode, ForwardConnectPolicy, ForwardDestinationPolicy, ForwardHeaderPolicy,
    ForwardHttpVersion, ForwardPeerPolicy, ForwardProxyService, ForwardResolverPolicy,
    HttpPathSelector, HttpRoute, HttpRouteAction, HttpRoutePolicy, HttpService, HttpVersionPolicy,
    L4Service, Listener, Management, Protocol, RtmpApplication, RtmpService, TlsProfile,
    TlsVersion, UpstreamAlgorithm, UpstreamConnectionReuse, UpstreamPool,
};
use rml_rtmp::handshake::{Handshake, PeerType};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore},
    task::{JoinHandle, JoinSet},
    time::{sleep, timeout},
};

use config_support::{empty_config, socket_bind, socket_endpoint};
use http_support::{HttpResponse, http_request};
use process_support::{ServerProcess, reserve_tcp_address, write_config};
use rtmp_support::RtmpWireClient;
use sse_support::{open_event_stream, read_chunk};

const TOKEN: &str = "7a89c4b6cefd8b4c11b6b4f9d1e6b5d0e8f1a2c3d4e5f60718293a4b5c6d7e8f";
const WIRE_TIMEOUT: Duration = Duration::from_secs(10);
const DRAIN_PROBE_TIMEOUT: Duration = Duration::from_millis(750);
static PROCESS_DRAIN_TEST_GATE: OnceLock<Arc<Semaphore>> = OnceLock::new();

type BoxError = Box<dyn Error + Send + Sync>;

async fn process_drain_test_guard() -> OwnedSemaphorePermit {
    PROCESS_DRAIN_TEST_GATE
        .get_or_init(|| Arc::new(Semaphore::new(1)))
        .clone()
        .acquire_owned()
        .await
        .expect("process drain test gate remains open")
}

#[tokio::test]
async fn http_reload_retains_the_old_keepalive_and_drain_rejects_new_admissions() {
    let _test_guard = process_drain_test_guard().await;
    let management_address = reserve_tcp_address();
    let listener_address = reserve_tcp_address();
    let initial = http_config(management_address, listener_address, "old-http");
    let mut server = ServerProcess::start(&initial, Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(listener_address).await;

    let authorization = format!("Bearer {TOKEN}");
    let mut old_connection = TcpStream::connect(listener_address)
        .await
        .expect("old HTTP connection");
    assert_eq!(
        persistent_request(&mut old_connection, "GET", "/", &[], &[])
            .await
            .body(),
        b"old-http"
    );
    let original_revision = active_revision(management_address, &authorization).await;

    let mut candidate = initial.clone();
    fixed_response_body(&mut candidate, "new-http");
    write_config(&server.config_path, &candidate);
    wait_for_new_revision(management_address, &authorization, &original_revision).await;

    assert_eq!(
        persistent_request(&mut old_connection, "GET", "/", &[], &[])
            .await
            .body(),
        b"old-http",
        "the established HTTP connection changed generations"
    );
    let mut new_connection = TcpStream::connect(listener_address)
        .await
        .expect("new HTTP connection");
    assert_eq!(
        persistent_request(&mut new_connection, "GET", "/", &[], &[])
            .await
            .body(),
        b"new-http"
    );
    drop(new_connection);

    let revision = active_revision(management_address, &authorization).await;
    let drain = generation_drain(management_address, &authorization, &revision).await;
    assert_eq!(drain.status, 202, "{}", drain.text());
    assert_eq!(drain.json()["outcome"], "draining");
    assert_no_http_response(listener_address).await;

    assert_eq!(
        persistent_request(&mut old_connection, "GET", "/", &[], &[])
            .await
            .body(),
        b"old-http",
        "draining the active generation closed the retained old connection"
    );
    drop(old_connection);
    server.shutdown();
    assert_listener_released([management_address, listener_address]);
}

#[tokio::test]
async fn h2_reload_sends_goaway_while_the_candidate_serves_new_connections() {
    let _test_guard = process_drain_test_guard().await;
    let management_address = reserve_tcp_address();
    let listener_address = reserve_tcp_address();
    let mut initial = wire_support::proxy_config(
        listener_address,
        reserve_tcp_address(),
        vec![oxiroute_config::AlpnProtocol::H2],
        None,
        HttpVersionPolicy::default(),
    );
    initial.management = Some(management(management_address));
    initial.http_services[0].routes[0].action = HttpRouteAction::FixedResponse {
        status: 200,
        body: "old-h2".into(),
        headers: Vec::new(),
    };
    fixed_response_body(&mut initial, "old-h2");
    let mut server = ServerProcess::start(&initial, Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(listener_address).await;

    let mut old_connection = H2Connection::try_connect(listener_address)
        .await
        .expect("old H2 connection");
    assert_eq!(
        old_connection
            .get()
            .await
            .expect("old H2 response")
            .as_ref(),
        b"old-h2"
    );
    let authorization = format!("Bearer {TOKEN}");
    let original_revision = active_revision(management_address, &authorization).await;

    let mut candidate = initial;
    fixed_response_body(&mut candidate, "new-h2");
    write_config(&server.config_path, &candidate);
    wait_for_new_revision(management_address, &authorization, &original_revision).await;

    let old_request = timeout(WIRE_TIMEOUT, old_connection.get())
        .await
        .expect("old H2 request remained pending after GOAWAY");
    assert!(
        old_request.is_err(),
        "old H2 connection accepted a stream after generation quiesce"
    );

    let mut new_connection = H2Connection::try_connect(listener_address)
        .await
        .expect("candidate H2 connection");
    assert_eq!(
        new_connection
            .get()
            .await
            .expect("candidate H2 response")
            .as_ref(),
        b"new-h2"
    );
    new_connection.finish().await;
    old_connection.finish().await;

    server.shutdown();
    assert_listener_released([management_address, listener_address]);
}

#[tokio::test]
async fn forward_h2_reload_sends_goaway_while_the_candidate_serves_new_connections() {
    let _test_guard = process_drain_test_guard().await;
    let management_address = reserve_tcp_address();
    let listener_address = reserve_tcp_address();
    let (origin_address, origin_task) = start_echo_upstream().await;
    let private_key = fixture_support::private_key_fixture("proxy-a-key.pem");
    let mut initial = forward_h2_config(
        management_address,
        listener_address,
        origin_address,
        private_key.path(),
    );
    let mut server = ServerProcess::start(&initial, Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(listener_address).await;

    let mut old_connection = H2Connection::try_connect(listener_address)
        .await
        .expect("old forward H2 connection");
    assert_eq!(
        old_connection
            .connect(origin_address, b"old-forward-h2")
            .await
            .expect("old forward H2 tunnel")
            .as_ref(),
        b"old-forward-h2"
    );
    let authorization = format!("Bearer {TOKEN}");
    let original_revision = active_revision(management_address, &authorization).await;

    initial.max_connections = Some(17);
    write_config(&server.config_path, &initial);
    wait_for_new_revision(management_address, &authorization, &original_revision).await;

    let old_request = timeout(
        WIRE_TIMEOUT,
        old_connection.connect(origin_address, b"old-after-reload"),
    )
    .await
    .expect("old forward H2 request remained pending after GOAWAY");
    assert!(
        old_request.is_err(),
        "old forward H2 connection accepted a stream after generation quiesce"
    );

    let mut new_connection = H2Connection::try_connect(listener_address)
        .await
        .expect("candidate forward H2 connection");
    assert_eq!(
        new_connection
            .connect(origin_address, b"new-forward-h2")
            .await
            .expect("candidate forward H2 tunnel")
            .as_ref(),
        b"new-forward-h2"
    );
    new_connection.finish().await;
    old_connection.finish().await;

    server.shutdown();
    origin_task.abort();
    let _ = origin_task.await;
    assert_listener_released([management_address, listener_address]);
}

#[tokio::test]
async fn tcp_reload_retains_the_old_relay_and_shutdown_cancels_at_the_deadline() {
    let _test_guard = process_drain_test_guard().await;
    let management_address = reserve_tcp_address();
    let listener_address = reserve_tcp_address();
    let (upstream_address, upstream_task) = start_echo_upstream().await;
    let mut initial = tcp_config(management_address, listener_address, upstream_address);
    let mut server = ServerProcess::start(&initial, Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(listener_address).await;

    let authorization = format!("Bearer {TOKEN}");
    let mut old_connection = TcpStream::connect(listener_address)
        .await
        .expect("old TCP relay connection");
    assert_echo(&mut old_connection, b"tcp-before-reload").await;
    let original_revision = active_revision(management_address, &authorization).await;

    initial.max_connections = Some(17);
    write_config(&server.config_path, &initial);
    wait_for_new_revision(management_address, &authorization, &original_revision).await;
    assert_echo(&mut old_connection, b"tcp-after-reload").await;

    let revision = active_revision(management_address, &authorization).await;
    let mut management_connection = TcpStream::connect(management_address)
        .await
        .expect("persistent management connection");
    let drain = persistent_json_request(
        &mut management_connection,
        "POST",
        "/api/v1/generations/drain",
        &authorization,
        &json!({
            "expectedActiveRevision": revision,
            "timeoutMs": 750,
        }),
    )
    .await;
    assert_eq!(drain.status, 202, "{}", drain.text());
    assert_echo(&mut old_connection, b"tcp-after-drain").await;
    assert_no_tcp_echo(listener_address, b"tcp-during-drain").await;

    let shutdown = persistent_json_request(
        &mut management_connection,
        "POST",
        "/api/v1/process/shutdown",
        &authorization,
        &json!({ "expectedActiveRevision": revision }),
    )
    .await;
    assert_eq!(shutdown.status, 202, "{}", shutdown.text());
    drop(management_connection);

    let started = Instant::now();
    timeout(
        Duration::from_secs(8),
        tokio::task::spawn_blocking(move || server.wait_for_exit()),
    )
    .await
    .expect("TCP process shutdown exceeded its deadline")
    .expect("TCP process wait task");
    assert!(
        started.elapsed() >= Duration::from_secs(4),
        "active TCP relay was not retained through the shutdown deadline: {:?}",
        started.elapsed()
    );

    let mut closed = Vec::new();
    timeout(
        Duration::from_secs(1),
        old_connection.read_to_end(&mut closed),
    )
    .await
    .expect("retained TCP relay did not close after shutdown")
    .expect("read retained TCP relay close");
    drop(old_connection);
    upstream_task.abort();
    let _ = upstream_task.await;
    assert_listener_released([management_address, listener_address]);
}

#[tokio::test]
async fn rtmp_reload_and_drain_retain_the_publisher_until_bounded_shutdown() {
    let _test_guard = process_drain_test_guard().await;
    let management_address = reserve_tcp_address();
    let listener_address = reserve_tcp_address();
    let mut initial = rtmp_config(management_address, listener_address);
    let mut server = ServerProcess::start(&initial, Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    server.wait_for_tcp(listener_address).await;

    let authorization = format!("Bearer {TOKEN}");
    let mut publisher = RtmpWireClient::connect(listener_address, "live").await;
    publisher.publish("old-publisher").await;
    publisher.publish_audio(1, &[0xaf, 0x00, 0x12]).await;
    let original_revision = active_revision(management_address, &authorization).await;

    initial.max_connections = Some(19);
    write_config(&server.config_path, &initial);
    wait_for_new_revision(management_address, &authorization, &original_revision).await;
    publisher.publish_audio(2, &[0xaf, 0x01, 0x44]).await;

    let revision = active_revision(management_address, &authorization).await;
    let mut management_connection = TcpStream::connect(management_address)
        .await
        .expect("persistent RTMP management connection");
    let drain = persistent_json_request(
        &mut management_connection,
        "POST",
        "/api/v1/generations/drain",
        &authorization,
        &json!({
            "expectedActiveRevision": revision,
            "timeoutMs": 750,
        }),
    )
    .await;
    assert_eq!(drain.status, 202, "{}", drain.text());
    publisher.publish_audio(3, &[0xaf, 0x01, 0x55]).await;
    assert_no_rtmp_handshake(listener_address).await;

    let revision = active_revision_from_persistent(&mut management_connection).await;
    let shutdown = persistent_json_request(
        &mut management_connection,
        "POST",
        "/api/v1/process/shutdown",
        &authorization,
        &json!({ "expectedActiveRevision": revision }),
    )
    .await;
    assert_eq!(shutdown.status, 202, "{}", shutdown.text());
    drop(management_connection);

    let started = Instant::now();
    timeout(
        Duration::from_secs(8),
        tokio::task::spawn_blocking(move || server.wait_for_exit()),
    )
    .await
    .expect("RTMP process shutdown exceeded its deadline")
    .expect("RTMP process wait task");
    assert!(
        started.elapsed() >= Duration::from_secs(4),
        "RTMP publisher was not retained through the shutdown deadline: {:?}",
        started.elapsed()
    );
    drop(publisher);
    assert_listener_released([management_address, listener_address]);
}

#[tokio::test]
async fn event_sse_closes_with_a_bounded_shutdown_frame_and_releases_its_listener() {
    let _test_guard = process_drain_test_guard().await;
    let management_address = reserve_tcp_address();
    let config = management_config_only(management_address);
    let mut server = ServerProcess::start(&config, Some(TOKEN));
    server.wait_for_tcp(management_address).await;

    let authorization = format!("Bearer {TOKEN}");
    let (mut events, _) = open_event_stream(management_address, &authorization, None).await;
    let ready = read_chunk(&mut events).await;
    assert!(
        String::from_utf8_lossy(&ready).starts_with("event: ready\ndata: {\"cursor\":"),
        "unexpected SSE ready frame: {}",
        String::from_utf8_lossy(&ready)
    );

    let authorization = format!("Bearer {TOKEN}");
    let revision = active_revision(management_address, &authorization).await;
    let shutdown = http_request(
        management_address,
        "POST",
        "/api/v1/process/shutdown",
        &[
            ("Authorization", &authorization),
            ("Content-Type", "application/json"),
        ],
        serde_json::to_vec(&json!({ "expectedActiveRevision": revision }))
            .expect("shutdown request JSON")
            .as_slice(),
    )
    .await;
    assert_eq!(shutdown.status, 202, "{}", shutdown.text());

    let shutdown_frame = timeout(WIRE_TIMEOUT, async {
        loop {
            let frame = read_chunk(&mut events).await;
            if frame.starts_with(b"event: shutdown\n") {
                break frame;
            }
        }
    })
    .await
    .expect("SSE shutdown frame exceeded the process deadline");
    assert_eq!(
        shutdown_frame,
        b"event: shutdown\ndata: {\"reason\":\"server_shutdown\"}\n\n"
    );
    let mut tail = Vec::new();
    timeout(Duration::from_secs(1), events.read_to_end(&mut tail))
        .await
        .expect("SSE stream did not close after shutdown frame")
        .expect("read SSE stream close");

    timeout(
        Duration::from_secs(8),
        tokio::task::spawn_blocking(move || server.wait_for_exit()),
    )
    .await
    .expect("SSE process shutdown exceeded its bounded close window")
    .expect("SSE process wait task");
    assert_listener_released([management_address]);
}

fn management(address: SocketAddr) -> Management {
    Management {
        bind: address,
        ui_dir: None,
    }
}

fn management_config_only(address: SocketAddr) -> Config {
    Config {
        management: Some(management(address)),
        ..empty_config()
    }
}

fn http_config(management_address: SocketAddr, listener_address: SocketAddr, body: &str) -> Config {
    Config {
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

fn forward_h2_config(
    management_address: SocketAddr,
    listener_address: SocketAddr,
    origin_address: SocketAddr,
    private_key_path: &Path,
) -> Config {
    Config {
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

fn tcp_config(
    management_address: SocketAddr,
    listener_address: SocketAddr,
    upstream_address: SocketAddr,
) -> Config {
    Config {
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
        }],
        l4_services: vec![L4Service {
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

fn rtmp_config(management_address: SocketAddr, listener_address: SocketAddr) -> Config {
    Config {
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

fn fixed_response_body(config: &mut Config, body: &str) {
    let HttpRouteAction::FixedResponse { body: target, .. } =
        &mut config.http_services[0].routes[0].action
    else {
        panic!("test route is not a fixed response");
    };
    *target = body.into();
}

async fn active_revision(address: SocketAddr, authorization: &str) -> String {
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

async fn active_revision_from_persistent(stream: &mut TcpStream) -> String {
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

async fn wait_for_new_revision(address: SocketAddr, authorization: &str, original: &str) {
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

async fn generation_drain(
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

async fn persistent_request(
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

async fn persistent_json_request(
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

async fn read_content_length_response(stream: &mut TcpStream) -> HttpResponse {
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

async fn assert_no_http_response(address: SocketAddr) {
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

async fn start_echo_upstream() -> (SocketAddr, JoinHandle<()>) {
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

async fn assert_echo(stream: &mut TcpStream, payload: &[u8]) {
    stream.write_all(payload).await.expect("TCP relay write");
    let mut response = vec![0_u8; payload.len()];
    timeout(WIRE_TIMEOUT, stream.read_exact(&mut response))
        .await
        .unwrap_or_else(|_| panic!("TCP relay response timeout for {payload:?}"))
        .expect("TCP relay response");
    assert_eq!(response, payload);
}

async fn assert_no_tcp_echo(address: SocketAddr, payload: &[u8]) {
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

async fn assert_no_rtmp_handshake(address: SocketAddr) {
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

fn assert_listener_released<const N: usize>(addresses: [SocketAddr; N]) {
    for address in addresses {
        let listener = std::net::TcpListener::bind(address)
            .unwrap_or_else(|error| panic!("listener {address} was not released: {error}"));
        drop(listener);
    }
}

struct H2Connection {
    sender: h2::client::SendRequest<Bytes>,
    driver: JoinHandle<Result<(), h2::Error>>,
}

impl H2Connection {
    async fn try_connect(address: SocketAddr) -> Result<Self, BoxError> {
        let stream = wire_support::tls_connect(
            address,
            wire_support::PROXY_SERVER_NAME,
            "ca-a.pem",
            &[b"h2"],
        )
        .await?;
        let (sender, connection) = h2::client::handshake(stream).await?;
        Ok(Self {
            sender,
            driver: tokio::spawn(connection),
        })
    }

    async fn get(&mut self) -> Result<Bytes, BoxError> {
        let mut sender = self.sender.clone().ready().await?;
        let request = Request::builder()
            .method(Method::GET)
            .uri(format!("https://{}/", wire_support::PROXY_SERVER_NAME))
            .body(())?;
        let (response, _request_body) = sender.send_request(request, true)?;
        let response = timeout(WIRE_TIMEOUT, response).await??;
        if response.status() != StatusCode::OK {
            return Err(
                io::Error::other(format!("unexpected H2 status {}", response.status())).into(),
            );
        }
        let mut body = response.into_body();
        let mut bytes = BytesMut::new();
        while let Some(chunk) = body.data().await {
            let chunk = chunk?;
            body.flow_control().release_capacity(chunk.len())?;
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes.freeze())
    }

    async fn connect(
        &mut self,
        destination: SocketAddr,
        payload: &[u8],
    ) -> Result<Bytes, BoxError> {
        let mut sender = self.sender.clone().ready().await?;
        let request = Request::builder()
            .method(Method::CONNECT)
            .uri(destination.to_string())
            .body(())?;
        let (response, mut request_body) = sender.send_request(request, false)?;
        request_body.send_data(Bytes::copy_from_slice(payload), true)?;
        let response = timeout(WIRE_TIMEOUT, response).await??;
        if response.status() != StatusCode::OK {
            return Err(io::Error::other(format!(
                "unexpected forward H2 status {}",
                response.status()
            ))
            .into());
        }
        let mut body = response.into_body();
        let chunk = timeout(WIRE_TIMEOUT, body.data())
            .await?
            .ok_or_else(|| io::Error::other("forward H2 tunnel ended early"))??;
        body.flow_control().release_capacity(chunk.len())?;
        Ok(chunk)
    }

    async fn finish(self) {
        drop(self.sender);
        self.driver.abort();
        let _ = self.driver.await;
    }
}
