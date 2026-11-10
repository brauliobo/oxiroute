#[allow(clippy::wildcard_imports)]
use super::*;

#[tokio::test]
pub(super) async fn supervised_worker_adopts_h3_and_serves_a_quic_request() {
    let directory = tempfile::tempdir().expect("supervised H3 fixture directory");
    let listener_address = ports_support::unused_address();
    let key = fixture_support::private_key_fixture("proxy-a-key.pem");
    let config = h3_only_config(listener_address, key.path());
    let path = directory.path().join("oxiroute.kdl");
    fs::write(
        &path,
        render_config(
            ConfigFormat::Kdl,
            &config.clone().validate().expect("valid H3 config"),
        ),
    )
    .expect("write H3 config");
    let mut harness = Harness::launch_at(&path, canonical_revision(&path), false);

    harness.poll_until(MasterState::Running);
    verify_listener_descriptors(harness.worker_pid, &harness.listener_targets);
    let status = harness
        .master
        .worker_status(WorkerRole::Active)
        .expect("active H3 worker status");
    assert_eq!(status.listeners.len(), 1);
    assert_eq!(status.listeners[0].name, "h3");
    assert_eq!(status.listeners[0].protocol, "http3");
    assert_eq!(
        status.listeners[0].state,
        oxiroute_supervisor_master::WorkerListenerState::Listening
    );

    let endpoint = h3_client_endpoint().expect("H3 client endpoint");
    let connection = timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(connecting) = endpoint.connect(listener_address, "proxy.example.test")
                && let Ok(connection) = connecting.await
            {
                break connection;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("supervised H3 connection timeout");
    let (driver, mut sender) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("H3 client connection");
    let driver = tokio::spawn(async move {
        let mut driver = driver;
        let _ = std::future::poll_fn(|context| driver.poll_close(context)).await;
    });
    let request = Request::builder()
        .method(Method::GET)
        .uri("https://proxy.example.test/")
        .body(())
        .expect("H3 request");
    let mut stream = sender.send_request(request).await.expect("send H3 request");
    stream.finish().await.expect("finish H3 request");
    let response = stream.recv_response().await.expect("H3 response");
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = stream
        .recv_data()
        .await
        .expect("H3 response body")
        .expect("H3 response data");
    let body = body.copy_to_bytes(body.remaining());
    assert_eq!(body, Bytes::from_static(b"supervised-h3"));
    assert!(stream.recv_data().await.expect("H3 response end").is_none());

    drop(stream);
    drop(sender);
    endpoint.close(quinn::VarInt::from_u32(0), b"test complete");
    driver.await.expect("H3 driver task");

    assert!(matches!(
        harness.master.shutdown(Instant::now()).expect("shutdown"),
        ShutdownProgress::Pending { .. }
    ));
    harness.poll_until(MasterState::Stopped);
    harness.verify_reaped();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::too_many_lines)]
