#[path = "support/config.rs"]
mod config_support;

use std::{net::SocketAddr, sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt};
use oxiroute_config::{
    Config, HttpPathSelector, HttpProxyPolicy, HttpRequestHeaderMutation, HttpRequestHeaderValue,
    HttpRoute, HttpRouteAction, HttpService, HttpVersionPolicy, Listener, Protocol,
    UpstreamAlgorithm, UpstreamPool,
};
use oxiroute_server::{HttpReverseProxy, RuntimeMetrics, ServiceKind, service_specs};
use pingora::{apps::ServerApp, proxy::http_proxy, server::configuration::ServerConf};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
    time::timeout,
};
use tokio_tungstenite::{accept_async, connect_async, tungstenite::Message};

use config_support::{empty_config, socket_bind, socket_endpoint};

#[tokio::test]
async fn websocket_upgrade_proxies_frames_in_both_directions() {
    timeout(Duration::from_secs(5), async {
        let origin_listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let origin_address = origin_listener.local_addr().expect("origin address");
        let origin = tokio::spawn(async move {
            let (stream, _) = origin_listener.accept().await.expect("origin accept");
            let mut websocket = accept_async(stream).await.expect("origin upgrade");
            assert_eq!(
                websocket
                    .next()
                    .await
                    .expect("client frame")
                    .expect("valid frame"),
                Message::Text("client-to-origin".into())
            );
            websocket
                .send(Message::Text("origin-to-client".into()))
                .await
                .expect("origin send");
            websocket.close(None).await.expect("origin close");
        });

        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.expect("proxy bind");
        let proxy_address = proxy_listener.local_addr().expect("proxy address");
        let runtime_metrics = RuntimeMetrics::new();
        let metrics = runtime_metrics
            .register_listener("websocket", "http", proxy_address.to_string(), 100)
            .expect("listener metrics");
        let config = websocket_config(proxy_address, origin_address);
        let mut services = service_specs(&config).expect("service plan");
        let ServiceKind::Http(http_service) = services.remove(0).kind else {
            panic!("websocket listener must compile as HTTP");
        };
        let configuration = Arc::new(ServerConf::default());
        let proxy = Arc::new(http_proxy(
            &configuration,
            HttpReverseProxy::new(http_service, metrics),
        ));
        let (_shutdown_tx, shutdown) = watch::channel(false);
        let proxy_task = tokio::spawn(async move {
            let (stream, _) = proxy_listener.accept().await.expect("proxy accept");
            let stream: pingora::protocols::Stream =
                Box::new(pingora::protocols::l4::stream::Stream::from(stream));
            proxy.process_new(stream, &shutdown).await;
        });

        let (mut client, response) = connect_async(format!("ws://{proxy_address}/socket"))
            .await
            .expect("client upgrade");
        assert_eq!(response.status().as_u16(), 101);
        client
            .send(Message::Text("client-to-origin".into()))
            .await
            .expect("client send");
        assert_eq!(
            client
                .next()
                .await
                .expect("origin frame")
                .expect("valid frame"),
            Message::Text("origin-to-client".into())
        );
        client.close(None).await.expect("client close");

        origin.await.expect("origin task");
        proxy_task.await.expect("proxy task");
        let snapshot = runtime_metrics.snapshot().expect("traffic snapshot");
        assert!(snapshot.traffic.bytes_received > 0);
        assert!(snapshot.traffic.bytes_sent > 0);
    })
    .await
    .expect("websocket exchange timed out");
}

