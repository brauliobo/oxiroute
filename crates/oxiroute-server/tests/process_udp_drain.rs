#![cfg(unix)]
#![allow(dead_code, unused_imports, clippy::duplicate_mod)]

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/http.rs"]
mod http_support;
#[path = "support/process.rs"]
mod process_support;

use std::{
    net::{Ipv4Addr, SocketAddr, UdpSocket as StdUdpSocket},
    time::{Duration, Instant},
};

use oxiroute_config::{
    Config, DownstreamTimeoutPolicy, L4Service, Listener, ListenerBind, Management, Protocol,
    UdpPolicy, UpstreamAlgorithm, UpstreamConnectionReuse, UpstreamPool,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpStream, UdpSocket},
    time::{sleep, timeout},
};

use config_support::{empty_config, socket_endpoint};
use http_support::{HttpResponse, http_request};
use process_support::{ServerProcess, reserve_tcp_address, write_config};

const TOKEN: &str = "2d9e0b7f5c4a3e1d8f6b0a9c7e5d3b1f2a4c6e8d0b9f7a5c3e1d9b7f5a3c1e8d";
const WIRE_TIMEOUT: Duration = Duration::from_secs(10);
const DRAIN_PROBE_TIMEOUT: Duration = Duration::from_millis(750);

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn udp_reload_and_drain_retain_sessions_reject_new_work_and_cancel_at_deadline() {
    let management_address = reserve_tcp_address();
    let listener_address = reserve_udp_address();
    let old_upstream = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("old UDP upstream bind");
    let old_upstream_address = old_upstream.local_addr().expect("old UDP upstream address");
    let new_upstream = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("new UDP upstream bind");
    let new_upstream_address = new_upstream.local_addr().expect("new UDP upstream address");

    let initial = udp_config(management_address, listener_address, old_upstream_address);
    let mut server = ServerProcess::start(&initial, Some(TOKEN));
    server.wait_for_tcp(management_address).await;
    let authorization = format!("Bearer {TOKEN}");
    wait_for_udp_listener(management_address, &authorization).await;

    let old_client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("old UDP client bind");
    old_client
        .send_to(b"old-before-reload", listener_address)
        .await
        .expect("old UDP session datagram");
    let old_peer = receive_upstream(&old_upstream, b"old-before-reload").await;

    let original_revision = active_revision(management_address, &authorization).await;
    let mut candidate = initial.clone();
    candidate.upstream_pools[0].endpoints = vec![socket_endpoint(new_upstream_address)];
    write_config(&server.config_path, &candidate);
    wait_for_new_revision(management_address, &authorization, &original_revision).await;

    let (new_client, new_peer) =
        start_candidate_session(listener_address, &new_upstream, b"new-before-drain").await;

    let revision = active_revision(management_address, &authorization).await;
    let mut management = TcpStream::connect(management_address)
        .await
        .expect("persistent UDP management connection");
    let drain = persistent_json_request(
        &mut management,
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
    assert_eq!(drain.json()["outcome"], "draining");

    let rejected_client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("rejected UDP client bind");
    rejected_client
        .send_to(b"rejected-after-drain", listener_address)
        .await
        .expect("rejected UDP datagram");
    assert_no_datagram(&old_upstream, "old UDP upstream").await;
    assert_no_datagram(&new_upstream, "new UDP upstream").await;

    old_upstream
        .send_to(b"old-after-reload-and-drain", old_peer)
        .await
        .expect("old UDP response");
    expect_client_datagram(&old_client, b"old-after-reload-and-drain").await;
    new_upstream
        .send_to(b"new-after-drain", new_peer)
        .await
        .expect("new UDP response");
    expect_client_datagram(&new_client, b"new-after-drain").await;

    let shutdown = persistent_json_request(
        &mut management,
        "POST",
        "/api/v1/process/shutdown",
        &authorization,
        &json!({
            "expectedActiveRevision": revision,
        }),
    )
    .await;
    assert_eq!(shutdown.status, 202, "{}", shutdown.text());
    drop(management);

    let started = Instant::now();
    timeout(
        Duration::from_secs(8),
        tokio::task::spawn_blocking(move || server.wait_for_exit()),
    )
    .await
    .expect("UDP process shutdown exceeded its deadline")
    .expect("UDP process wait task");
    assert!(
        started.elapsed() >= Duration::from_secs(4),
        "active UDP sessions were not retained through the shutdown deadline: {:?}",
        started.elapsed()
    );

    StdUdpSocket::bind(listener_address).expect("UDP listener released after shutdown");
}

fn udp_config(
    management_address: SocketAddr,
    listener_address: SocketAddr,
    upstream_address: SocketAddr,
) -> Config {
    Config {
        management: Some(Management {
            bind: management_address,
            ui_dir: None,
        }),
        listeners: vec![Listener {
            name: "relay".into(),
            bind: ListenerBind::Udp {
                address: listener_address,
            },
            protocol: Protocol::Udp,
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
            http_versions: oxiroute_config::HttpVersionPolicy::default(),
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
            lifetime_timeout_ms: Some(30_000),
            proxy_protocol: None,
            udp: Some(UdpPolicy::default()),
        }],
        ..empty_config()
    }
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
        .expect("active UDP revision")
        .to_owned()
}

async fn wait_for_udp_listener(address: SocketAddr, authorization: &str) {
    timeout(WIRE_TIMEOUT, async {
        loop {
            let status = http_request(
                address,
                "GET",
                "/api/v1/status",
                &[("Authorization", authorization)],
                &[],
            )
            .await
            .json();
            if status["listeners"].as_array().is_some_and(|listeners| {
                listeners
                    .iter()
                    .any(|listener| listener["name"] == "relay" && listener["state"] == "listening")
            }) {
                return;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("UDP listener readiness timed out");
}

async fn wait_for_new_revision(address: SocketAddr, authorization: &str, original: &str) {
    timeout(WIRE_TIMEOUT, async {
        loop {
            let response = http_request(
                address,
                "GET",
                "/api/v1/status",
                &[("Authorization", authorization)],
                &[],
            )
            .await;
            if response.json()["activeRevision"]
                .as_str()
                .is_some_and(|revision| revision != original)
            {
                return;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("UDP generation reload timed out");
}

async fn receive_upstream(socket: &UdpSocket, expected: &[u8]) -> SocketAddr {
    let mut buffer = [0_u8; 256];
    let (length, peer) = timeout(WIRE_TIMEOUT, socket.recv_from(&mut buffer))
        .await
        .expect("UDP upstream receive timeout")
        .expect("UDP upstream receive");
    assert_eq!(&buffer[..length], expected);
    peer
}

async fn start_candidate_session(
    listener_address: SocketAddr,
    upstream: &UdpSocket,
    payload: &[u8],
) -> (UdpSocket, SocketAddr) {
    let deadline = Instant::now() + WIRE_TIMEOUT;
    loop {
        assert!(
            Instant::now() < deadline,
            "candidate UDP session admission timed out"
        );
        let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("candidate UDP client bind");
        client
            .send_to(payload, listener_address)
            .await
            .expect("candidate UDP session datagram");
        let wait = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(500));
        let mut buffer = [0_u8; 256];
        match timeout(wait, upstream.recv_from(&mut buffer)).await {
            Ok(Ok((length, peer))) => {
                assert_eq!(&buffer[..length], payload);
                return (client, peer);
            }
            Ok(Err(error)) => panic!("candidate UDP upstream receive: {error}"),
            Err(_) => {}
        }
    }
}

async fn expect_client_datagram(client: &UdpSocket, expected: &[u8]) {
    let mut buffer = [0_u8; 256];
    let (length, _) = timeout(WIRE_TIMEOUT, client.recv_from(&mut buffer))
        .await
        .expect("UDP client receive timeout")
        .expect("UDP client receive");
    assert_eq!(&buffer[..length], expected);
}

async fn assert_no_datagram(socket: &UdpSocket, label: &str) {
    let mut buffer = [0_u8; 256];
    let result = timeout(DRAIN_PROBE_TIMEOUT, socket.recv_from(&mut buffer)).await;
    match result {
        Err(_) => {}
        Ok(Ok((length, _))) => panic!("{label} received work after drain: {:?}", &buffer[..length]),
        Ok(Err(error)) => panic!("{label} receive failed: {error}"),
    }
}

async fn persistent_json_request(
    stream: &mut TcpStream,
    method: &str,
    path: &str,
    authorization: &str,
    body: &Value,
) -> HttpResponse {
    let body = serde_json::to_vec(body).expect("persistent UDP request JSON");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\nAuthorization: {authorization}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("persistent UDP request head");
    stream
        .write_all(&body)
        .await
        .expect("persistent UDP request body");
    read_content_length_response(stream).await
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

fn reserve_udp_address() -> SocketAddr {
    StdUdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("reserve UDP address")
        .local_addr()
        .expect("reserved UDP address")
}
