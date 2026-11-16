use crate::TEST_TIMEOUT;
use crate::http_support::HttpResponse as RawResponse;
use crate::http_support::read_request_head as read_request_head_bytes;
use crate::support::builders::pool;
use crate::support::builders::route;
use crate::support::harness::ProxyHarness;
use crate::support::origin::Origin;
use crate::support::wire::assert_origin_response;
use crate::support::wire::read_framed_response;
use crate::support::wire::read_response_head;
use oxiroute_config::HttpPathSelector;
use oxiroute_config::HttpRoute;
use oxiroute_config::HttpRouteAction;
use std::io;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::time::sleep;
use tokio::time::timeout;

#[tokio::test]
async fn rejects_oversized_content_length_before_contacting_an_origin() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start("oversized", 1).await;
        let proxy = ProxyHarness::start(
            vec![pool("upload", &[origin.address])],
            vec![route(None, "/upload", &["POST"], "upload")],
            8,
            1,
        )
        .await;

        let response = proxy
            .request("POST /upload HTTP/1.1\r\nHost: upload.test\r\nContent-Length: 9\r\n")
            .await;

        assert_eq!(response.status, 413, "response: {}", response.text());
        proxy.finish().await;
        origin.assert_not_contacted().await;
    })
    .await
    .expect("oversized Content-Length test timed out");
}

#[tokio::test]
async fn rejects_a_chunked_body_when_streaming_crosses_the_limit() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start_silent().await;
        let proxy = ProxyHarness::start(
            vec![pool("upload", &[origin.address])],
            vec![route(None, "/upload", &["POST"], "upload")],
            8,
            1,
        )
        .await;

        let response = proxy
            .request_bytes(
                b"POST /upload HTTP/1.1\r\n\
                  Host: upload.test\r\n\
                  Transfer-Encoding: chunked\r\n\
                  Connection: close\r\n\
                  \r\n\
                  4\r\n1234\r\n\
                  5\r\n56789\r\n\
                  0\r\n\r\n",
            )
            .await;

        assert_eq!(response.status, 413, "response: {}", response.text());
        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("chunked overflow test timed out");
}

#[tokio::test]
async fn an_unbounded_body_limit_streams_without_size_rejection() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start("unbounded", 1).await;
        let proxy = ProxyHarness::start_unbounded(
            vec![pool("upload", &[origin.address])],
            vec![route(None, "/upload", &["POST"], "upload")],
            1,
        )
        .await;
        let body = vec![b'x'; 32 * 1024];
        let mut request = format!(
            "POST /upload HTTP/1.1\r\nHost: upload.test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        request.extend_from_slice(&body);

        let response = proxy.request_bytes(&request).await;
        assert_origin_response(&response, "unbounded");

        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("unbounded body test timed out");
}

