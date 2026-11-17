use crate::TEST_TIMEOUT;
use crate::http_support::HttpResponse as RawResponse;
use crate::http_support::read_request_head as read_request_head_bytes;
use crate::support::builders::capped_pool;
use crate::support::builders::pool;
use crate::support::builders::route;
use crate::support::harness::ProxyHarness;
use crate::support::origin::StatusOrigin;
use crate::support::wire::wait_for_queued_requests;
use oxiroute_config::HttpProxyPolicy;
use oxiroute_config::HttpRouteAction;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::timeout;

#[tokio::test]
async fn latency_outlier_is_ejected_and_stops_receiving_requests() {
    timeout(Duration::from_secs(30), async {
        let origins = [
            StatusOrigin::start(200, "fast-a").await,
            StatusOrigin::start_delayed(200, "slow", Duration::from_millis(150)).await,
            StatusOrigin::start(200, "fast-b").await,
        ];
        let mut farm = pool("farm", &origins.each_ref().map(|origin| origin.address));
        farm.passive_health = Some(oxiroute_config::PassiveHealthPolicy {
            outlier_detection: Some(oxiroute_config::OutlierDetection {
                factor_percent: 300,
                min_samples: 2,
                interval_ms: 500,
                max_ejected_percent: 50,
            }),
            ..oxiroute_config::PassiveHealthPolicy::default()
        });
        let proxy = ProxyHarness::start_with_retries(
            vec![farm],
            vec![route(None, "/", &[], "farm")],
            0,
            40,
        )
        .await;

        let mut served = Vec::new();
        for _ in 0..40 {
            let response = proxy
                .request("GET /run HTTP/1.1\r\nHost: slow.test\r\n")
                .await;
            served.push(response.header("x-origin").expect("origin").to_owned());
        }

        let endpoint = &proxy.pools[0].health_snapshot().endpoints[1];
        assert!(endpoint.passive_ejected, "the slow server is ejected");
        assert_eq!(
            endpoint.passive_ejection_reason,
            Some(oxiroute_server::HealthFailure::Slow)
        );
        assert!(
            served[served.len() - 10..]
                .iter()
                .all(|origin| origin != "slow"),
            "no request reaches it after the ejection: {served:?}"
        );
        proxy.finish().await;
    })
    .await
    .expect("outlier test timed out");
}

#[tokio::test]
async fn queue_classes_serve_interactive_requests_before_earlier_batch_requests() {
    timeout(TEST_TIMEOUT, async {
        let origin = StatusOrigin::start_delayed(200, "worker", Duration::from_millis(250)).await;
        let mut workers = capped_pool("workers", origin.address, 1);
        workers.queue = Some(oxiroute_config::PoolQueuePolicy {
            classes: ["interactive", "batch"]
                .map(|name| oxiroute_config::PoolQueueClass {
                    name: name.into(),
                    weight: 1,
                })
                .to_vec(),
            discipline: oxiroute_config::QueueDiscipline::Strict,
            class_header: Some("x-priority".into()),
            default_class: Some("batch".into()),
            ..oxiroute_config::PoolQueuePolicy::default()
        });
        let proxy = Arc::new(
            ProxyHarness::start(
                vec![workers],
                vec![route(None, "/", &[], "workers")],
                1024,
                3,
            )
            .await,
        );
        let finished = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut requests = Vec::new();
        for (name, class) in [
            ("first", "batch"),
            ("batch", "batch"),
            ("interactive", "interactive"),
        ] {
            let (proxy, finished) = (Arc::clone(&proxy), Arc::clone(&finished));
            requests.push(tokio::spawn(async move {
                let response = proxy
                    .request(&format!(
                        "GET /run HTTP/1.1\r\nHost: classes.test\r\nX-Priority: {class}\r\n"
                    ))
                    .await;
                assert_eq!(response.status, 200, "{name}");
                finished.lock().expect("finished").push(name);
            }));
            tokio::time::sleep(Duration::from_millis(60)).await;
        }
        for request in requests {
            request.await.expect("request task");
        }

        assert_eq!(
            *finished.lock().expect("finished"),
            ["first", "interactive", "batch"]
        );
        Arc::into_inner(proxy).expect("proxy").finish().await;
    })
    .await
    .expect("queue class test timed out");
}

async fn queued_request_response(queue_headers: bool) -> (RawResponse, RawResponse) {
    let origin = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let address = origin.local_addr().expect("origin address");
    let (held_tx, held_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut held, _) = origin.accept().await.expect("held accept");
        read_request_head_bytes(&mut held)
            .await
            .expect("held request");
        held_tx.send(()).expect("held signal");
        release_rx.await.expect("release");
        let reply = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
        held.write_all(reply).await.expect("held response");
        let (mut queued, _) = origin.accept().await.expect("queued accept");
        read_request_head_bytes(&mut queued)
            .await
            .expect("queued request");
        queued.write_all(reply).await.expect("queued response");
    });
    let mut queue_route = route(None, "/", &[], "qh");
    queue_route.action = HttpRouteAction::Proxy {
        upstream_pool: "qh".into(),
        policy: HttpProxyPolicy {
            queue_headers,
            ..HttpProxyPolicy::default()
        }
        .into(),
    };
    let proxy = Arc::new(
        ProxyHarness::start(
            vec![capped_pool("qh", address, 1)],
            vec![queue_route],
            1024,
            2,
        )
        .await,
    );
    let held_proxy = Arc::clone(&proxy);
    let held = tokio::spawn(async move {
        held_proxy
            .request("GET /a HTTP/1.1\r\nHost: qh.test\r\n")
            .await
    });
    held_rx.await.expect("held request reached the origin");
    let queued_proxy = Arc::clone(&proxy);
    let queued = tokio::spawn(async move {
        queued_proxy
            .request("GET /b HTTP/1.1\r\nHost: qh.test\r\n")
            .await
    });
    wait_for_queued_requests(&proxy.pools[0], 1).await;
    tokio::time::sleep(Duration::from_millis(40)).await;
    release_tx.send(()).expect("release capacity");
    let responses = (
        held.await.expect("held task"),
        queued.await.expect("queued task"),
    );
    task.await.expect("origin task");
    Arc::into_inner(proxy)
        .expect("last proxy handle")
        .finish()
        .await;
    responses
}

#[tokio::test]
async fn opted_in_routes_tell_a_queued_client_its_position_and_wait() {
    timeout(TEST_TIMEOUT, async {
        let (held, queued) = queued_request_response(true).await;
        assert_eq!(held.header("x-oxiroute-queue-position"), None);
        assert_eq!(queued.status, 200, "response: {}", queued.text());
        assert_eq!(queued.header("x-oxiroute-queue-position"), Some("1"));
        let waited: u64 = queued
            .header("x-oxiroute-queue-wait-ms")
            .expect("wait header")
            .parse()
            .expect("numeric wait");
        assert!(waited >= 30, "waited {waited} ms");
    })
    .await
    .expect("queue headers test timed out");
}

#[tokio::test]
async fn queue_headers_stay_off_unless_the_route_asks_for_them() {
    timeout(TEST_TIMEOUT, async {
        let (_, queued) = queued_request_response(false).await;
        assert_eq!(queued.status, 200, "response: {}", queued.text());
        assert_eq!(queued.header("x-oxiroute-queue-position"), None);
        assert_eq!(queued.header("x-oxiroute-queue-wait-ms"), None);
    })
    .await
    .expect("queue headers off test timed out");
}
