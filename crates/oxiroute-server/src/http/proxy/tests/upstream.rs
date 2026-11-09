use super::*;
use crate::test_ports::unused_tcp_address;

#[test]
fn rescue_only_statuses_are_not_passive_failures() {
    let policy = ProxyPolicyPlan::compile(&HttpProxyPolicy {
        retry: HttpRetryPolicy {
            response_statuses: vec![503],
            ..HttpRetryPolicy::default()
        },
        rescue: Some(oxiroute_config::HttpRescuePolicy {
            statuses: vec![404, 503],
            target: oxiroute_config::HttpRescueTarget::NextServer,
            pool: None,
        }),
        ..HttpProxyPolicy::default()
    });
    let status = |code| Error::new_up(ErrorType::HTTPStatus(code));
    assert!(is_rescue_only(&status(404), &policy));
    assert!(!is_rescue_only(&status(503), &policy));
    assert!(!is_rescue_only(&status(500), &policy));
}

#[test]
fn cache_reuse_rejects_ranges_and_unsafe_preconditions() {
    let mut headers = HeaderMap::new();
    headers.insert(RANGE, HeaderValue::from_static("bytes=0-1"));
    assert!(cache_request_bypasses_cache(&headers));
    headers.remove(RANGE);
    headers.insert(IF_NONE_MATCH, HeaderValue::from_static("\"v1\""));
    assert!(!cache_request_bypasses_cache(&headers));
    headers.insert(IF_UNMODIFIED_SINCE, HeaderValue::from_static("now"));
    assert!(cache_request_bypasses_cache(&headers));
}

#[test]
fn cache_etag_matching_uses_weak_comparison_and_wildcards() {
    assert!(cached_etag_list_matches(
        b"W/\"v1\", \"v2\"",
        Some(b"\"v1\"")
    ));
    assert!(cached_etag_list_matches(b"*", None));
    assert!(!cached_etag_list_matches(b"\"v1\"", Some(b"\"v2\"")));
}

#[tokio::test]
async fn http_falls_back_to_the_second_address_without_route_retry_budget() {
    let first = unused_tcp_address();
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 2], first.port())))
        .await
        .expect("second address listener");
    let second = listener.local_addr().expect("second address");
    let endpoint = RuntimeEndpoint::Dns {
        host: "origin.example.test".into(),
        port: second.port(),
    };
    let selector = Arc::new(
        RoundRobinPool::new_named(
            "fallback".into(),
            [endpoint.clone()],
            UpstreamAlgorithm::RoundRobin,
            false,
        )
        .expect("fallback selector"),
    );
    let plan = Arc::new(UpstreamPlan::new(Arc::clone(&selector), None));
    let policy = ProxyPolicyPlan::compile(&HttpProxyPolicy::default());
    assert_eq!(policy.max_retries, 0);
    let route = Arc::new(HttpRoutePlan {
        access: None,
        action: HttpActionPlan::Proxy(crate::http::action::ProxyActionPlan {
            pool: Arc::clone(&plan),
            rescue_pool: None,
            select: None,
            policy: Box::new(policy),
        }),
        policy: crate::http::action::RoutePolicyPlan::compile(
            oxiroute_config::HttpRoutePolicy::default(),
        ),
        route_id: "test".into(),
    });
    let runtime = RuntimeMetrics::new();
    let metrics = runtime
        .register_listener("http", "http", "127.0.0.1:8080", 100)
        .expect("listener metrics");
    let service = Arc::new(HttpServicePlan::new(
        Some(1024),
        HashMap::new(),
        Duration::from_secs(1),
        RouteTable::default(),
    ));
    let proxy = HttpReverseProxy::new(service, metrics);
    let downstream_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("downstream listener");
    let client = TcpStream::connect(
        downstream_listener
            .local_addr()
            .expect("downstream address"),
    );
    let accept = downstream_listener.accept();
    let (client, downstream) = tokio::join!(client, accept);
    let _client = client.expect("downstream client");
    let (downstream, _) = downstream.expect("downstream connection");
    let mut session = Session::new_h1(Box::new(pingora::protocols::l4::stream::Stream::from(
        downstream,
    )));
    let mut context = proxy.new_ctx();
    context.attempted_upstreams.push("0".into());
    context.pool = Some(Arc::clone(&plan));
    context.route = Some(route);
    context.selected = Some(SelectedEndpoint::with_addresses(
        selector.select().expect("fallback lease"),
        vec![first, second],
    ));

    let first_peer = proxy
        .upstream_peer(&mut session, &mut context)
        .await
        .expect("first peer");
    assert_eq!(first_peer.address().as_inet(), Some(&first));
    assert!(TcpStream::connect(first).await.is_err());
    let retry = proxy.fail_to_connect(
        &mut session,
        &first_peer,
        &mut context,
        Error::new_up(ErrorType::ConnectRefused),
    );
    assert!(retry.retry());
    assert_eq!(context.attempted_upstreams.len(), 1);

    let second_peer = proxy
        .upstream_peer(&mut session, &mut context)
        .await
        .expect("second peer");
    assert_eq!(second_peer.address().as_inet(), Some(&second));
    let connection = TcpStream::connect(second)
        .await
        .expect("second address connection");
    let (_accepted, _) = listener.accept().await.expect("second address accept");
    drop(connection);
    assert_eq!(context.attempted_upstreams.len(), 1);
}

