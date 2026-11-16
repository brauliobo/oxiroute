use super::*;

fn cached_route(host: Option<&str>, path: &str, pool: &str) -> HttpRoute {
    let mut route = route(host, path, &["GET", "HEAD"], pool);
    let HttpRouteAction::Proxy { policy, .. } = &mut route.action else {
        unreachable!("cached route is a proxy route");
    };
    policy.cache = Some(Box::new(oxiroute_config::HttpCachePolicy {
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
        compression: None,
    }));
    route
}

#[tokio::test]
async fn memory_cache_reuses_get_and_head_responses() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start("cached", 1).await;
        let proxy = ProxyHarness::start_with_memory_cache(
            vec![pool("origin", &[origin.address])],
            vec![cached_route(None, "/", "origin")],
            3,
        )
        .await;

        let first = proxy
            .request("GET /item?variant=one HTTP/1.1\r\nHost: cache.test\r\n")
            .await;
        let second = proxy
            .request("GET /item?variant=one HTTP/1.1\r\nHost: cache.test\r\n")
            .await;
        let head = proxy
            .request("HEAD /item?variant=one HTTP/1.1\r\nHost: cache.test\r\n")
            .await;

        assert_origin_response(&first, "cached");
        assert_origin_response(&second, "cached");
        assert_eq!(head.status, 200, "response: {}", head.text());
        assert!(head.body.is_empty());
        assert_eq!(origin.accepted.load(Ordering::SeqCst), 1);

        let cache = proxy
            .metrics
            .snapshot()
            .expect("cache metrics snapshot")
            .listeners[0]
            .cache
            .clone()
            .expect("cache metrics");
        assert_eq!(cache.hits, 2);
        assert_eq!(cache.misses, 1);
        assert_eq!(cache.admissions, 1);

        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("memory cache reuse test timed out");
}

#[tokio::test]
async fn head_miss_warms_get_representation_with_upstream_get() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("HEAD warming origin bind");
        let address = listener.local_addr().expect("HEAD warming origin address");
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("HEAD warming accept");
            let request = read_request_head_bytes(&mut stream)
                .await
                .expect("HEAD warming request");
            assert!(request.starts_with(b"GET /warm HTTP/1.1\r\n"));
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nCache-Control: max-age=60\r\nConnection: close\r\n\r\nwarmed",
                )
                .await
                .expect("HEAD warming response");
        });
        let proxy = ProxyHarness::start_with_memory_cache(
            vec![pool("origin", &[address])],
            vec![cached_route(None, "/", "origin")],
            2,
        )
        .await;

        let head = proxy
            .request("HEAD /warm HTTP/1.1\r\nHost: cache.test\r\n")
            .await;
        let get = proxy
            .request("GET /warm HTTP/1.1\r\nHost: cache.test\r\n")
            .await;
        assert_eq!(head.status, 200);
        assert!(head.body.is_empty());
        assert_origin_response(&get, "warmed");
        let cache = proxy.metrics.snapshot().expect("metrics").listeners[0]
            .cache
            .clone()
            .expect("cache metrics");
        assert_eq!(cache.hits, 1);
        assert_eq!(cache.misses, 1);
        assert_eq!(cache.admissions, 1);

        proxy.finish().await;
        origin.await.expect("HEAD warming origin");
    })
    .await
    .expect("HEAD warming test timed out");
}

#[tokio::test]
async fn only_if_cached_miss_returns_gateway_timeout_without_origin_contact() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start("unused", 1).await;
        let proxy = ProxyHarness::start_with_memory_cache(
            vec![pool("origin", &[origin.address])],
            vec![cached_route(None, "/", "origin")],
            1,
        )
        .await;

        let response = proxy
            .request(
                "GET /uncached HTTP/1.1\r\nHost: cache.test\r\nCache-Control: only-if-cached\r\n",
            )
            .await;
        assert_eq!(response.status, 504, "response: {}", response.text());
        let cache = proxy.metrics.snapshot().expect("metrics").listeners[0]
            .cache
            .clone()
            .expect("cache metrics");
        assert_eq!(cache.hits, 0);
        assert_eq!(cache.misses, 1);
        assert_eq!(cache.admissions, 0);

        proxy.finish().await;
        origin.assert_not_contacted().await;
    })
    .await
    .expect("only-if-cached test timed out");
}