pub(super) async fn supervised_worker_replaces_an_active_h3_request_with_goaway_and_owned_descriptors()
 {
    let directory = tempfile::tempdir().expect("supervised H3 replacement directory");
    let listener_address = ports_support::unused_address();
    let (origin_address, origin_started, release_origin, origin_task) = spawn_slow_h3_origin();
    let key = fixture_support::private_key_fixture("proxy-a-key.pem");
    let initial = h3_proxy_config(listener_address, key.path(), origin_address);
    let mut candidate = initial.clone();
    candidate.upstream_pools.clear();
    candidate.http_services[0].routes[0].action = HttpRouteAction::FixedResponse {
        status: 200,
        body: "candidate-h3".into(),
        headers: Vec::new(),
    };
    let path = directory.path().join("oxiroute.kdl");
    fs::write(
        &path,
        render_config(
            ConfigFormat::Kdl,
            &initial.clone().validate().expect("valid initial H3 config"),
        ),
    )
    .expect("write initial H3 config");
    let mut harness = Harness::launch_at(&path, canonical_revision(&path), false);
    poll_master_until_async(&mut harness, |master| {
        master.state() == MasterState::Running
    })
    .await;
    let baseline_descriptors = matching_descriptor_count(&harness.listener_targets);
    assert_eq!(baseline_descriptors, 2);
    let old_worker = harness
        .master
        .worker_id(WorkerRole::Active)
        .expect("old H3 worker pid");

    let endpoint = h3_client_endpoint().expect("old H3 client endpoint");
    let connection = connect_h3(&endpoint, listener_address).await;
    let (driver, mut sender) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("old H3 client connection");
    let driver = tokio::spawn(async move {
        let mut driver = driver;
        let _ = std::future::poll_fn(|context| driver.poll_close(context)).await;
    });
    let mut old_stream = sender
        .send_request(
            Request::builder()
                .method(Method::GET)
                .uri("https://proxy.example.test/")
                .body(())
                .expect("old H3 request"),
        )
        .await
        .expect("send old H3 request");
    old_stream.finish().await.expect("finish old H3 request");
    origin_started.await.expect("old H3 request reached origin");

    fs::write(
        &path,
        render_config(
            ConfigFormat::Kdl,
            &candidate
                .clone()
                .validate()
                .expect("valid candidate H3 config"),
        ),
    )
    .expect("write candidate H3 config");
    replace_real_worker(
        &mut harness.master,
        &path,
        canonical_revision(&path),
        2,
        false,
    );
    poll_master_until_async(&mut harness, |master| {
        master.state() == MasterState::Quiescing
    })
    .await;

    poll_master_until_async(&mut harness, |master| {
        master.state() == MasterState::DrainingRetired
            && master
                .worker_status(WorkerRole::Retired)
                .is_some_and(|status| !status.accepting)
    })
    .await;
    assert_eq!(
        harness.master.worker_id(WorkerRole::Retired),
        Some(old_worker)
    );
    assert_eq!(
        harness.master.worker_state(WorkerRole::Retired),
        Some(oxiroute_supervisor_master::WorkerState::Running)
    );
    release_origin.send(()).expect("release active H3 request");

    let response = timeout(TEST_TIMEOUT, old_stream.recv_response())
        .await
        .expect("active H3 response timeout")
        .expect("active H3 response");
    let mut body = old_stream
        .recv_data()
        .await
        .expect("active H3 response body")
        .expect("active H3 response data");
    let body = body.copy_to_bytes(body.remaining());
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "active H3 response body: {body:?}"
    );
    assert_eq!(body, Bytes::from_static(b"origin-active"));
    assert!(
        old_stream
            .recv_data()
            .await
            .expect("active H3 response end")
            .is_none()
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

    let rejected_after_goaway = (timeout(Duration::from_secs(30), async {
        let request = Request::builder()
            .method(Method::GET)
            .uri("https://proxy.example.test/")
            .body(())
            .expect("post-GOAWAY H3 request");
        let Ok(mut stream) = sender.send_request(request).await else {
            return true;
        };
        if stream.finish().await.is_err() {
            return true;
        }
        stream.recv_response().await.is_err()
    })
    .await)
        .unwrap_or(true);
    assert!(
        rejected_after_goaway,
        "old H3 connection accepted work after GOAWAY"
    );

    // Both workers read the one UDP socket until the retired worker exits, so a new connection
    // is only deterministic once the old client has left and the retired worker is gone.
    endpoint.close(quinn::VarInt::from_u32(0), b"old H3 complete");
    driver.await.expect("old H3 driver task");
    origin_task.await.expect("slow H3 origin task");
    poll_master_until_async(&mut harness, |master| {
        master.state() == MasterState::Running
    })
    .await;

    let candidate_endpoint = h3_client_endpoint().expect("candidate H3 client endpoint");
    let candidate_connection = connect_h3(&candidate_endpoint, listener_address).await;
    let (candidate_driver, mut candidate_sender) =
        h3::client::new(h3_quinn::Connection::new(candidate_connection))
            .await
            .expect("candidate H3 client connection");
    let candidate_driver = tokio::spawn(async move {
        let mut driver = candidate_driver;
        let _ = std::future::poll_fn(|context| driver.poll_close(context)).await;
    });
    let mut candidate_stream = candidate_sender
        .send_request(
            Request::builder()
                .method(Method::GET)
                .uri("https://proxy.example.test/")
                .body(())
                .expect("candidate H3 request"),
        )
        .await
        .expect("send candidate H3 request");
    candidate_stream
        .finish()
        .await
        .expect("finish candidate H3 request");
    let response = candidate_stream
        .recv_response()
        .await
        .expect("candidate H3 response");
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = candidate_stream
        .recv_data()
        .await
        .expect("candidate H3 response body")
        .expect("candidate H3 response data");
    let body = body.copy_to_bytes(body.remaining());
    assert_eq!(body, Bytes::from_static(b"candidate-h3"));
    assert!(
        candidate_stream
            .recv_data()
            .await
            .expect("candidate H3 response end")
            .is_none()
    );

    candidate_endpoint.close(quinn::VarInt::from_u32(0), b"candidate H3 complete");
    candidate_driver.await.expect("candidate H3 driver task");
    let candidate_worker = harness
        .master
        .worker_id(WorkerRole::Active)
        .expect("candidate H3 worker pid");
    let candidate_launcher = harness
        .master
        .worker_process_group_id(WorkerRole::Active)
        .expect("candidate H3 launcher pid");
    verify_listener_descriptors(candidate_worker, &harness.listener_targets);
    assert_eq!(
        matching_descriptor_count(&harness.listener_targets),
        baseline_descriptors
    );
    assert_eq!(
        StdUdpSocket::bind(listener_address)
            .expect_err("master and candidate must own the H3 listener")
            .kind(),
        io::ErrorKind::AddrInUse
    );

    harness
        .master
        .shutdown(Instant::now())
        .expect("H3 replacement shutdown");
    poll_master_until_async(&mut harness, |master| {
        master.state() == MasterState::Stopped
    })
    .await;
    harness.verify_reaped();
    wait_process_gone(candidate_worker);
    wait_process_gone(candidate_launcher);
    drop(harness);
    assert_udp_can_bind_again(listener_address);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
pub(super) async fn supervised_h3_replacement_rolls_back_after_candidate_rejects_adoption() {
    let directory = tempfile::tempdir().expect("supervised H3 rollback directory");
    let listener_address = ports_support::unused_address();
    let key = fixture_support::private_key_fixture("proxy-a-key.pem");
    let config = h3_only_config(listener_address, key.path());
    let path = directory.path().join("oxiroute.kdl");
    fs::write(
        &path,
        render_config(
            ConfigFormat::Kdl,
            &config.clone().validate().expect("valid H3 rollback config"),
        ),
    )
    .expect("write H3 rollback config");
    let mut harness = Harness::launch_at(&path, canonical_revision(&path), false);
    poll_master_until_async(&mut harness, |master| {
        master.state() == MasterState::Running
    })
    .await;
    let active_worker = harness
        .master
        .worker_id(WorkerRole::Active)
        .expect("active H3 worker pid");

    replace_real_worker(&mut harness.master, &path, "0".repeat(64), 2, false);
    let candidate_worker = harness
        .master
        .worker_id(WorkerRole::Candidate)
        .expect("H3 rollback candidate pid");
    poll_master_until_async(&mut harness, |master| {
        master.state() == MasterState::Running
            && master.active_instance().map(InstanceId::as_str) == Some("oxiroute-stage-2")
    })
    .await;
    assert_eq!(
        harness.master.worker_id(WorkerRole::Active),
        Some(active_worker)
    );
    wait_process_gone(candidate_worker);
    assert_h3_fixed_response(listener_address, b"supervised-h3").await;

    harness
        .master
        .shutdown(Instant::now())
        .expect("H3 rollback shutdown");
    poll_master_until_async(&mut harness, |master| {
        master.state() == MasterState::Stopped
    })
    .await;
    harness.verify_reaped();
    drop(harness);
    assert_udp_can_bind_again(listener_address);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
pub(super) async fn supervised_worker_restarts_and_serves_new_h3_traffic_after_clean_shutdown() {
    let directory = tempfile::tempdir().expect("supervised H3 restart directory");
    let listener_address = ports_support::unused_address();
    let key = fixture_support::private_key_fixture("proxy-a-key.pem");
    let config = h3_only_config(listener_address, key.path());
    let path = directory.path().join("oxiroute.kdl");
    fs::write(
        &path,
        render_config(
            ConfigFormat::Kdl,
            &config.clone().validate().expect("valid H3 restart config"),
        ),
    )
    .expect("write H3 restart config");

    let mut first = Harness::launch_at(&path, canonical_revision(&path), false);
    poll_master_until_async(&mut first, |master| master.state() == MasterState::Running).await;
    assert_h3_fixed_response(listener_address, b"supervised-h3").await;
    first
        .master
        .shutdown(Instant::now())
        .expect("first H3 shutdown");
    poll_master_until_async(&mut first, |master| master.state() == MasterState::Stopped).await;
    first.verify_reaped();
    drop(first);

    let mut restarted = Harness::launch_at(&path, canonical_revision(&path), false);
    poll_master_until_async(&mut restarted, |master| {
        master.state() == MasterState::Running
    })
    .await;
    assert_h3_fixed_response(listener_address, b"supervised-h3").await;
    restarted
        .master
        .shutdown(Instant::now())
        .expect("restarted H3 shutdown");
    poll_master_until_async(&mut restarted, |master| {
        master.state() == MasterState::Stopped
    })
    .await;
    restarted.verify_reaped();
    drop(restarted);
    assert_udp_can_bind_again(listener_address);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
pub(super) async fn supervised_worker_crash_with_active_h3_request_is_reaped_and_releases_listener()
{
    let directory = tempfile::tempdir().expect("supervised H3 crash directory");
    let listener_address = ports_support::unused_address();
    let (origin_address, origin_started, release_origin, origin_task) = spawn_slow_h3_origin();
    let key = fixture_support::private_key_fixture("proxy-a-key.pem");
    let config = h3_proxy_config(listener_address, key.path(), origin_address);
    let path = directory.path().join("oxiroute.kdl");
    fs::write(
        &path,
        render_config(
            ConfigFormat::Kdl,
            &config.clone().validate().expect("valid H3 crash config"),
        ),
    )
    .expect("write H3 crash config");
    let mut harness = Harness::launch_at(&path, canonical_revision(&path), false);
    poll_master_until_async(&mut harness, |master| {
        master.state() == MasterState::Running
    })
    .await;

    let endpoint = h3_client_endpoint().expect("H3 client endpoint");
    let connection = connect_h3(&endpoint, listener_address).await;
    let (driver, mut sender) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("H3 client connection");
    let driver = tokio::spawn(async move {
        let mut driver = driver;
        let _ = std::future::poll_fn(|context| driver.poll_close(context)).await;
    });
    let mut stream = sender
        .send_request(
            Request::builder()
                .method(Method::GET)
                .uri("https://proxy.example.test/")
                .body(())
                .expect("active crash H3 request"),
        )
        .await
        .expect("send active crash H3 request");
    stream
        .finish()
        .await
        .expect("finish active crash H3 request");
    origin_started
        .await
        .expect("active H3 request reached origin");

    let worker = harness
        .master
        .worker_id(WorkerRole::Active)
        .expect("active H3 worker pid");
    let status = Command::new("kill")
        .arg("-KILL")
        .arg(worker.to_string())
        .status()
        .expect("kill active H3 worker");
    assert!(status.success(), "failed to kill active H3 worker");
    poll_master_until_async(&mut harness, |master| master.state() == MasterState::Failed).await;
    harness.verify_reaped();
    let _ = release_origin.send(());
    assert!(
        !matches!(
            timeout(TEST_TIMEOUT, stream.recv_response()).await,
            Ok(Ok(_))
        ),
        "crashed H3 worker completed the active request"
    );
    drop(stream);
    drop(sender);
    endpoint.close(quinn::VarInt::from_u32(0), b"H3 worker crashed");
    driver.await.expect("H3 driver task");
    origin_task.await.expect("slow H3 origin task");
    drop(harness);
    assert_udp_can_bind_again(listener_address);
}

pub(super) fn h3_client_endpoint() -> io::Result<quinn::Endpoint> {
    let mut roots = rustls::RootCertStore::empty();
    let ca = fs::read(fixture_support::fixture("ca-a.pem"))?;
    for certificate in CertificateDer::pem_slice_iter(&ca) {
        roots
            .add(certificate.map_err(io::Error::other)?)
            .map_err(io::Error::other)?;
    }
    let mut crypto = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(std::io::Error::other)?
        .with_root_certificates(roots)
        .with_no_client_auth();
    crypto.alpn_protocols = vec![b"h3".to_vec()];
    let config = quinn::ClientConfig::new(Arc::new(
        QuicClientConfig::try_from(crypto).map_err(io::Error::other)?,
    ));
    let mut endpoint = quinn::Endpoint::client((Ipv4Addr::LOCALHOST, 0).into())?;
    endpoint.set_default_client_config(config);
    Ok(endpoint)
}

pub(super) fn h3_proxy_config(
    listener_address: SocketAddr,
    private_key_path: &Path,
    origin_address: SocketAddr,
) -> ConfigDraft {
    let mut config = h3_only_config(listener_address, private_key_path);
    config.upstream_pools.push(UpstreamPool {
        name: "origin".into(),
        servers: Vec::new(),
        endpoints: vec![UpstreamEndpoint::Socket {
            address: origin_address,
        }],
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: None,
        passive_health: None,
        tls: Some(UpstreamTls {
            server_name: "origin.example.test".into(),
            ca_certificate_path: Some(fixture_support::fixture("ca-a.pem")),
        }),
        http_versions: HttpVersionPolicy {
            min: HttpVersion::Http3,
            max: HttpVersion::Http3,
        },
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: UpstreamConnectionReuse::default(),
        registration: None,
        canary: None,
        resilience: oxiroute_config::PoolResilience::default(),
        queue: None,
    });
    config.http_services[0].routes[0].action = HttpRouteAction::Proxy {
        upstream_pool: "origin".into(),
        policy: HttpProxyPolicy::default().into(),
    };
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
                http::Response::builder()
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
    let mut crypto = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("origin TLS protocol versions")
        .with_no_client_auth()
            .with_single_cert(certificates, private_key)
            .expect("origin TLS identity");
    crypto.alpn_protocols = vec![b"h3".to_vec()];
    crypto.max_early_data_size = 0;
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(
        QuicServerConfig::try_from(crypto).expect("origin QUIC TLS configuration"),
    ));
    config.migration(false);
    config
}

pub(super) async fn connect_h3(
    endpoint: &quinn::Endpoint,
    address: SocketAddr,
) -> quinn::Connection {
    timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(connecting) = endpoint.connect(address, "proxy.example.test")
                && let Ok(connection) = connecting.await
            {
                break connection;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("H3 connection timeout")
}

pub(super) async fn assert_h3_fixed_response(address: SocketAddr, expected: &'static [u8]) {
    let endpoint = h3_client_endpoint().expect("H3 client endpoint");
    let connection = connect_h3(&endpoint, address).await;
    let (driver, mut sender) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("H3 client connection");
    let driver = tokio::spawn(async move {
        let mut driver = driver;
        let _ = std::future::poll_fn(|context| driver.poll_close(context)).await;
    });
    let request = Request::builder()
        .method(Method::GET)
        .uri("https://proxy.example.test/")
        .body(())
        .expect("H3 request");
    let mut stream = sender.send_request(request).await.expect("send H3 request");
    stream.finish().await.expect("finish H3 request");
    let response = stream.recv_response().await.expect("H3 response");
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = stream
        .recv_data()
        .await
        .expect("H3 response body")
        .expect("H3 response data");
    let body = body.copy_to_bytes(body.remaining());
    assert_eq!(body, Bytes::from_static(expected));
    assert!(stream.recv_data().await.expect("H3 response end").is_none());
    drop(stream);
    drop(sender);
    endpoint.close(quinn::VarInt::from_u32(0), b"H3 request complete");
    driver.await.expect("H3 driver task");
}
