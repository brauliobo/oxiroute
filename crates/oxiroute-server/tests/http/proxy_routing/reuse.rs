use crate::TEST_TIMEOUT;
use crate::config_support::socket_endpoint;
use crate::http_support::HttpResponse as RawResponse;
use crate::http_support::read_request_head as read_request_head_bytes;
use crate::support::builders::capped_pool;
use crate::support::builders::pool;
use crate::support::builders::route;
use crate::support::harness::ProxyHarness;
use crate::support::origin::Origin;
use crate::support::wire::assert_origin_response;
use crate::support::wire::read_framed_response;
use crate::support::wire::read_request_head;
use crate::support::wire::read_response_head;
use crate::support::wire::wait_for_queued_requests;
use oxiroute_config::DnsResolutionPolicy;
use oxiroute_config::HttpVersionPolicy;
use oxiroute_config::UpstreamAlgorithm;
use oxiroute_config::UpstreamPool;
use oxiroute_config::UpstreamServer;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::time::sleep;
use tokio::time::timeout;

#[tokio::test]
async fn informational_responses_reach_final_and_reuse_upstream_connection() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("informational origin bind");
        let address = listener.local_addr().expect("informational origin address");
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("informational origin accept");
            let first_request = read_request_head_bytes(&mut stream)
                .await
                .expect("informational origin HEAD request");
            assert!(first_request.starts_with(b"HEAD /first HTTP/1.1\r\n"));
            stream
                .write_all(
                    b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 103 Early Hints\r\nlInK: </style.css>; rel=preload\r\n\r\nHTTP/1.1 200 OK\r\ncOnTeNt-LeNgTh: 7\r\nConnection: keep-alive\r\n\r\n",
                )
                .await
                .expect("informational origin responses");

            let second_request = read_request_head_bytes(&mut stream)
                .await
                .expect("reused informational origin request");
            assert!(second_request.starts_with(b"GET /second HTTP/1.1\r\n"));
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecond",
                )
                .await
                .expect("reused informational origin response");
        });
        let proxy = ProxyHarness::start(
            vec![capped_pool("informational", address, 1)],
            vec![route(None, "/", &[], "informational")],
            1024,
            1,
        )
        .await;
        let mut client = TcpStream::connect(proxy.address)
            .await
            .expect("informational downstream connect");

        client
            .write_all(
                b"HEAD /first HTTP/1.1\r\nHost: informational.test\r\nExpect: 100-continue\r\nConnection: keep-alive\r\n\r\n",
            )
            .await
            .expect("informational downstream HEAD request");
        let continue_response = RawResponse::parse(
            read_response_head(&mut client)
                .await
                .expect("informational downstream response"),
        );
        assert_eq!(continue_response.status, 100);
        let informational = RawResponse::parse(
            read_response_head(&mut client)
                .await
                .expect("early hints downstream response"),
        );
        assert_eq!(informational.status, 103);
        assert!(informational.text().contains("\r\nlink: </style.css>"));
        assert!(!informational.text().contains("\r\nlInK:"));
        let final_response = RawResponse::parse(
            read_response_head(&mut client)
                .await
                .expect("final downstream HEAD response"),
        );
        assert_eq!(final_response.status, 200);
        assert!(final_response.body.is_empty());
        assert!(final_response.text().contains("\r\nContent-Length: 7\r\n"));
        assert!(!final_response.text().contains("cOnTeNt-LeNgTh"));

        client
            .write_all(
                b"GET /second HTTP/1.1\r\nHost: informational.test\r\nConnection: close\r\n\r\n",
            )
            .await
            .expect("reused downstream request");
        assert_eq!(
            read_framed_response(&mut client)
                .await
                .expect("reused downstream response")
                .body,
            b"second"
        );
        drop(client);

        origin.await.expect("informational origin task");
        proxy.wait_for_no_active_leases().await;
        proxy.finish().await;
    })
    .await
    .expect("informational HEAD response test timed out");
}

