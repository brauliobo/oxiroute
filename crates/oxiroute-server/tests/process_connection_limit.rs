#![allow(dead_code, unused_imports, clippy::duplicate_mod)]

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/http.rs"]
mod http_support;
#[path = "support/process.rs"]
mod process_support;

use std::net::SocketAddr;

use oxiroute_config::{
    ConfigDraft, DownstreamTimeoutPolicy, HttpPathSelector, HttpRoute, HttpRouteAction,
    HttpRoutePolicy, HttpService, Listener, Protocol, Stats, StatsPage, StatsPageAdminPolicy,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use config_support::{empty_config, socket_bind};
use http_support::raw_http_request;
use process_support::{ServerProcess, reserve_tcp_address};

const SEQUENTIAL_REQUESTS: usize = 300;
const REQUEST: &[u8] = b"GET /probe HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";

#[tokio::test]
async fn stats_page_with_one_connection_never_rejects_sequential_close_requests() {
    let address = reserve_tcp_address();
    let mut config = empty_config();
    config.stats = Some(Stats {
        binds: Vec::new(),
        admin_token_file: None,
        pages: vec![StatsPage {
            bind: address,
            uri_prefix: "/probe".into(),
            refresh_ms: 10_000,
            admin: StatsPageAdminPolicy::Localhost,
            max_connections: Some(1),
            downstream_timeouts: DownstreamTimeoutPolicy::default(),
        }],
    });

    assert_sequential_requests_are_admitted(config, address).await;
}

#[tokio::test]
async fn http_listener_with_one_connection_never_rejects_sequential_close_requests() {
    let address = reserve_tcp_address();
    let mut config = empty_config();
    config.listeners = vec![Listener {
        name: "limited".into(),
        bind: socket_bind(address),
        protocol: Protocol::Http,
        service: Some("web".into()),
        tls_profile: None,
        proxy_protocol: None,
        max_connections: Some(1),
        downstream_timeouts: DownstreamTimeoutPolicy::default(),
    }];
    config.http_services = vec![HttpService {
        drain_deadline_ms: None,
        name: "web".into(),
        routes: vec![HttpRoute {
            host: None,
            path: HttpPathSelector::Exact {
                value: "/probe".into(),
            },
            methods: Vec::new(),
            access_policy: None,
            policy: HttpRoutePolicy::default(),
            action: HttpRouteAction::FixedResponse {
                status: 200,
                body: "ok".into(),
                headers: Vec::new(),
            },
        }],
        automatic_response_headers: true,
        upstream_io_timeout_ms: 30_000,
        max_request_body_bytes: Some(1_024),
        gzip: None,
        access_log: None,
    }];

    assert_sequential_requests_are_admitted(config, address).await;
}

/// Every request reconnects the instant the previous `Connection: close` response reached EOF, so
/// the single admission slot must already be free: any reset means the slot outlived the socket.
async fn assert_sequential_requests_are_admitted(config: ConfigDraft, address: SocketAddr) {
    let mut server = ServerProcess::start(&config.validate().expect("valid process config"), None);
    let mut first = server.connect_when_bound(address).await;
    first.write_all(REQUEST).await.expect("write first request");
    let mut response = Vec::new();
    first
        .read_to_end(&mut response)
        .await
        .expect("read first response");
    assert!(response.starts_with(b"HTTP/1.1 200"), "first response");

    for index in 0..SEQUENTIAL_REQUESTS {
        let response = raw_http_request(address, REQUEST).await;
        assert_eq!(response.status, 200, "sequential request {index}");
    }
    server.shutdown();
}