#[tokio::test]
async fn paused_upload_receives_early_final_response_without_spinning() {
    timeout(TEST_TIMEOUT, async {
        const ATTEMPTS: usize = 32;
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("early-final origin bind");
        let address = listener.local_addr().expect("early-final origin address");
        let origin = tokio::spawn(async move {
            for attempt in 0..ATTEMPTS {
                let (mut stream, _) = listener.accept().await.expect("early-final accept");
                read_request_head_bytes(&mut stream)
                    .await
                    .expect("early-final request head");
                if attempt % 3 == 0 {
                    tokio::task::yield_now().await;
                }
                stream
                    .write_all(
                        b"HTTP/1.1 413 Payload Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await
                    .expect("early-final response");
            }
        });
        let proxy = ProxyHarness::start(
            vec![pool("upload", &[address])],
            vec![route(None, "/upload", &["POST"], "upload")],
            2 * 1024 * 1024,
            ATTEMPTS,
        )
        .await;

        for attempt in 0..ATTEMPTS {
            let mut client = TcpStream::connect(proxy.address)
                .await
                .expect("early-final client");
            client
                .write_all(
                    b"POST /upload HTTP/1.1\r\nHost: upload.test\r\nContent-Length: 1048576\r\nConnection: close\r\n\r\npartial",
                )
                .await
                .expect("partial upload");
            if attempt % 2 == 0 {
                tokio::task::yield_now().await;
            }
            let response = RawResponse::parse(
                read_response_head(&mut client)
                    .await
                    .expect("early-final downstream response"),
            );
            assert_eq!(response.status, 413);
        }

        origin.await.expect("early-final origin task");
        proxy.wait_for_no_active_leases().await;
        proxy.finish().await;
    })
    .await
    .expect("paused upload early-final test timed out");
}

#[tokio::test]
async fn slow_downstream_preserves_response_larger_than_pipe_capacity() {
    timeout(TEST_TIMEOUT, async {
        const CHUNKS: usize = 64;
        const CHUNK_SIZE: usize = 512 * 1024;
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("slow-response origin bind");
        let address = listener.local_addr().expect("slow-response origin address");
        let (write_done_tx, mut write_done_rx) = oneshot::channel();
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("slow-response accept");
            read_request_head_bytes(&mut stream)
                .await
                .expect("slow-response request");
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        CHUNKS * CHUNK_SIZE
                    )
                    .as_bytes(),
                )
                .await
                .expect("slow-response head");
            for value in 0..CHUNKS {
                let value = u8::try_from(value).expect("response chunk index fits in u8");
                stream
                    .write_all(&vec![value; CHUNK_SIZE])
                    .await
                    .expect("slow-response chunk");
                if value % 3 == 0 {
                    tokio::task::yield_now().await;
                }
            }
            write_done_tx
                .send(())
                .expect("report completed origin write");
        });
        let proxy = ProxyHarness::start(
            vec![pool("slow", &[address])],
            vec![route(None, "/", &[], "slow")],
            1024,
            1,
        )
        .await;
        let mut client = TcpStream::connect(proxy.address)
            .await
            .expect("slow-response client");
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: slow.test\r\nConnection: close\r\n\r\n")
            .await
            .expect("slow-response request write");
        sleep(Duration::from_millis(25)).await;
        assert!(
            timeout(Duration::from_millis(50), &mut write_done_rx)
                .await
                .is_err(),
            "origin did not block after the four-slot handoff and socket buffers filled"
        );

        let mut wire = Vec::new();
        let mut buffer = [0; 1024];
        loop {
            let read = client
                .read(&mut buffer)
                .await
                .expect("slow downstream read");
            if read == 0 {
                break;
            }
            wire.extend_from_slice(&buffer[..read]);
            tokio::task::yield_now().await;
        }
        let response = RawResponse::parse(wire);
        assert_eq!(response.status, 200);
        assert_eq!(response.body().len(), CHUNKS * CHUNK_SIZE);
        for (value, chunk) in response
            .body()
            .as_chunks::<CHUNK_SIZE>()
            .0
            .iter()
            .enumerate()
        {
            let value = u8::try_from(value).expect("response chunk index fits in u8");
            assert!(chunk.iter().all(|byte| *byte == value));
        }
        timeout(Duration::from_secs(30), write_done_rx)
            .await
            .expect("origin did not resume after downstream reads")
            .expect("origin write completion sender dropped");

        origin.await.expect("slow-response origin task");
        proxy.wait_for_no_active_leases().await;
        proxy.finish().await;
    })
    .await
    .expect("slow downstream response test timed out");
}

