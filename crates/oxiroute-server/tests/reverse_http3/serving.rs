#[allow(clippy::wildcard_imports)]
use super::*;

#[tokio::test]
#[allow(clippy::too_many_lines)]
pub(super) async fn daemon_accepts_reverse_h3_and_reuses_the_http_service_pool() {
    let origin_endpoint =
        quinn::Endpoint::server(origin_server_config(), (Ipv4Addr::LOCALHOST, 0).into())
            .expect("origin endpoint");
    let origin_address = origin_endpoint.local_addr().expect("origin address");
    let origin_task = tokio::spawn(async move {
        let incoming = origin_endpoint.accept().await.expect("origin accept");
        let connection = incoming.await.expect("origin QUIC connection");
        let handshake = connection
            .handshake_data()
            .expect("origin handshake data")
            .downcast::<HandshakeData>()
            .expect("origin rustls handshake data");
        assert_eq!(handshake.protocol.as_deref(), Some(H3_ALPN));
        let mut h3: Connection<_, Bytes> = h3::server::builder()
            .build(h3_quinn::Connection::new(connection))
            .await
            .expect("origin H3 connection");
        let resolver = h3
            .accept()
            .await
            .expect("origin H3 accept")
            .expect("origin H3 request");
        let (request, mut stream) = resolver.resolve_request().await.expect("origin request");
        assert_eq!(request.method(), Method::GET);
        assert_eq!(request.uri().path(), "/h3");
        assert!(
            stream
                .recv_data()
                .await
                .expect("origin request body")
                .is_none()
        );
        stream
            .send_response(
                Response::builder()
                    .status(StatusCode::OK)
                    .header("content-length", "4")
                    .body(())
                    .expect("origin response"),
            )
            .await
            .expect("origin response headers");
        stream
            .send_data(Bytes::from_static(b"pong"))
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

    let listener_address = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("reserve UDP address")
        .local_addr()
        .expect("reserved UDP address");
    let key = fixture_support::private_key_fixture("proxy-a-key.pem");
    let mut config = support::empty_config();
    config.certificates.push(Certificate {
        name: "downstream".into(),
        dns_names: vec![support::PROXY_SERVER_NAME.into()],
        source: CertificateSource::Files {
            certificate_chain_path: fixture_support::fixture("proxy-a.pem"),
            private_key_path: key.path().to_path_buf(),
        },
    });
    config.tls_profiles.push(TlsProfile {
        name: "downstream".into(),
        certificates: vec!["downstream".into()],
        default_certificate: "downstream".into(),
        min_version: TlsVersion::Tls13,
        alpn: vec![AlpnProtocol::H3],
        policy: oxiroute_config::TlsPolicy::default(),
    });
    config.upstream_pools.push(UpstreamPool {
        name: "origin".into(),
        servers: Vec::new(),
        endpoints: vec![support::socket_endpoint(origin_address)],
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: None,
        passive_health: None,
        tls: Some(UpstreamTls {
            server_name: support::ORIGIN_SERVER_NAME.into(),
            ca_certificate_path: Some(fixture_support::fixture("ca-a.pem")),
        }),
        http_versions: HttpVersionPolicy {
            min: HttpVersion::Http3,
            max: HttpVersion::Http3,
        },
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
        registration: None,
        canary: None,
        resilience: oxiroute_config::PoolResilience::default(),
        queue: None,
    });
    config.http_services.push(HttpService {
        drain_deadline_ms: None,
        name: "web".into(),
        routes: vec![HttpRoute {
            host: None,
            path: HttpPathSelector::SegmentPrefix { value: "/".into() },
            methods: Vec::new(),
            access_policy: None,
            policy: HttpRoutePolicy {
                max_request_body_bytes: Some(64 * 1024),
                request_buffering: true,
                ..HttpRoutePolicy::default()
            },
            action: HttpRouteAction::Proxy {
                upstream_pool: "origin".into(),
                policy: HttpProxyPolicy::default().into(),
            },
        }],
        automatic_response_headers: true,
        upstream_io_timeout_ms: 5_000,
        max_request_body_bytes: Some(64 * 1024),
        gzip: None,
        access_log: None,
    });
    config.listeners.push(Listener {
        name: "reverse".into(),
        bind: ListenerBind::Udp {
            address: listener_address,
        },
        protocol: Protocol::Http3,
        service: Some("web".into()),
        tls_profile: Some("downstream".into()),
        proxy_protocol: None,
        max_connections: Some(8),
        downstream_timeouts: DownstreamTimeoutPolicy::default(),
    });

    let config = config.validate().expect("valid reverse H3 configuration");
    let server = process_support::ServerProcess::start(&config, None);
    let endpoint = client_endpoint().expect("H3 client endpoint");
    let connection = timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(connecting) = endpoint.connect(listener_address, support::PROXY_SERVER_NAME)
                && let Ok(connection) = connecting.await
            {
                break connection;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("H3 daemon connection timeout");
    let (driver, mut sender) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("H3 client connection");
    let driver = drive_client(driver);
    let request = Request::builder()
        .method(Method::GET)
        .uri("http://example.test/h3")
        .body(())
        .expect("H3 reverse request");
    let mut stream = sender.send_request(request).await.expect("send H3 request");
    stream.finish().await.expect("finish H3 request");
    let response = stream.recv_response().await.expect("H3 response");
    let response_body = recv_chunk(&mut stream).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "response body: {response_body:?}"
    );
    assert_eq!(response_body.as_ref(), b"pong");
    assert_eq!(
        stream
            .recv_trailers()
            .await
            .expect("H3 response trailers")
            .expect("origin response trailers")
            .get("x-origin-trailer"),
        Some(&HeaderValue::from_static("complete"))
    );

    drop(stream);
    drop(sender);
    endpoint.close(quinn::VarInt::from_u32(0), b"test complete");
    driver.await.expect("H3 driver task");
    server.shutdown_gracefully();
    origin_task.await.expect("origin task");

    let released = std::net::UdpSocket::bind(listener_address).expect("UDP listener release");
    drop(released);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
pub(super) async fn reverse_h3_cache_serves_a_second_get_without_origin_contact() {
    let origin_endpoint =
        quinn::Endpoint::server(origin_server_config(), (Ipv4Addr::LOCALHOST, 0).into())
            .expect("cache origin endpoint");
    let origin_address = origin_endpoint.local_addr().expect("cache origin address");
    let second_origin_endpoint = origin_endpoint.clone();
    let origin_task = tokio::spawn(async move {
        let incoming = origin_endpoint.accept().await.expect("cache origin accept");
        let connection = incoming.await.expect("cache origin connection");
        let mut h3: Connection<_, Bytes> = h3::server::builder()
            .build(h3_quinn::Connection::new(connection))
            .await
            .expect("cache origin H3 connection");
        let resolver = h3
            .accept()
            .await
            .expect("cache origin H3 accept")
            .expect("cache origin request");
        let (request, mut stream) = resolver
            .resolve_request()
            .await
            .expect("cache origin request");
        assert_eq!(request.method(), Method::GET);
        assert_eq!(request.uri().path(), "/cached");
        assert!(
            stream
                .recv_data()
                .await
                .expect("cache origin request body")
                .is_none()
        );
        stream
            .send_response(
                Response::builder()
                    .status(StatusCode::OK)
                    .header("cache-control", "public, max-age=60")
                    .header("content-length", "6")
                    .body(())
                    .expect("cache origin response"),
            )
            .await
            .expect("cache origin response headers");
        stream
            .send_data(Bytes::from_static(b"cached"))
            .await
            .expect("cache origin response body");
        stream.finish().await.expect("cache origin response finish");
        assert!(
            timeout(Duration::from_millis(300), second_origin_endpoint.accept())
                .await
                .is_err(),
            "reverse H3 cache hit contacted the origin"
        );
    });

    let listener_address = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("reserve cache UDP address")
        .local_addr()
        .expect("reserved cache UDP address");
    let key = fixture_support::private_key_fixture("proxy-a-key.pem");
    let mut config = support::empty_config();
    config
        .cache_stores
        .push(CacheStore::memory("memory", 1024 * 1024));
    config.certificates.push(Certificate {
        name: "downstream".into(),
        dns_names: vec![support::PROXY_SERVER_NAME.into()],
        source: CertificateSource::Files {
            certificate_chain_path: fixture_support::fixture("proxy-a.pem"),
            private_key_path: key.path().to_path_buf(),
        },
    });
    config.tls_profiles.push(TlsProfile {
        name: "downstream".into(),
        certificates: vec!["downstream".into()],
        default_certificate: "downstream".into(),
        min_version: TlsVersion::Tls13,
        alpn: vec![AlpnProtocol::H3],
        policy: oxiroute_config::TlsPolicy::default(),
    });
    config.upstream_pools.push(UpstreamPool {
        name: "origin".into(),
        servers: Vec::new(),
        endpoints: vec![support::socket_endpoint(origin_address)],
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: None,
        passive_health: None,
        tls: Some(UpstreamTls {
            server_name: support::ORIGIN_SERVER_NAME.into(),
            ca_certificate_path: Some(fixture_support::fixture("ca-a.pem")),
        }),
        http_versions: HttpVersionPolicy {
            min: HttpVersion::Http3,
            max: HttpVersion::Http3,
        },
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: oxiroute_config::UpstreamConnectionReuse::default(),
        registration: None,
        canary: None,
        resilience: oxiroute_config::PoolResilience::default(),
        queue: None,
    });
    let proxy_policy = HttpProxyPolicy {
        cache: Some(Box::new(oxiroute_config::HttpCachePolicy {
            store: "memory".into(),
            methods: vec!["GET".into(), "HEAD".into()],
            key_components: vec![
                CacheKeyComponent::Scheme,
                CacheKeyComponent::NormalizedHost,
                CacheKeyComponent::PathAndQuery,
            ],
            use_origin_cache_control: true,
            default_ttl_ms: 60_000,
            status_ttls: Vec::new(),
            grace_ms: 30_000,
            keep_ms: 300_000,
            revalidate: true,
            collapsed_forwarding: true,
            stale_on: Vec::new(),
            bypass_request: Vec::new(),
            no_store_request: Vec::new(),
            no_store_response: Vec::new(),
            set_cookie_policy: oxiroute_config::CacheSetCookiePolicy::default(),
            authorization_policy: oxiroute_config::CacheAuthorizationPolicy::default(),
            vary_policy: oxiroute_config::CacheVaryPolicy::default(),
            surrogate_tags: None,
            purge_authorization: None,
        })),
        ..HttpProxyPolicy::default()
    };
    config.http_services.push(HttpService {
        drain_deadline_ms: None,
        name: "web".into(),
        routes: vec![HttpRoute {
            host: None,
            path: HttpPathSelector::SegmentPrefix { value: "/".into() },
            methods: Vec::new(),
            access_policy: None,
            policy: HttpRoutePolicy {
                max_request_body_bytes: Some(64 * 1024),
                request_buffering: true,
                ..HttpRoutePolicy::default()
            },
            action: HttpRouteAction::Proxy {
                upstream_pool: "origin".into(),
                policy: proxy_policy.into(),
            },
        }],
        automatic_response_headers: true,
        upstream_io_timeout_ms: 5_000,
        max_request_body_bytes: Some(64 * 1024),
        gzip: None,
        access_log: None,
    });
    config.listeners.push(Listener {
        name: "reverse".into(),
        bind: ListenerBind::Udp {
            address: listener_address,
        },
        protocol: Protocol::Http3,
        service: Some("web".into()),
        tls_profile: Some("downstream".into()),
        proxy_protocol: None,
        max_connections: Some(8),
        downstream_timeouts: DownstreamTimeoutPolicy::default(),
    });

    let config = config
        .validate()
        .expect("valid cached reverse H3 configuration");
    let server = process_support::ServerProcess::start(&config, None);
    let endpoint = client_endpoint().expect("cache H3 client endpoint");
    let connection = timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(connecting) = endpoint.connect(listener_address, support::PROXY_SERVER_NAME)
                && let Ok(connection) = connecting.await
            {
                break connection;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cache H3 daemon connection timeout");
    let (driver, mut sender) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("cache H3 client connection");
    let driver = drive_client(driver);
    for _ in 0..2 {
        let request = Request::builder()
            .method(Method::GET)
            .uri("https://example.test/cached")
            .body(())
            .expect("cache H3 request");
        let mut stream = sender
            .send_request(request)
            .await
            .expect("send cache H3 request");
        stream.finish().await.expect("finish cache H3 request");
        let response = stream.recv_response().await.expect("cache H3 response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(recv_chunk(&mut stream).await.as_ref(), b"cached");
        assert!(
            stream
                .recv_data()
                .await
                .expect("cache H3 response finish")
                .is_none()
        );
    }

    origin_task.await.expect("cache origin task");
    drop(sender);
    endpoint.close(quinn::VarInt::from_u32(0), b"test complete");
    driver.await.expect("cache H3 driver task");
    server.shutdown_gracefully();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
pub(super) async fn reverse_h3_characterizes_http_policy_parity_and_early_origin_contact() {
    let origin_endpoint =
        quinn::Endpoint::server(origin_server_config(), (Ipv4Addr::LOCALHOST, 0).into())
            .expect("policy origin endpoint");
    let origin_address = origin_endpoint.local_addr().expect("policy origin address");
    let origin_task = tokio::spawn(async move {
        let incoming = origin_endpoint
            .accept()
            .await
            .expect("policy origin accept");
        let connection = incoming.await.expect("policy origin connection");
        let mut h3: Connection<_, Bytes> = h3::server::builder()
            .build(h3_quinn::Connection::new(connection))
            .await
            .expect("policy origin H3 connection");
        let resolver = h3
            .accept()
            .await
            .expect("policy origin H3 accept")
            .expect("policy origin request");
        let (request, mut stream) = resolver.resolve_request().await.expect("policy request");
        assert_eq!(request.uri().path(), "/proxy");
        for (name, expected) in [
            ("x-literal", "literal"),
            ("x-forwarded-for", "trusted, 127.0.0.1"),
            ("x-joined", "one, two"),
            ("x-scheme", "https"),
        ] {
            assert_eq!(request.headers()[name], expected, "{name}");
        }
        assert!(
            stream
                .recv_data()
                .await
                .expect("policy request body")
                .is_none()
        );
        let response = Response::builder()
            .status(StatusCode::OK)
            .header("content-length", "2")
            .header("x-set", "upstream")
            .header("x-add", "upstream")
            .header("x-remove", "stale")
            .header(
                "set-cookie",
                "sid=1; Path=/internal; Secure; SameSite=Strict",
            )
            .header("set-cookie", "other=2; Path=/internal")
            .body(())
            .expect("policy origin response");
        stream
            .send_response(response)
            .await
            .expect("policy response headers");
        stream
            .send_data(Bytes::from_static(b"ok"))
            .await
            .expect("policy response body");
        stream.finish().await.expect("policy response finish");

        if let Ok(Ok(Some(_))) = timeout(Duration::from_millis(300), h3.accept()).await {
            panic!("redirect or bounded policy failure contacted the H3 origin");
        }
    });

    let listener_address = reserve_udp_address();
    let key = fixture_support::private_key_fixture("proxy-a-key.pem");
    let policy = HttpProxyPolicy {
        request_headers: vec![
            HttpRequestHeaderMutation::Set {
                name: "x-literal".into(),
                value: HttpRequestHeaderValue::Literal {
                    value: "literal".into(),
                },
            },
            HttpRequestHeaderMutation::Set {
                name: "x-forwarded-for".into(),
                value: HttpRequestHeaderValue::AppendedXForwardedFor {
                    max_bytes: 128,
                    except_source_cidrs: Vec::new(),
                },
            },
            HttpRequestHeaderMutation::Set {
                name: "x-joined".into(),
                value: HttpRequestHeaderValue::IncomingHeader {
                    name: "x-source".into(),
                    max_bytes: 8,
                },
            },
            HttpRequestHeaderMutation::Set {
                name: "x-scheme".into(),
                value: HttpRequestHeaderValue::DownstreamScheme,
            },
        ],
        response_headers: vec![
            HttpResponseHeaderMutation::Set {
                name: "x-set".into(),
                value: "policy".into(),
                always: true,
            },
            HttpResponseHeaderMutation::Add {
                name: "x-add".into(),
                value: "policy".into(),
                always: false,
            },
            HttpResponseHeaderMutation::Remove {
                name: "x-remove".into(),
            },
        ],
        response_cookie_path_rewrites: vec![HttpCookiePathRewrite {
            from: "/internal".into(),
            to: "/".into(),
        }],
        response_cookie_attributes: vec![HttpCookieAttributePolicy {
            name: "sid".into(),
            secure: Some(false),
            http_only: Some(true),
            same_site: Some(HttpSameSite::Lax),
        }],
        ..HttpProxyPolicy::default()
    };
    let service = HttpService {
        drain_deadline_ms: None,
        name: "web".into(),
        routes: vec![
            HttpRoute {
                host: None,
                path: HttpPathSelector::Exact {
                    value: "/proxy".into(),
                },
                methods: Vec::new(),
                access_policy: None,
                policy: HttpRoutePolicy {
                    max_request_body_bytes: Some(64 * 1024),
                    request_buffering: true,
                    ..HttpRoutePolicy::default()
                },
                action: HttpRouteAction::Proxy {
                    upstream_pool: "origin".into(),
                    policy: policy.into(),
                },
            },
            HttpRoute {
                host: None,
                path: HttpPathSelector::Exact {
                    value: "/redirect".into(),
                },
                methods: Vec::new(),
                access_policy: None,
                policy: HttpRoutePolicy {
                    max_request_body_bytes: Some(64 * 1024),
                    request_buffering: true,
                    ..HttpRoutePolicy::default()
                },
                action: HttpRouteAction::Redirect {
                    status: 308,
                    location: HttpRedirectLocation::RequestTemplate {
                        value: "$scheme://$host$request_uri".into(),
                        nginx_host_fallback: None,
                    },
                    headers: Vec::new(),
                },
            },
        ],
        automatic_response_headers: true,
        upstream_io_timeout_ms: 5_000,
        max_request_body_bytes: Some(64 * 1024),
        gzip: None,
        access_log: None,
    };
    let config = reverse_config(
        listener_address,
        key.path(),
        service,
        vec![h3_pool(origin_address)],
        Some(8),
    );
    let config = config.validate().expect("valid reverse H3 policy config");
    let server = process_support::ServerProcess::start(&config, None);
    let endpoint = client_endpoint().expect("policy H3 client endpoint");
    let connection = connect_h3(&endpoint, listener_address).await;
    let (driver, mut sender) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .expect("policy H3 client connection");
    let driver = drive_client(driver);

    let mut request = Request::builder()
        .uri("https://Client.Example.:8443/proxy")
        .header("x-forwarded-for", "trusted")
        .header("x-source", "one")
        .body(())
        .expect("policy H3 request");
    request
        .headers_mut()
        .append("x-source", HeaderValue::from_static("two"));
    let mut stream = sender
        .send_request(request)
        .await
        .expect("send policy request");
    stream.finish().await.expect("finish policy request");
    let response = stream.recv_response().await.expect("policy response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-set"], "policy");
    assert_eq!(
        response
            .headers()
            .get_all("x-add")
            .iter()
            .map(HeaderValue::as_bytes)
            .collect::<Vec<_>>(),
        [b"upstream".as_slice(), b"policy".as_slice()]
    );
    assert!(!response.headers().contains_key("x-remove"));
    assert_eq!(
        response
            .headers()
            .get_all("set-cookie")
            .iter()
            .map(HeaderValue::as_bytes)
            .collect::<Vec<_>>(),
        [
            b"sid=1; Path=/; SameSite=Lax; HttpOnly".as_slice(),
            b"other=2; Path=/".as_slice(),
        ]
    );
    assert_eq!(recv_body(&mut stream).await.as_ref(), b"ok");

    let mut redirect = sender
        .send_request(
            Request::builder()
                .uri("https://Actions.Example.:8443/redirect?next=1")
                .body(())
                .unwrap(),
        )
        .await
        .expect("send redirect request");
    redirect.finish().await.expect("finish redirect request");
    let response = redirect.recv_response().await.expect("redirect response");
    assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
    assert_eq!(
        response.headers()[http::header::LOCATION],
        "https://actions.example/redirect?next=1"
    );
    assert!(recv_body(&mut redirect).await.is_empty());

    let mut oversized = Request::builder()
        .uri("https://client.example/proxy")
        .header(http::header::CONTENT_LENGTH, "1")
        .header("x-source", "one")
        .body(())
        .unwrap();
    oversized
        .headers_mut()
        .append("x-source", HeaderValue::from_static("four"));
    let mut rejected = sender
        .send_request(oversized)
        .await
        .expect("send bounded policy request");
    assert_eq!(
        rejected
            .recv_response()
            .await
            .expect("bounded policy response")
            .status(),
        StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE
    );
    let _ = recv_body(&mut rejected).await;

    origin_task.await.expect("policy origin task");
    drop(stream);
    drop(redirect);
    drop(rejected);
    drop(sender);
    endpoint.close(quinn::VarInt::from_u32(0), b"test complete");
    driver.await.expect("policy H3 driver task");
    server.shutdown_gracefully();
}
