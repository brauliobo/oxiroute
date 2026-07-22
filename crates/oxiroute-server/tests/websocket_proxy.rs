use std::{sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt};
use oxiroute_config::{
    Config, HttpRoute, HttpService, Listener, Protocol, UpstreamAlgorithm, UpstreamPool,
};
use oxiroute_server::{HttpReverseProxy, RuntimeMetrics, ServiceKind, service_specs};
use pingora::{apps::ServerApp, proxy::http_proxy, server::configuration::ServerConf};
use tokio::{net::TcpListener, sync::watch, time::timeout};
use tokio_tungstenite::{accept_async, connect_async, tungstenite::Message};

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
        let config = Config {
            version: 1,
            management: None,
            listeners: vec![Listener {
                name: "websocket".into(),
                bind: proxy_address,
                protocol: Protocol::Http,
                service: Some("websocket".into()),
                max_connections: 100,
            }],
            upstream_pools: vec![UpstreamPool {
                name: "origin".into(),
                endpoints: vec![origin_address],
                algorithm: UpstreamAlgorithm::RoundRobin,
            }],
            http_services: vec![HttpService {
                name: "websocket".into(),
                routes: vec![HttpRoute {
                    host: None,
                    path_prefix: "/".into(),
                    methods: Vec::new(),
                    upstream_pool: "origin".into(),
                }],
                upstream_io_timeout_ms: 5_000,
                max_request_body_bytes: 8,
            }],
            l4_services: Vec::new(),
        };
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