#[tokio::test]
async fn downstream_disconnect_releases_origin_blocked_by_response_backpressure() {
    timeout(TEST_TIMEOUT, async {
        const CHUNKS: usize = 1024;
        const CHUNK_SIZE: usize = 64 * 1024;
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("disconnect origin bind");
        let address = listener.local_addr().expect("disconnect origin address");
        let (started_tx, started_rx) = oneshot::channel();
        let (result_tx, mut result_rx) = oneshot::channel();
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("disconnect origin accept");
            read_request_head_bytes(&mut stream)
                .await
                .expect("disconnect origin request");
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        CHUNKS * CHUNK_SIZE
                    )
                    .as_bytes(),
                )
                .await
                .expect("disconnect response head");
            started_tx.send(()).expect("report response start");
            let chunk = vec![b'x'; CHUNK_SIZE];
            let mut result = Ok(());
            for _ in 0..CHUNKS {
                if let Err(error) = stream.write_all(&chunk).await {
                    result = Err(error);
                    break;
                }
            }
            result_tx.send(result).expect("report origin write result");
        });
        let proxy = ProxyHarness::start(
            vec![pool("disconnect", &[address])],
            vec![route(None, "/", &[], "disconnect")],
            1024,
            1,
        )
        .await;
        let mut client = TcpStream::connect(proxy.address)
            .await
            .expect("disconnect client");
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: disconnect.test\r\nConnection: close\r\n\r\n")
            .await
            .expect("disconnect request");
        started_rx.await.expect("origin started response");
        assert!(
            timeout(Duration::from_millis(50), &mut result_rx)
                .await
                .is_err(),
            "origin response completed before downstream disconnect"
        );
        drop(client);
        assert!(
            timeout(Duration::from_secs(2), result_rx)
                .await
                .expect("origin stayed blocked after downstream disconnect")
                .expect("origin result sender dropped")
                .is_err(),
            "origin unexpectedly wrote the complete response after disconnect"
        );

        origin.await.expect("disconnect origin task");
        proxy.wait_for_no_active_leases().await;
        proxy.finish().await;
    })
    .await
    .expect("downstream disconnect backpressure test timed out");
}

#[tokio::test]
async fn saturated_upload_receives_early_final_then_observes_upstream_close() {
    timeout(TEST_TIMEOUT, async {
        const CHUNKS: usize = 512;
        const CHUNK_SIZE: usize = 64 * 1024;
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("saturated-upload origin bind");
        let address = listener.local_addr().expect("saturated-upload origin address");
        let (head_seen_tx, head_seen_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("saturated-upload accept");
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                head.push(stream.read_u8().await.expect("saturated-upload head"));
            }
            head_seen_tx.send(()).expect("report upload head");
            release_rx.await.expect("release early final");
            stream
                .write_all(
                    b"HTTP/1.1 413 Payload Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("saturated-upload early final");
        });
        let proxy = ProxyHarness::start(
            vec![pool("upload", &[address])],
            vec![route(None, "/upload", &["POST"], "upload")],
            64 * 1024 * 1024,
            1,
        )
        .await;
        let client = TcpStream::connect(proxy.address)
            .await
            .expect("saturated-upload client");
        let (mut client_read, mut client_write) = client.into_split();
        client_write
            .write_all(
                format!(
                    "POST /upload HTTP/1.1\r\nHost: upload.test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    CHUNKS * CHUNK_SIZE
                )
                .as_bytes(),
            )
            .await
            .expect("saturated-upload request head");
        head_seen_rx.await.expect("origin observed request head");
        let mut writer = tokio::spawn(async move {
            let chunk = vec![b'u'; CHUNK_SIZE];
            for _ in 0..CHUNKS {
                client_write.write_all(&chunk).await?;
            }
            Ok::<_, io::Error>(())
        });
        assert!(
            timeout(Duration::from_millis(50), &mut writer)
                .await
                .is_err(),
            "upload completed without saturating request backpressure"
        );
        release_tx.send(()).expect("send early final");

        let mut response = Vec::new();
        while !response.ends_with(b"\r\n\r\n") {
            response.push(client_read.read_u8().await.expect("early-final response"));
        }
        assert_eq!(RawResponse::parse(response).status, 413);
        assert!(
            timeout(Duration::from_secs(1), writer)
                .await
                .expect("upload writer stayed blocked after upstream close")
                .expect("upload writer task panicked")
                .is_err(),
            "upload writer did not observe the closed proxy path"
        );

        origin.await.expect("saturated-upload origin task");
        proxy.wait_for_no_active_leases().await;
        proxy.finish().await;
    })
    .await
    .expect("saturated upload early-final test timed out");
}

