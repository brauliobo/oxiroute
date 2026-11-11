//! The route policy of a proxy route applies to an HTTP/3 request the way it does to the other protocols.
#[allow(clippy::wildcard_imports)]
use super::*;

fn set(name: &str, value: HttpRequestHeaderValue) -> HttpRequestHeaderMutation {
    HttpRequestHeaderMutation::Set {
        name: name.into(),
        value,
    }
}

pub(super) fn rewrite() -> oxiroute_config::HttpProxyPathRewrite {
    oxiroute_config::HttpProxyPathRewrite {
        from: "/api".into(),
        to: "/v2".into(),
    }
}

pub(super) fn forwarding_policy() -> HttpProxyPolicy {
    HttpProxyPolicy {
        request_headers: vec![
            set(
                "x-forwarded-for",
                HttpRequestHeaderValue::AppendedXForwardedFor {
                    max_bytes: 256,
                    except_source_cidrs: Vec::new(),
                },
            ),
            set(
                "x-forwarded-proto",
                HttpRequestHeaderValue::DownstreamScheme,
            ),
            set("x-real-ip", HttpRequestHeaderValue::ClientIp),
            set("x-selected", HttpRequestHeaderValue::SelectedUpstreamHost),
            HttpRequestHeaderMutation::Remove {
                name: "x-secret".into(),
            },
        ],
        ..HttpProxyPolicy::default()
    }
}

#[tokio::test]
pub(super) async fn h3_requests_reach_an_h1_origin_with_the_forwarded_headers_and_path_of_their_route()
 {
    let (origin, address) = ScriptedOrigin::tcp(|_| ok(b"seen")).await;
    let listener = reserve_udp_address();
    let hosted = |host: oxiroute_config::HttpUpstreamHost| HttpProxyPolicy {
        upstream_host: host,
        ..forwarding_policy()
    };
    let routes = vec![
        route(
            "/api",
            "origin",
            HttpProxyPolicy {
                upstream_path_rewrite: Some(rewrite()),
                ..hosted(oxiroute_config::HttpUpstreamHost::Literal {
                    value: "origin.internal".into(),
                })
            },
        ),
        route(
            "/endpoint",
            "origin",
            hosted(oxiroute_config::HttpUpstreamHost::Endpoint {
                unix_fallback: None,
            }),
        ),
        route("/", "origin", forwarding_policy()),
    ];
    let config = config(
        listener,
        service(routes),
        vec![tcp_pool("origin", &[address])],
    );
    let mut daemon = Daemon::start(listener, &config).await;
    let headers = [
        ("x-secret", "s3"),
        ("x-forwarded-for", "192.0.2.7"),
        ("x-custom", "kept"),
    ];

    assert_eq!(
        daemon
            .send("GET", "/api/items?q=1&r=%20", &headers, b"")
            .await
            .status,
        200
    );
    assert_eq!(
        daemon
            .send("POST", "/endpoint/post", &headers, b"abc")
            .await
            .status,
        200
    );
    assert_eq!(
        daemon.send("GET", "/plain", &headers, b"").await.status,
        200
    );

    let requests = origin.requests();
    let host = format!("127.0.0.1:{}", address.port());
    assert_eq!(requests[0].target, "/v2/items?q=1&r=%20");
    assert_eq!(requests[0].header("host"), Some("origin.internal"));
    assert_eq!(requests[1].header("host"), Some(host.as_str()));
    assert_eq!(requests[1].header("content-length"), Some("3"));
    assert_eq!(requests[2].header("host"), Some(AUTHORITY));
    for request in &requests {
        assert_eq!(
            request.header("x-forwarded-for"),
            Some("192.0.2.7, 127.0.0.1")
        );
        assert_eq!(request.header("x-forwarded-proto"), Some("https"));
        assert_eq!(request.header("x-real-ip"), Some("127.0.0.1"));
        assert_eq!(request.header("x-custom"), Some("kept"));
        assert_eq!(request.header("x-secret"), None);
        assert_eq!(request.header("te"), None);
    }
    assert_eq!(requests[0].header("x-selected"), Some("origin.internal"));
    assert_eq!(requests[1].header("x-selected"), Some(host.as_str()));
    assert_eq!(requests[2].target, "/plain");
    daemon.finish().await;
}

#[tokio::test]
pub(super) async fn h3_responses_from_an_h1_origin_get_the_response_policy_of_their_route() {
    let (_origin, address) = ScriptedOrigin::tcp(|_| {
        Reply::Send(response(
            "200 OK",
            &[
                ("X-Origin", "internal"),
                ("Keep-Alive", "timeout=5"),
                ("Connection", "x-drop"),
                ("X-Drop", "1"),
                ("Set-Cookie", "session=1; Path=/app"),
            ],
            b"policy",
        ))
    })
    .await;
    let policy = HttpProxyPolicy {
        response_headers: vec![
            oxiroute_config::HttpResponseHeaderMutation::Set {
                name: "x-added".into(),
                value: "yes".into(),
                always: true,
            },
            oxiroute_config::HttpResponseHeaderMutation::Remove {
                name: "x-origin".into(),
            },
        ],
        response_cookie_path_rewrites: vec![HttpCookiePathRewrite {
            from: "/app".into(),
            to: "/".into(),
        }],
        ..HttpProxyPolicy::default()
    };
    let mut daemon = daemon_for(&[address], policy).await;
    let answer = daemon.get("/policy").await;
    assert_eq!((answer.status, answer.text().as_str()), (200, "policy"));
    assert_eq!(answer.header("x-added"), Some("yes"));
    for dropped in ["x-origin", "x-drop", "keep-alive", "connection"] {
        assert!(answer.header(dropped).is_none(), "{dropped}");
    }
    assert_eq!(answer.header("set-cookie"), Some("session=1; Path=/"));
    daemon.finish().await;
}

fn caching_policy() -> HttpProxyPolicy {
    let mut policy = HttpProxyPolicy::default();
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
    }));
    policy
}

#[tokio::test]
pub(super) async fn h3_cache_serves_a_second_get_of_an_h1_origin_without_origin_contact() {
    let (origin, address) = ScriptedOrigin::tcp(|_| {
        Reply::Send(response(
            "200 OK",
            &[("Cache-Control", "public, max-age=60")],
            b"cached",
        ))
    })
    .await;
    let listener = reserve_udp_address();
    let mut config = draft(
        listener,
        service(vec![route("/", "origin", caching_policy())]),
        vec![tcp_pool("origin", &[address])],
    );
    config
        .cache_stores
        .push(CacheStore::memory("memory", 1024 * 1024));
    let config = config
        .validate()
        .expect("cached reverse HTTP/3 configuration");
    let mut daemon = Daemon::start(listener, &config).await;
    let first = daemon.get("/cached").await;
    let second = daemon.get("/cached").await;
    assert_eq!((first.status, first.text()), (200, "cached".into()));
    assert_eq!((second.status, second.text()), (200, "cached".into()));
    assert_eq!(origin.requests().len(), 1);
    daemon.finish().await;
}