#[tokio::test]
async fn reusable_upstream_connection_holds_its_lease_until_the_socket_closes() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("reusable origin bind");
        let address = listener.local_addr().expect("reusable origin address");
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("reusable origin accept");
            read_request_head(&mut stream)
                .await
                .expect("first reusable request");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: keep-alive\r\n\r\nfirst",
                )
                .await
                .expect("first reusable response");
            read_request_head(&mut stream)
                .await
                .expect("second reusable request");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecond",
                )
                .await
                .expect("second reusable response");
        });
        let proxy = ProxyHarness::start(
            vec![capped_pool("reused", address, 1)],
            vec![route(None, "/", &[], "reused")],
            1024,
            1,
        )
        .await;
        let mut client = TcpStream::connect(proxy.address)
            .await
            .expect("reusable downstream connect");

        client
            .write_all(b"GET / HTTP/1.1\r\nHost: reuse.test\r\n\r\n")
            .await
            .expect("first reusable downstream request");
        assert_eq!(
            read_framed_response(&mut client)
                .await
                .expect("first reusable downstream response")
                .body,
            b"first"
        );
        assert_eq!(
            proxy.pools[0].health_snapshot().endpoints[0].active_connections,
            1,
            "an idle pooled socket must retain its physical connection lease"
        );
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: reuse.test\r\nConnection: close\r\n\r\n")
            .await
            .expect("second reusable downstream request");
        assert_eq!(
            read_framed_response(&mut client)
                .await
                .expect("second reusable downstream response")
                .body,
            b"second"
        );
        origin.await.expect("reusable origin task");
        proxy.wait_for_no_active_leases().await;
        proxy.finish().await;
    })
    .await
    .expect("reusable upstream lease test timed out");
}

#[tokio::test]
async fn a_client_connection_close_does_not_close_the_upstream_connection() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("persistent origin bind");
        let address = listener.local_addr().expect("persistent origin address");
        let accepted = Arc::new(AtomicUsize::new(0));
        let origin_accepted = Arc::clone(&accepted);
        let origin = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.expect("persistent origin accept");
                origin_accepted.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    while let Ok(head) = read_request_head_bytes(&mut stream).await {
                        let head = String::from_utf8_lossy(&head).to_ascii_lowercase();
                        assert!(!head.contains("connection:"), "forwarded: {head}");
                        stream
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                            .await
                            .expect("persistent origin response");
                    }
                });
            }
        });
        let proxy = ProxyHarness::start(
            vec![capped_pool("persistent", address, 4)],
            vec![route(None, "/", &[], "persistent")],
            1024,
            3,
        )
        .await;
        for _ in 0..3 {
            let mut client = TcpStream::connect(proxy.address)
                .await
                .expect("persistent downstream connect");
            client
                .write_all(b"GET / HTTP/1.1\r\nHost: close.test\r\nConnection: close\r\n\r\n")
                .await
                .expect("persistent downstream request");
            let response = read_framed_response(&mut client)
                .await
                .expect("persistent downstream response");
            assert_eq!(response.body, b"ok");
            sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(accepted.load(Ordering::SeqCst), 1);
        origin.abort();
        proxy.finish().await;
    })
    .await
    .expect("upstream persistence test timed out");
}

#[tokio::test]
async fn reusable_idle_connection_remains_counted_by_least_connections() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("reusable leastconn origin bind");
        let address = listener.local_addr().expect("reusable leastconn address");
        let (release_tx, release_rx) = oneshot::channel();
        let retained = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("retained origin accept");
            read_request_head(&mut stream)
                .await
                .expect("retained origin request");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: keep-alive\r\n\r\nretained",
                )
                .await
                .expect("retained origin response");
            release_rx.await.expect("retained origin release");
        });
        let idle = Origin::start("idle", 1).await;
        let mut balanced = pool("balanced", &[address, idle.address]);
        balanced.algorithm = UpstreamAlgorithm::LeastConnections;
        balanced.connection_reuse = oxiroute_config::UpstreamConnectionReuse::Safe;
        let proxy = ProxyHarness::start(
            vec![balanced],
            vec![route(None, "/", &[], "balanced")],
            1024,
            1,
        )
        .await;

        let mut client = TcpStream::connect(proxy.address)
            .await
            .expect("leastconn downstream connect");
        client
            .write_all(b"GET /first HTTP/1.1\r\nHost: least.test\r\n\r\n")
            .await
            .expect("first leastconn request");
        let first = read_framed_response(&mut client)
            .await
            .expect("first leastconn response");
        assert_eq!(first.body, b"retained");
        assert_eq!(
            proxy.pools[0].health_snapshot().endpoints[0].active_connections,
            1,
            "the idle reusable socket must retain its leastconn lease"
        );
        client
            .write_all(
                b"GET /second HTTP/1.1\r\nHost: least.test\r\nConnection: close\r\n\r\n",
            )
            .await
            .expect("second leastconn request");
        let second = read_framed_response(&mut client)
            .await
            .expect("second leastconn response");
        assert_origin_response(&second, "idle");
        drop(client);

        release_tx.send(()).expect("release retained origin");
        retained.await.expect("retained origin task");
        proxy.wait_for_no_active_leases().await;
        proxy.finish().await;
        idle.finish().await;
    })
    .await
    .expect("reusable least-connections test timed out");
}