#[tokio::test]
async fn route_local_body_limits_are_independent_within_one_service() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start("accepted", 1).await;
        let mut small = route(None, "/small", &["POST"], "upload");
        small.policy.max_request_body_bytes = Some(4);
        let mut larger = route(None, "/larger", &["POST"], "upload");
        larger.policy.max_request_body_bytes = Some(8);
        let proxy = ProxyHarness::start(
            vec![pool("upload", &[origin.address])],
            vec![small, larger],
            1024,
            2,
        )
        .await;

        let rejected = proxy
            .request_bytes(
                b"POST /small HTTP/1.1\r\nHost: upload.test\r\nContent-Length: 5\r\nConnection: close\r\n\r\n12345",
            )
            .await;
        assert_eq!(rejected.status, 413);
        let accepted = proxy
            .request_bytes(
                b"POST /larger HTTP/1.1\r\nHost: upload.test\r\nContent-Length: 5\r\nConnection: close\r\n\r\n12345",
            )
            .await;
        assert_origin_response(&accepted, "accepted");

        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("route-local body test timed out");
}

#[tokio::test]
async fn request_buffering_reads_the_complete_body_before_connecting_upstream() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("buffered origin bind");
        let address = listener.local_addr().expect("buffered origin address");
        let mut buffered_route = route(None, "/", &[], "buffered");
        buffered_route.policy.request_buffering = true;
        let proxy = ProxyHarness::start(
            vec![pool("buffered", &[address])],
            vec![buffered_route],
            1024,
            1,
        )
        .await;
        let mut client = TcpStream::connect(proxy.address)
            .await
            .expect("buffered client");
        client
            .write_all(
                b"POST / HTTP/1.1\r\nHost: buffered.test\r\nContent-Length: 8\r\nConnection: close\r\n\r\npart",
            )
            .await
            .expect("partial buffered request");

        assert!(
            timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "origin was contacted before the complete request body arrived"
        );

        client
            .write_all(b"body")
            .await
            .expect("complete buffered request");
        let (mut upstream, _) = listener.accept().await.expect("buffered origin accept");
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = upstream
                .read(&mut chunk)
                .await
                .expect("buffered origin request");
            assert_ne!(read, 0, "buffered origin request ended early");
            request.extend_from_slice(&chunk[..read]);
            if request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .is_some_and(|header_end| request.len() >= header_end + 4 + 8)
            {
                break;
            }
        }
        assert!(request.ends_with(b"partbody"), "request: {request:?}");
        upstream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await
            .expect("buffered origin response");

        let mut response = Vec::new();
        client
            .read_to_end(&mut response)
            .await
            .expect("buffered response");
        assert!(response.ends_with(b"ok"), "response: {response:?}");
        proxy.finish().await;
    })
    .await
    .expect("request buffering test timed out");
}

#[tokio::test]
async fn response_buffering_holds_a_bounded_content_length_body_until_end_of_stream() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("response buffer origin bind");
        let address = listener
            .local_addr()
            .expect("response buffer origin address");
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener
                .accept()
                .await
                .expect("response buffer origin accept");
            read_request_head_bytes(&mut stream)
                .await
                .expect("response buffer origin request");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabc")
                .await
                .expect("response buffer first chunk");
            sleep(Duration::from_millis(100)).await;
            stream
                .write_all(b"def")
                .await
                .expect("response buffer final chunk");
        });
        let mut buffered_route = route(None, "/", &[], "buffered-response");
        buffered_route.policy.max_request_body_bytes = Some(6);
        buffered_route.policy.response_buffering = true;
        let proxy = ProxyHarness::start(
            vec![pool("buffered-response", &[address])],
            vec![buffered_route],
            6,
            1,
        )
        .await;
        let mut client = TcpStream::connect(proxy.address)
            .await
            .expect("response buffer client");
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: buffered.test\r\nConnection: close\r\n\r\n")
            .await
            .expect("response buffer request");
        let response_head = read_response_head(&mut client)
            .await
            .expect("response buffer response head");
        assert!(
            response_head.starts_with(b"HTTP/1.1 200"),
            "response: {:?}",
            String::from_utf8_lossy(&response_head)
        );
        let mut body = [0; 6];
        assert!(
            timeout(Duration::from_millis(50), client.read(&mut body))
                .await
                .is_err(),
            "buffered response body became visible before the origin completed"
        );
        client
            .read_exact(&mut body)
            .await
            .expect("buffered response body");
        assert_eq!(&body, b"abcdef");

        proxy.finish().await;
        origin.await.expect("response buffer origin task");
    })
    .await
    .expect("response buffering test timed out");
}

