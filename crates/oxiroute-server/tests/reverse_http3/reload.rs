#[allow(clippy::wildcard_imports)]
use super::*;

#[tokio::test]
pub(super) async fn daemon_reloads_reverse_h3_while_active_request_drains_and_rejects_new_streams()
{
    reload_reverse_h3_while_a_request_is_in_flight(Duration::ZERO).await;
}

#[tokio::test]
pub(super) async fn reverse_h3_request_outlives_the_old_five_second_retirement_drain() {
    reload_reverse_h3_while_a_request_is_in_flight(Duration::from_millis(6_500)).await;
}

#[allow(clippy::too_many_lines)]
pub(super) async fn reload_reverse_h3_while_a_request_is_in_flight(hold: Duration) {
    let (origin_address, origin_started, release_origin, origin_task) = spawn_slow_h3_origin();
    let management_address = process_support::reserve_tcp_address();
    let listener_address = reserve_udp_address();
    let key = fixture_support::private_key_fixture("proxy-a-key.pem");
    let mut config = reverse_config(
        listener_address,
        key.path(),
        reverse_service(HttpRouteAction::Proxy {
            upstream_pool: "origin".into(),
            policy: HttpProxyPolicy::default().into(),
        }),
        vec![h3_pool(origin_address)],
        Some(8),
    );
    config.http_services[0].upstream_io_timeout_ms = 20_000;
    config.management = Some(Management {
        bind: management_address,
        ui_dir: None,
    });
    let active_config = config.clone().validate().expect("valid active H3 config");
    let mut server = process_support::ServerProcess::start(&active_config, Some(TEST_TOKEN));
    server.wait_for_tcp(management_address).await;
    let authorization = format!("Bearer {TEST_TOKEN}");
    let original_revision = active_revision(management_address, &authorization).await;
    let endpoint = client_endpoint().expect("H3 client endpoint");
    let old_connection = connect_h3(&endpoint, listener_address).await;
    let (driver, mut sender) = h3::client::new(h3_quinn::Connection::new(old_connection))
        .await
        .expect("old H3 client connection");
    let driver = drive_client(driver);
    let mut old_stream = sender
        .send_request(fixed_request("/active"))
        .await
        .expect("send active H3 request");
    old_stream.finish().await.expect("finish active H3 request");
    origin_started
        .await
        .expect("active H3 request reached origin");

    config.http_services[0].routes[0].action = HttpRouteAction::FixedResponse {
        status: 200,
        body: "candidate-h3".into(),
        headers: Vec::new(),
    };
    process_support::write_config(
        &server.config_path,
        &config.validate().expect("valid candidate H3 config"),
    );
    wait_for_new_revision(management_address, &authorization, &original_revision).await;

    assert!(
        timeout(Duration::from_millis(100), old_stream.recv_response())
            .await
            .is_err(),
        "active H3 request completed before the origin was released"
    );

    timeout(Duration::from_secs(10), async {
        loop {
            let candidate_connection = connect_h3(&endpoint, listener_address).await;
            let raw_connection = candidate_connection.clone();
            let (candidate_driver, mut candidate_sender) =
                h3::client::new(h3_quinn::Connection::new(candidate_connection))
                    .await
                    .expect("candidate H3 client connection");
            let candidate_driver = drive_client(candidate_driver);
            let response = timeout(Duration::from_millis(500), async {
                let mut stream = candidate_sender
                    .send_request(fixed_request("/candidate"))
                    .await
                    .ok()?;
                stream.finish().await.ok()?;
                let response = stream.recv_response().await.ok()?;
                if response.status() != StatusCode::OK {
                    return None;
                }
                Some(recv_body(&mut stream).await)
            })
            .await
            .ok()
            .flatten();
            drop(candidate_sender);
            raw_connection.close(quinn::VarInt::from_u32(0), b"candidate probe complete");
            candidate_driver.abort();
            let _ = candidate_driver.await;
            if response == Some(Bytes::from_static(b"candidate-h3")) {
                return;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("candidate H3 generation activation timeout");

    sleep(hold).await;
    release_origin.send(()).expect("release active H3 request");
    let response = timeout(Duration::from_secs(10), old_stream.recv_response())
        .await
        .expect("active H3 response timeout")
        .expect("active H3 response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        recv_body(&mut old_stream).await,
        Bytes::from_static(b"origin-active")
    );
    assert_eq!(
        old_stream
            .recv_trailers()
            .await
            .expect("active H3 response trailers")
            .expect("origin H3 response trailers")
            .get("x-origin-trailer"),
        Some(&HeaderValue::from_static("complete"))
    );

    let rejected_after_goaway = timeout(Duration::from_secs(2), async {
        let request = fixed_request("/after-goaway");
        let Ok(mut stream) = sender.send_request(request).await else {
            return true;
        };
        if stream.finish().await.is_err() {
            return true;
        }
        stream.recv_response().await.is_err()
    })
    .await
    .unwrap_or(true);
    assert!(
        rejected_after_goaway,
        "old H3 connection accepted a request after generation GOAWAY"
    );

    drop(old_stream);
    drop(sender);
    endpoint.close(quinn::VarInt::from_u32(0), b"test complete");
    driver.await.expect("old H3 driver task");
    origin_task.await.expect("slow H3 origin task");
    server.shutdown_gracefully();
}

pub(super) async fn active_revision(address: SocketAddr, authorization: &str) -> String {
    http_support::http_request(
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

pub(super) async fn wait_for_new_revision(
    address: SocketAddr,
    authorization: &str,
    original: &str,
) {
    timeout(Duration::from_secs(10), async {
        loop {
            let status = http_support::http_request(
                address,
                "GET",
                "/api/v1/status",
                &[("Authorization", authorization)],
                &[],
            )
            .await;
            assert_eq!(status.status, 200);
            if status.json()["activeRevision"].as_str() != Some(original) {
                return;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("generation reload timed out");
}

pub(super) fn origin_server_config() -> quinn::ServerConfig {
    let mut certificate_reader = BufReader::new(
        fs::File::open(fixture_support::fixture("origin.pem")).expect("origin certificate"),
    );
    let certificates = CertificateDer::pem_reader_iter(&mut certificate_reader)
        .collect::<Result<Vec<_>, _>>()
        .expect("origin certificate chain");
    let mut key_reader = BufReader::new(
        fs::File::open(fixture_support::fixture("origin-key.pem")).expect("origin key"),
    );
    let private_key = PrivateKeyDer::pem_reader_iter(&mut key_reader)
        .next()
        .transpose()
        .expect("origin private key")
        .expect("origin private key block");
    let mut crypto =
        rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_no_client_auth()
            .with_single_cert(certificates, private_key)
            .expect("origin TLS identity");
    crypto.alpn_protocols = vec![H3_ALPN.to_vec()];
    crypto.max_early_data_size = 0;
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(
        QuicServerConfig::try_from(crypto).expect("origin QUIC TLS configuration"),
    ));
    config.migration(false);
    config
}

pub(super) fn spawn_slow_h3_origin() -> (
    SocketAddr,
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let endpoint = quinn::Endpoint::server(origin_server_config(), (Ipv4Addr::LOCALHOST, 0).into())
        .expect("origin endpoint");
    let address = endpoint.local_addr().expect("origin address");
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let incoming = endpoint.accept().await.expect("origin accept");
        let connection = incoming.await.expect("origin QUIC connection");
        let mut h3 = h3::server::builder()
            .build(h3_quinn::Connection::new(connection))
            .await
            .expect("origin H3 connection");
        let resolver = h3
            .accept()
            .await
            .expect("origin H3 accept")
            .expect("origin H3 request");
        let (_request, mut stream) = resolver.resolve_request().await.expect("origin request");
        assert!(
            stream
                .recv_data()
                .await
                .expect("origin request body")
                .is_none()
        );
        started_tx.send(()).expect("signal origin request");
        release_rx.await.expect("release origin request");
        stream
            .send_response(
                Response::builder()
                    .status(StatusCode::OK)
                    .header("content-length", "13")
                    .body(())
                    .expect("origin response"),
            )
            .await
            .expect("origin response headers");
        stream
            .send_data(Bytes::from_static(b"origin-active"))
            .await
            .expect("origin response body");
        let mut trailers = HeaderMap::new();
        trailers.insert("x-origin-trailer", HeaderValue::from_static("complete"));
        stream
            .send_trailers(trailers)
            .await
            .expect("origin response trailers");
        stream.finish().await.expect("origin response finish");
        let _ = h3.accept().await;
    });
    (address, started_rx, release_tx, task)
}