#[tokio::test]
async fn preread_one_kib_response_is_exact_and_keeps_upstream_reusable() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("preread origin bind");
        let address = listener.local_addr().expect("preread origin address");
        let expected_body = vec![b'x'; 1024];
        let origin_body = expected_body.clone();
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("preread origin accept");
            read_request_head(&mut stream)
                .await
                .expect("first preread request");
            let mut response =
                b"HTTP/1.1 200 OK\r\nContent-Length: 1024\r\nConnection: keep-alive\r\n\r\n"
                    .to_vec();
            response.extend_from_slice(&origin_body);
            stream
                .write_all(&response)
                .await
                .expect("single-write preread response");

            let second_request = read_request_head_bytes(&mut stream)
                .await
                .expect("reused preread request");
            assert!(second_request.starts_with(b"GET /second HTTP/1.1\r\n"));
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecond",
                )
                .await
                .expect("reused preread response");
        });
        let proxy = ProxyHarness::start(
            vec![capped_pool("preread", address, 1)],
            vec![route(None, "/", &[], "preread")],
            1024,
            1,
        )
        .await;
        let mut client = TcpStream::connect(proxy.address)
            .await
            .expect("preread downstream connect");

        client
            .write_all(
                b"GET /first HTTP/1.1\r\nHost: preread.test\r\nConnection: keep-alive\r\n\r\n",
            )
            .await
            .expect("first preread downstream request");
        assert_eq!(
            read_framed_response(&mut client)
                .await
                .expect("first preread downstream response")
                .body,
            expected_body
        );
        client
            .write_all(b"GET /second HTTP/1.1\r\nHost: preread.test\r\nConnection: close\r\n\r\n")
            .await
            .expect("second preread downstream request");
        assert_eq!(
            read_framed_response(&mut client)
                .await
                .expect("second preread downstream response")
                .body,
            b"second"
        );

        origin.await.expect("preread origin task");
        proxy.wait_for_no_active_leases().await;
        proxy.finish().await;
    })
    .await
    .expect("preread response test timed out");
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one wire regression covers FIFO admission across two saturated workers"
)]
async fn capped_first_pool_dispatches_queued_requests_fifo_to_the_next_available_worker() {
    timeout(TEST_TIMEOUT, async {
        let primary = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("primary worker bind");
        let primary_address = primary.local_addr().expect("primary worker address");
        let secondary = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("secondary worker bind");
        let secondary_address = secondary.local_addr().expect("secondary worker address");
        let (primary_seen_tx, primary_seen_rx) = oneshot::channel();
        let (secondary_seen_tx, secondary_seen_rx) = oneshot::channel();
        let (old_seen_tx, old_seen_rx) = oneshot::channel();
        let (new_seen_tx, new_seen_rx) = oneshot::channel();
        let (release_primary_tx, release_primary_rx) = oneshot::channel();
        let (release_secondary_tx, release_secondary_rx) = oneshot::channel();
        let (release_old_tx, release_old_rx) = oneshot::channel();
        let primary_worker = tokio::spawn(async move {
            let (mut held, _) = primary.accept().await.expect("primary held accept");
            let request = read_request_head_bytes(&mut held)
                .await
                .expect("primary held request");
            assert!(request.starts_with(b"GET /hold-primary HTTP/1.1\r\n"));
            primary_seen_tx.send(()).expect("primary held signal");
            release_primary_rx.await.expect("release primary worker");
            held.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\nConnection: close\r\n\r\nhold-primary",
            )
            .await
            .expect("primary held response");
            let (mut queued, _) = primary.accept().await.expect("primary queued accept");
            let request = read_request_head_bytes(&mut queued)
                .await
                .expect("primary queued request");
            assert!(request.starts_with(b"GET /new HTTP/1.1\r\n"));
            new_seen_tx.send(()).expect("new request signal");
            queued
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nnew")
                .await
                .expect("primary queued response");
        });
        let secondary_worker = tokio::spawn(async move {
            let (mut held, _) = secondary.accept().await.expect("secondary held accept");
            let request = read_request_head_bytes(&mut held)
                .await
                .expect("secondary held request");
            assert!(request.starts_with(b"GET /hold-secondary HTTP/1.1\r\n"));
            secondary_seen_tx.send(()).expect("secondary held signal");
            release_secondary_rx
                .await
                .expect("release secondary worker");
            held.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\nConnection: close\r\n\r\nhold-secondary",
            )
            .await
            .expect("secondary held response");
            let (mut queued, _) = secondary.accept().await.expect("secondary queued accept");
            let request = read_request_head_bytes(&mut queued)
                .await
                .expect("secondary queued request");
            assert!(request.starts_with(b"GET /old HTTP/1.1\r\n"));
            old_seen_tx.send(()).expect("old request signal");
            release_old_rx.await.expect("release old request");
            queued
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nold")
                .await
                .expect("secondary queued response");
        });
        let proxy = Arc::new(
            ProxyHarness::start(
                vec![UpstreamPool {
                    name: "workers".into(),
                    servers: vec![
                        UpstreamServer {
                            name: "primary".into(),
                            endpoint: socket_endpoint(primary_address),
                            max_connections: Some(1),
                            dns_resolution: DnsResolutionPolicy::OnConnect,
                        },
                        UpstreamServer {
                            name: "secondary".into(),
                            endpoint: socket_endpoint(secondary_address),
                            max_connections: Some(1),
                            dns_resolution: DnsResolutionPolicy::OnConnect,
                        },
                    ],
                    endpoints: Vec::new(),
                    algorithm: UpstreamAlgorithm::First,
                    health_check: None,
                    passive_health: None,
                    tls: None,
                    http_versions: HttpVersionPolicy::default(),
                    queue_timeout_ms: Some(1_000),
                    connect_timeout_ms: None,
                    server_timeout_ms: None,
                    connection_reuse: oxiroute_config::UpstreamConnectionReuse::Never,
                    registration: None,
                    canary: None,
                    resilience: oxiroute_config::PoolResilience::default(),
                    queue: None,
                }],
                vec![route(None, "/", &[], "workers")],
                1024,
                4,
            )
            .await,
        );
        let held_primary_proxy = Arc::clone(&proxy);
        let held_primary = tokio::spawn(async move {
            held_primary_proxy
                .request("GET /hold-primary HTTP/1.1\r\nHost: workers.test\r\n")
                .await
        });
        primary_seen_rx
            .await
            .expect("primary request reached worker");
        let held_secondary_proxy = Arc::clone(&proxy);
        let held_secondary = tokio::spawn(async move {
            held_secondary_proxy
                .request("GET /hold-secondary HTTP/1.1\r\nHost: workers.test\r\n")
                .await
        });
        secondary_seen_rx
            .await
            .expect("secondary request reached worker");
        assert_eq!(
            proxy.pools[0]
                .health_snapshot()
                .endpoints
                .iter()
                .map(|endpoint| endpoint.active_connections)
                .collect::<Vec<_>>(),
            vec![1, 1],
            "both held workers retain their capacity leases"
        );

        let old_proxy = Arc::clone(&proxy);
        let old = tokio::spawn(async move {
            old_proxy
                .request("GET /old HTTP/1.1\r\nHost: workers.test\r\n")
                .await
        });
        wait_for_queued_requests(&proxy.pools[0], 1).await;
        let new_proxy = Arc::clone(&proxy);
        let new = tokio::spawn(async move {
            new_proxy
                .request("GET /new HTTP/1.1\r\nHost: workers.test\r\n")
                .await
        });
        wait_for_queued_requests(&proxy.pools[0], 2).await;

        release_secondary_tx
            .send(())
            .expect("release secondary capacity");
        assert_eq!(
            held_secondary.await.expect("secondary held task").body,
            b"hold-secondary"
        );
        timeout(Duration::from_secs(30), old_seen_rx)
            .await
            .expect("old request dispatch")
            .expect("old request signal");
        wait_for_queued_requests(&proxy.pools[0], 1).await;

        release_primary_tx
            .send(())
            .expect("release primary capacity");
        assert_eq!(
            held_primary.await.expect("primary held task").body,
            b"hold-primary"
        );
        timeout(Duration::from_secs(30), new_seen_rx)
            .await
            .expect("new request dispatch")
            .expect("new request signal");
        assert_eq!(new.await.expect("new request task").body, b"new");
        release_old_tx.send(()).expect("release old response");
        assert_eq!(old.await.expect("old request task").body, b"old");
        wait_for_queued_requests(&proxy.pools[0], 0).await;
        primary_worker.await.expect("primary worker task");
        secondary_worker.await.expect("secondary worker task");
        proxy.wait_for_no_active_leases().await;
        let proxy = Arc::into_inner(proxy).expect("proxy harness owner");
        proxy.finish().await;
    })
    .await
    .expect("FIFO worker dispatch test timed out");
}