#[tokio::test]
async fn passive_failures_eject_connect_and_stream_endpoints_before_retrying() {
    let policy = HttpProxyPolicy {
        retry: HttpRetryPolicy {
            max_retries: 1,
            ..HttpRetryPolicy::default()
        },
        ..HttpProxyPolicy::default()
    };
    let (proxy, mut session, mut context, _client) = request_preparation_fixture(
        policy,
        UpstreamConnectionReuse::Safe,
        b"GET / HTTP/1.1\r\nHost: example.test\r\n\r\n",
    )
    .await;
    let first_address: SocketAddr = "192.0.2.1:3000".parse().expect("first endpoint");
    let second_address: SocketAddr = "192.0.2.2:3000".parse().expect("second endpoint");
    let first = RuntimeEndpoint::Socket {
        address: first_address,
    };
    let second = RuntimeEndpoint::Socket {
        address: second_address,
    };
    let selector = Arc::new(
        RoundRobinPool::from_endpoints_with_policy(
            [first.clone(), second.clone()],
            UpstreamAlgorithm::RoundRobin,
            PassiveFailurePolicy::new(1, Duration::from_mins(1), Duration::from_mins(1)),
        )
        .expect("passive selector"),
    );
    let plan = Arc::new(UpstreamPlan::with_policy(
        selector.clone(),
        None,
        None,
        None,
        UpstreamConnectionReuse::Safe,
    ));
    context.pool = Some(plan);
    context.connection_retryable = true;

    let first_peer = proxy
        .upstream_peer(&mut session, &mut context)
        .await
        .expect("first peer");
    assert_eq!(first_peer.address().as_inet(), Some(&first_address));
    let retry = proxy.fail_to_connect(
        &mut session,
        &first_peer,
        &mut context,
        Error::new_up(ErrorType::ConnectRefused),
    );
    assert!(retry.retry());
    assert!(selector.health_snapshot().endpoints[0].passive_ejected);

    let second_peer = proxy
        .upstream_peer(&mut session, &mut context)
        .await
        .expect("second peer after connect failure");
    assert_eq!(second_peer.address().as_inet(), Some(&second_address));
    let terminal = proxy.error_while_proxy(
        &second_peer,
        &mut session,
        Error::new_up(ErrorType::ReadTimedout),
        &mut context,
        false,
    );
    assert!(!terminal.retry());
    assert!(selector.health_snapshot().endpoints[1].passive_ejected);
    assert_eq!(selector.health_snapshot().available_endpoints, 0);
    assert!(selector.select().is_none());
}