#[tokio::test]
async fn unknown_length_get_bodies_bypass_cache_admission() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start("chunked-request", 2).await;
        let proxy = ProxyHarness::start_with_memory_cache(
            vec![pool("origin", &[origin.address])],
            vec![cached_route(None, "/", "origin")],
            2,
        )
        .await;
        let request = b"GET /chunked HTTP/1.1\r\nHost: cache.test\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n7\r\npayload\r\n0\r\n\r\n";

        for _ in 0..2 {
            assert_origin_response(&proxy.request_bytes(request).await, "chunked-request");
        }
        assert_eq!(origin.accepted.load(Ordering::SeqCst), 2);

        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("unknown-length cache bypass test timed out");
}

#[tokio::test]
async fn disk_cache_survives_proxy_restart() {
    timeout(TEST_TIMEOUT, async {
        let temp = TempDir::new().expect("disk cache parent");
        let origin = Origin::start("persistent", 1).await;
        let root = temp.path().join("cache");
        let mut route = cached_route(None, "/", "origin");
        let HttpRouteAction::Proxy { policy, .. } = &mut route.action else {
            unreachable!("cached route is a proxy route");
        };
        policy.cache.as_mut().expect("cache policy").store = "disk".into();
        let store = disk_cache_store("disk", root.clone());

        let proxy = ProxyHarness::start_with_features_and_cache(
            vec![pool("origin", &[origin.address])],
            vec![route.clone()],
            Some(1024),
            true,
            100,
            0,
            false,
            1,
            None,
            None,
            DownstreamTimeoutPolicy::default(),
            vec![store.clone()],
        )
        .await;
        assert_origin_response(
            &proxy
                .request("GET /persistent HTTP/1.1\r\nHost: cache.test\r\n")
                .await,
            "persistent",
        );
        proxy.finish().await;

        let proxy = ProxyHarness::start_with_features_and_cache(
            vec![pool("origin", &[origin.address])],
            vec![route],
            Some(1024),
            true,
            100,
            0,
            false,
            1,
            None,
            None,
            DownstreamTimeoutPolicy::default(),
            vec![store],
        )
        .await;
        assert_origin_response(
            &proxy
                .request("GET /persistent HTTP/1.1\r\nHost: cache.test\r\n")
                .await,
            "persistent",
        );
        assert_eq!(origin.accepted.load(Ordering::SeqCst), 1);

        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("persistent cache restart test timed out");
}

#[tokio::test]
async fn authenticated_surrogate_tag_purge_removes_a_cached_response() {
    timeout(TEST_TIMEOUT, async {
        let temp = TempDir::new().expect("purge token parent");
        let token = "x".repeat(32);
        let token_file = write_secure_token(temp.path(), "purge.token", &token);
        let origin = Origin::start_with_surrogate_tag("tagged", 2).await;
        let mut route = cached_route(None, "/", "origin");
        route.methods = vec!["GET".into(), "HEAD".into(), "PURGE".into()];
        let HttpRouteAction::Proxy { policy, .. } = &mut route.action else {
            unreachable!("cached route is a proxy route");
        };
        let cache = policy.cache.as_mut().expect("cache policy");
        cache.surrogate_tags = Some(CacheSurrogateTags {
            response_header: "Surrogate-Key".into(),
            max_tags: 4,
            max_tag_bytes: 64,
        });
        cache.purge_authorization = Some(CachePurgeAuthorization::BearerTokenFile {
            token_file_path: token_file,
        });
        let proxy = ProxyHarness::start_with_features_and_cache(
            vec![pool("origin", &[origin.address])],
            vec![route],
            Some(1024),
            true,
            100,
            0,
            false,
            4,
            None,
            None,
            DownstreamTimeoutPolicy::default(),
            vec![memory_cache_store("memory")],
        )
        .await;

        assert_origin_response(
            &proxy
                .request("GET /tagged HTTP/1.1\r\nHost: cache.test\r\n")
                .await,
            "tagged",
        );
        let unauthorized = proxy
            .request("PURGE /tagged HTTP/1.1\r\nHost: cache.test\r\n")
            .await;
        assert_eq!(unauthorized.status, 401);
        let purge_request = format!(
            "PURGE /tagged HTTP/1.1\r\nHost: cache.test\r\nAuthorization: Bearer {token}\r\nSurrogate-Key: page\r\n"
        );
        let purged = proxy.request(&purge_request).await;
        assert_eq!(purged.status, 200, "response: {}", purged.text());
        assert_origin_response(
            &proxy
                .request("GET /tagged HTTP/1.1\r\nHost: cache.test\r\n")
                .await,
            "tagged",
        );
        assert_eq!(origin.accepted.load(Ordering::SeqCst), 2);

        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("authenticated purge test timed out");
}

#[tokio::test]
async fn cache_collapses_concurrent_misses_into_one_origin_request() {
    timeout(TEST_TIMEOUT, async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("blocked origin bind");
        let address = listener.local_addr().expect("blocked origin address");
        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_by_task = Arc::clone(&accepted);
        let (started_tx, mut started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let origin = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("blocked origin accept");
            accepted_by_task.fetch_add(1, Ordering::SeqCst);
            read_request_head(&mut stream)
                .await
                .expect("blocked origin request");
            started_tx.send(()).expect("blocked origin started");
            release_rx.await.expect("blocked origin release");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\ncached",
                )
                .await
                .expect("blocked origin response");
        });
        let proxy = ProxyHarness::start_with_memory_cache(
            vec![pool("origin", &[address])],
            vec![cached_route(None, "/", "origin")],
            2,
        )
        .await;

        let (first_response, second_response) = {
            let first = proxy.request("GET /shared HTTP/1.1\r\nHost: cache.test\r\n");
            tokio::pin!(first);
            tokio::select! {
                _ = &mut started_rx => {}
                response = &mut first => panic!(
                    "leader completed before reaching origin: {}",
                    response.text()
                ),
            }
            let second = proxy.request("GET /shared HTTP/1.1\r\nHost: cache.test\r\n");
            tokio::pin!(second);
            tokio::select! {
                () = sleep(Duration::from_millis(25)) => {}
                response = &mut second => panic!(
                    "follower completed before leader release: {}",
                    response.text()
                ),
            }
            release_tx.send(()).expect("release blocked origin");
            tokio::join!(&mut first, &mut second)
        };

        assert_origin_response(&first_response, "cached");
        assert_origin_response(&second_response, "cached");
        assert_eq!(accepted.load(Ordering::SeqCst), 1);

        proxy.finish().await;
        origin.await.expect("blocked origin task");
    })
    .await
    .expect("collapsed forwarding test timed out");
}

#[tokio::test]
async fn memory_cache_revalidates_expired_responses_with_origin_validators() {
    timeout(TEST_TIMEOUT, async {
        let origin = RevalidatingOrigin::start("fresh").await;
        let proxy = ProxyHarness::start_with_memory_cache(
            vec![pool("origin", &[origin.address])],
            vec![cached_route(None, "/", "origin")],
            2,
        )
        .await;

        let first = proxy
            .request("GET /fresh HTTP/1.1\r\nHost: cache.test\r\n")
            .await;
        let second = proxy
            .request("GET /fresh HTTP/1.1\r\nHost: cache.test\r\n")
            .await;

        assert_origin_response(&first, "fresh");
        assert_origin_response(&second, "fresh");
        assert_eq!(origin.accepted.load(Ordering::SeqCst), 2);
        assert_eq!(origin.conditional_requests.load(Ordering::SeqCst), 1);

        let cache = proxy
            .metrics
            .snapshot()
            .expect("cache metrics snapshot")
            .listeners[0]
            .cache
            .clone()
            .expect("cache metrics");
        assert_eq!(cache.hits, 1);
        assert_eq!(cache.misses, 2);
        assert_eq!(cache.admissions, 2);

        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("cache freshness test timed out");
}

#[tokio::test]
async fn cache_policy_does_not_change_uncached_routes() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start("uncached", 2).await;
        let proxy = ProxyHarness::start_with_memory_cache(
            vec![pool("origin", &[origin.address])],
            vec![route(None, "/", &[], "origin")],
            2,
        )
        .await;

        assert_origin_response(
            &proxy
                .request("GET /uncached HTTP/1.1\r\nHost: cache.test\r\n")
                .await,
            "uncached",
        );
        assert_origin_response(
            &proxy
                .request("GET /uncached HTTP/1.1\r\nHost: cache.test\r\n")
                .await,
            "uncached",
        );
        assert_eq!(origin.accepted.load(Ordering::SeqCst), 2);
        assert!(
            proxy
                .metrics
                .snapshot()
                .expect("cache metrics snapshot")
                .listeners[0]
                .cache
                .is_none()
        );

        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("uncached route test timed out");
}

#[tokio::test]
async fn no_store_origin_responses_are_not_admitted() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start_with_cache_control("no-store", 2, Some("no-store")).await;
        let proxy = ProxyHarness::start_with_memory_cache(
            vec![pool("origin", &[origin.address])],
            vec![cached_route(None, "/", "origin")],
            2,
        )
        .await;

        for _ in 0..2 {
            assert_origin_response(
                &proxy
                    .request("GET /no-store HTTP/1.1\r\nHost: cache.test\r\n")
                    .await,
                "no-store",
            );
        }
        assert_eq!(origin.accepted.load(Ordering::SeqCst), 2);
        let cache = proxy
            .metrics
            .snapshot()
            .expect("cache metrics snapshot")
            .listeners[0]
            .cache
            .clone()
            .expect("cache metrics");
        assert_eq!(cache.admissions, 0);

        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("no-store cache test timed out");
}

#[tokio::test]
async fn set_cookie_origin_responses_are_not_admitted() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start_with_set_cookie("cookie", 2).await;
        let proxy = ProxyHarness::start_with_memory_cache(
            vec![pool("origin", &[origin.address])],
            vec![cached_route(None, "/", "origin")],
            2,
        )
        .await;

        for _ in 0..2 {
            assert_origin_response(
                &proxy
                    .request("GET /cookie HTTP/1.1\r\nHost: cache.test\r\n")
                    .await,
                "cookie",
            );
        }
        assert_eq!(origin.accepted.load(Ordering::SeqCst), 2);
        let cache = proxy
            .metrics
            .snapshot()
            .expect("cache metrics snapshot")
            .listeners[0]
            .cache
            .clone()
            .expect("cache metrics");
        assert_eq!(cache.admissions, 0);

        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("set-cookie cache test timed out");
}