#[tokio::test]
async fn maxconn_one_serializes_concurrent_h1_creation_and_reuses_the_physical_connection() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("capped origin bind");
        let address = listener.local_addr().expect("capped origin address");
        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_by_origin = Arc::clone(&accepted);
        let (first_seen_tx, first_seen_rx) = oneshot::channel();
        let (release_first_tx, release_first_rx) = oneshot::channel();
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("capped origin accept");
            accepted_by_origin.fetch_add(1, Ordering::SeqCst);
            read_request_head(&mut stream)
                .await
                .expect("first capped request");
            first_seen_tx.send(()).expect("first capped request signal");
            release_first_rx
                .await
                .expect("release first capped request");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: keep-alive\r\n\r\nfirst",
                )
                .await
                .expect("first capped response");
            read_request_head(&mut stream)
                .await
                .expect("second capped request");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecond",
                )
                .await
                .expect("second capped response");
        });
        let proxy = Arc::new(
            ProxyHarness::start(
                vec![capped_pool("capped", address, 1)],
                vec![route(None, "/", &[], "capped")],
                1024,
                2,
            )
            .await,
        );
        let first_proxy = Arc::clone(&proxy);
        let first =
            tokio::spawn(async move { keepalive_request(first_proxy.address, "/first").await });
        first_seen_rx.await.expect("first request reached origin");
        let second_proxy = Arc::clone(&proxy);
        let second =
            tokio::spawn(async move { keepalive_request(second_proxy.address, "/second").await });
        sleep(Duration::from_millis(50)).await;
        assert_eq!(accepted.load(Ordering::SeqCst), 1);
        assert_eq!(
            proxy.pools[0].health_snapshot().endpoints[0].active_connections,
            1
        );
        release_first_tx.send(()).expect("release first response");
        assert_eq!(first.await.expect("first request task").body, b"first");
        assert_eq!(second.await.expect("second request task").body, b"second");
        origin.await.expect("capped origin task");
        assert_eq!(accepted.load(Ordering::SeqCst), 1);
        proxy.wait_for_no_active_leases().await;
        let proxy = Arc::into_inner(proxy).expect("proxy harness owner");
        proxy.finish().await;
    })
    .await
    .expect("concurrent capped H1 test timed out");
}