#[tokio::test]
async fn upgrade_preserves_request_body_before_101_and_tunnel_bytes_after_101() {
    timeout(Duration::from_secs(5), async {
        let origin_listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
        let origin_address = origin_listener.local_addr().expect("origin address");
        let origin = tokio::spawn(async move {
            let (mut stream, _) = origin_listener.accept().await.expect("origin accept");
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                let byte = stream.read_u8().await.expect("origin request head");
                head.push(byte);
            }
            assert!(String::from_utf8_lossy(&head).starts_with("POST /socket HTTP/1.1\r\n"));
            assert!(head.windows(b"\r\nx-upgrade-meta: retained\r\n".len()).any(|window| {
                window == b"\r\nx-upgrade-meta: retained\r\n"
            }));
            assert!(!head.windows(b"X-UpGrAdE-MeTa:".len()).any(|window| {
                window == b"X-UpGrAdE-MeTa:"
            }));
            let mut before_upgrade = [0; 4];
            stream
                .read_exact(&mut before_upgrade)
                .await
                .expect("body before upgrade");
            assert_eq!(&before_upgrade, b"pre!");
            stream
                .write_all(
                    b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: test\r\nX-UpGrAdE-MeTa: retained\r\n\r\n",
                )
                .await
                .expect("origin upgrade response");
            let mut after_upgrade = [0; 5];
            stream
                .read_exact(&mut after_upgrade)
                .await
                .expect("body after upgrade");
            assert_eq!(&after_upgrade, b"post!");
            stream.write_all(b"reply").await.expect("origin tunnel reply");
        });

        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.expect("proxy bind");
        let proxy_address = proxy_listener.local_addr().expect("proxy address");
        let runtime_metrics = RuntimeMetrics::new();
        let metrics = runtime_metrics
            .register_listener("upgrade-body", "http", proxy_address.to_string(), 100)
            .expect("listener metrics");
        let config = websocket_config(proxy_address, origin_address);
        let mut services = service_specs(&config).expect("service plan");
        let ServiceKind::Http(http_service) = services.remove(0).kind else {
            panic!("upgrade listener must compile as HTTP");
        };
        let configuration = Arc::new(ServerConf::default());
        let proxy = Arc::new(http_proxy(
            &configuration,
            HttpReverseProxy::new(http_service, metrics),
        ));
        let (_shutdown_tx, shutdown) = watch::channel(false);
        let proxy_task = tokio::spawn(async move {
            let (stream, _) = proxy_listener.accept().await.expect("proxy accept");
            let stream: pingora::protocols::Stream =
                Box::new(pingora::protocols::l4::stream::Stream::from(stream));
            proxy.process_new(stream, &shutdown).await;
        });

        let mut client = TcpStream::connect(proxy_address)
            .await
            .expect("upgrade client");
        client
            .write_all(
                b"POST /socket HTTP/1.1\r\nHost: upgrade.test\r\nConnection: Upgrade\r\nUpgrade: test\r\nX-UpGrAdE-MeTa: retained\r\nContent-Length: 4\r\n\r\npre!",
            )
            .await
            .expect("request and pre-upgrade body");
        let mut response = Vec::new();
        while !response.ends_with(b"\r\n\r\n") {
            response.push(client.read_u8().await.expect("upgrade response"));
        }
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 101"));
        assert!(response.windows(b"\r\nx-upgrade-meta: retained\r\n".len()).any(|window| {
            window == b"\r\nx-upgrade-meta: retained\r\n"
        }));
        assert!(!response.windows(b"X-UpGrAdE-MeTa:".len()).any(|window| {
            window == b"X-UpGrAdE-MeTa:"
        }));
        client
            .write_all(b"post!")
            .await
            .expect("post-upgrade body");
        let mut reply = [0; 5];
        client.read_exact(&mut reply).await.expect("tunnel reply");
        assert_eq!(&reply, b"reply");
        drop(client);

        origin.await.expect("origin task");
        proxy_task.await.expect("proxy task");
    })
    .await
    .expect("upgrade body exchange timed out");
}

fn websocket_config(proxy_address: SocketAddr, origin_address: SocketAddr) -> Config {
    Config {
        listeners: vec![Listener {
            name: "websocket".into(),
            bind: socket_bind(proxy_address),
            protocol: Protocol::Http,
            service: Some("websocket".into()),
            tls_profile: None,
            max_connections: Some(100),
            downstream_timeouts: oxiroute_config::DownstreamTimeoutPolicy::default(),
        }],
        upstream_pools: vec![UpstreamPool {
            name: "origin".into(),
            servers: Vec::new(),
            endpoints: vec![socket_endpoint(origin_address)],
            algorithm: UpstreamAlgorithm::RoundRobin,
            health_check: None,
            tls: None,
            http_versions: HttpVersionPolicy::default(),
            queue_timeout_ms: None,
            connect_timeout_ms: None,
            server_timeout_ms: None,
            connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
        }],
        http_services: vec![HttpService {
            name: "websocket".into(),
            routes: vec![HttpRoute {
                host: None,
                path: HttpPathSelector::SegmentPrefix { value: "/".into() },
                methods: Vec::new(),
                access_policy: None,
                policy: oxiroute_config::HttpRoutePolicy::default(),
                action: HttpRouteAction::Proxy {
                    upstream_pool: "origin".into(),
                    policy: HttpProxyPolicy {
                        request_headers: vec![
                            HttpRequestHeaderMutation::Set {
                                name: "upgrade".into(),
                                value: HttpRequestHeaderValue::IncomingHeader {
                                    name: "upgrade".into(),
                                    max_bytes: 128,
                                },
                            },
                            HttpRequestHeaderMutation::Set {
                                name: "connection".into(),
                                value: HttpRequestHeaderValue::Literal {
                                    value: "upgrade".into(),
                                },
                            },
                        ],
                        ..HttpProxyPolicy::default()
                    },
                },
            }],
            automatic_response_headers: true,
            upstream_io_timeout_ms: 5_000,
            max_request_body_bytes: Some(8),
            gzip: None,
            access_log: None,
        }],
        ..empty_config()
    }
}