#[tokio::test]
async fn failed_cache_fills_do_not_poison_later_requests() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start_fail_once("recovered").await;
        let proxy = ProxyHarness::start_with_memory_cache(
            vec![pool("origin", &[origin.address])],
            vec![cached_route(None, "/", "origin")],
            3,
        )
        .await;

        let failed = proxy
            .request("GET /recovered HTTP/1.1\r\nHost: cache.test\r\n")
            .await;
        assert_eq!(failed.status, 502, "response: {}", failed.text());
        assert_origin_response(
            &proxy
                .request("GET /recovered HTTP/1.1\r\nHost: cache.test\r\n")
                .await,
            "recovered",
        );
        assert_origin_response(
            &proxy
                .request("GET /recovered HTTP/1.1\r\nHost: cache.test\r\n")
                .await,
            "recovered",
        );
        assert_eq!(origin.accepted.load(Ordering::SeqCst), 2);

        let cache = proxy
            .metrics
            .snapshot()
            .expect("cache metrics snapshot")
            .listeners[0]
            .cache
            .clone()
            .expect("cache metrics");
        assert_eq!(cache.admissions, 1);
        assert_eq!(cache.hits, 1);

        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("failed cache fill test timed out");
}

#[tokio::test]
async fn chunked_responses_are_forwarded_without_cache_admission() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start_chunked("streaming", 2).await;
        let proxy = ProxyHarness::start_with_memory_cache(
            vec![pool("origin", &[origin.address])],
            vec![cached_route(None, "/", "origin")],
            2,
        )
        .await;

        for _ in 0..2 {
            let response = proxy
                .request("GET /stream HTTP/1.1\r\nHost: cache.test\r\n")
                .await;
            assert_eq!(response.status, 200, "response: {}", response.text());
            assert_eq!(response.body(), b"streaming");
        }
        assert_eq!(origin.accepted.load(Ordering::SeqCst), 2);
        let cache = proxy
            .metrics
            .snapshot()
            .expect("cache metrics snapshot")
            .listeners[0]
            .cache
            .clone()
            .expect("cache metrics");
        assert_eq!(cache.admissions, 0);

        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("streaming cache test timed out");
}