#[tokio::test]
async fn response_buffering_rejects_unbounded_and_oversized_bodies() {
    timeout(TEST_TIMEOUT, async {
        let chunked = Origin::start_chunked("abcdef", 1).await;
        let mut chunked_route = route(None, "/", &[], "chunked-response");
        chunked_route.policy.max_request_body_bytes = Some(6);
        chunked_route.policy.response_buffering = true;
        let chunked_proxy = ProxyHarness::start(
            vec![pool("chunked-response", &[chunked.address])],
            vec![chunked_route],
            6,
            1,
        )
        .await;
        let chunked_response = chunked_proxy
            .request("GET / HTTP/1.1\r\nHost: chunked.test\r\n")
            .await;
        assert_eq!(chunked_response.status, 502);
        chunked_proxy.finish().await;
        chunked.finish().await;

        let oversized = Origin::start_status(200, "1234567").await;
        let mut oversized_route = route(None, "/", &[], "oversized-response");
        oversized_route.policy.max_request_body_bytes = Some(6);
        oversized_route.policy.response_buffering = true;
        let oversized_proxy = ProxyHarness::start(
            vec![pool("oversized-response", &[oversized.address])],
            vec![oversized_route],
            6,
            1,
        )
        .await;
        let oversized_response = oversized_proxy
            .request("GET / HTTP/1.1\r\nHost: oversized.test\r\n")
            .await;
        assert_eq!(oversized_response.status, 502);
        oversized_proxy.finish().await;
        oversized.finish().await;
    })
    .await
    .expect("response buffering rejection test timed out");
}

#[tokio::test]
async fn early_response_drains_a_remaining_request_body_before_h1_reuse() {
    timeout(TEST_TIMEOUT, async {
        let route = HttpRoute {
            host: None,
            path: HttpPathSelector::SegmentPrefix { value: "/".into() },
            methods: Vec::new(),
            access_policy: None,
            policy: oxiroute_config::HttpRoutePolicy::default(),
            action: HttpRouteAction::FixedResponse {
                status: 200,
                body: "ok".into(),
                headers: Vec::new(),
            },
        };
        let proxy = ProxyHarness::start(
            Vec::new(),
            vec![route],
            1024,
            1,
        )
        .await;
        let mut client = TcpStream::connect(proxy.address)
            .await
            .expect("drain client");
        client
            .write_all(
                b"POST /first HTTP/1.1\r\nHost: drain.test\r\nContent-Length: 4\r\nConnection: keep-alive\r\n\r\nab",
            )
            .await
            .expect("partial request body");
        let first = read_framed_response(&mut client)
            .await
            .expect("early response");
        assert_eq!(first.status, 200);
        assert_eq!(first.body(), b"ok");

        client
            .write_all(b"cdGET /second HTTP/1.1\r\nHost: drain.test\r\nConnection: close\r\n\r\n")
            .await
            .expect("remaining body and second request");
        let second = timeout(Duration::from_secs(30), read_framed_response(&mut client))
            .await
            .expect("drained H1 connection did not accept the next request")
            .expect("second response");
        assert_eq!(second.status, 200);
        assert_eq!(second.body(), b"ok");

        proxy.finish().await;
    })
    .await
    .expect("request drain test timed out");
}