#[tokio::test]
async fn enforces_the_connection_cap_in_the_production_http_wrapper() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start("limited", 1).await;
        let proxy = ProxyHarness::start_with_limit(
            vec![pool("default", &[origin.address])],
            vec![route(None, "/", &[], "default")],
            1024,
            1,
            2,
        )
        .await;

        let mut first = TcpStream::connect(proxy.address)
            .await
            .expect("first client connect");
        first
            .write_all(b"GET / HTTP/1.1\r\nHost: limited.test\r\n")
            .await
            .expect("partial first request");
        proxy.wait_for_active_connections(1).await;

        let mut second = TcpStream::connect(proxy.address)
            .await
            .expect("second client connect");
        let mut byte = [0; 1];
        let closed = timeout(Duration::from_secs(30), second.read(&mut byte))
            .await
            .expect("limited connection must close promptly");
        assert!(
            matches!(closed, Ok(0) | Err(_)),
            "limited connection stayed open"
        );

        let snapshot = proxy.metrics.snapshot().expect("connection-cap snapshot");
        assert_eq!(snapshot.traffic.accepted_connections, 2);
        assert_eq!(snapshot.traffic.rejected_connections, 1);
        assert_eq!(snapshot.traffic.active_connections, 1);

        drop(first);
        proxy.finish().await;
        origin.assert_not_contacted().await;
    })
    .await
    .expect("connection-cap test timed out");
}

async fn keepalive_request(address: SocketAddr, path: &str) -> RawResponse {
    let mut stream = TcpStream::connect(address)
        .await
        .expect("keepalive downstream connect");
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: capped.test\r\nConnection: keep-alive\r\n\r\n")
                .as_bytes(),
        )
        .await
        .expect("keepalive downstream request");
    read_framed_response(&mut stream)
        .await
        .expect("keepalive downstream response")
}