#[tokio::test]
async fn incomplete_responses_cancel_fill_without_cache_admission() {
    timeout(TEST_TIMEOUT, async {
        let origin = Origin::start_incomplete(2).await;
        let proxy = ProxyHarness::start_with_memory_cache(
            vec![pool("origin", &[origin.address])],
            vec![cached_route(None, "/", "origin")],
            2,
        )
        .await;

        for _ in 0..2 {
            let response = proxy
                .request("GET /incomplete HTTP/1.1\r\nHost: cache.test\r\n")
                .await;
            assert_eq!(response.status, 200, "response: {}", response.text());
            assert_eq!(response.body(), b"short");
        }
        assert_eq!(origin.accepted.load(Ordering::SeqCst), 2);
        let cache = proxy.metrics.snapshot().expect("metrics").listeners[0]
            .cache
            .clone()
            .expect("cache metrics");
        assert_eq!(cache.admissions, 0);

        proxy.finish().await;
        origin.finish().await;
    })
    .await
    .expect("incomplete cache response test timed out");
}

#[tokio::test]
async fn cache_serves_stale_only_for_transport_and_server_failures() {
    for server_error in [false, true] {
        timeout(TEST_TIMEOUT, async {
            let origin = Origin::start_stale_then_failure("stale", server_error).await;
            let proxy = ProxyHarness::start_with_memory_cache(
                vec![pool("origin", &[origin.address])],
                vec![cached_route(None, "/", "origin")],
                2,
            )
            .await;

            assert_origin_response(
                &proxy
                    .request("GET /stale HTTP/1.1\r\nHost: cache.test\r\n")
                    .await,
                "stale",
            );
            assert_origin_response(
                &proxy
                    .request("GET /stale HTTP/1.1\r\nHost: cache.test\r\n")
                    .await,
                "stale",
            );
            let cache = proxy.metrics.snapshot().expect("metrics").listeners[0]
                .cache
                .clone()
                .expect("cache metrics");
            assert_eq!(cache.hits, 1);
            assert_eq!(cache.misses, 2);
            assert_eq!(cache.admissions, 1);

            proxy.finish().await;
            origin.finish().await;
        })
        .await
        .expect("stale-if-error test timed out");
    }
}
