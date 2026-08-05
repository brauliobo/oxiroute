use std::{
    net::SocketAddr,
    sync::{Arc, Barrier},
    thread,
};

use http::{Method, Uri, uri::Authority};
use oxiroute_config::{HttpHostSelector, HttpPathSelector, UpstreamAlgorithm};
use oxiroute_server::{PoolError, RoundRobinPool, Route, RouteError, RouteTable, RuntimeEndpoint};

#[test]
fn host_precedence_is_exact_authority_then_normalized_exact_wildcard_and_catch_all() {
    let table = RouteTable::new(vec![
        route(None, "/api/v1", None, "catch-all"),
        route(Some("*.example.com"), "/api", None, "wildcard"),
        route(Some("api.example.com"), "/", None, "exact"),
        exact_authority_route("api.example.com:8443", "/", None, "authority"),
    ]);

    assert_eq!(
        selected_pool(
            &table,
            Some("api.example.com:8443"),
            "/api/v1/users",
            &Method::GET,
        ),
        Some("authority")
    );
    assert_eq!(
        selected_pool(
            &table,
            Some("API.EXAMPLE.COM:443"),
            "/api/v1/users",
            &Method::GET,
        ),
        Some("exact")
    );
    assert_eq!(
        selected_pool(
            &table,
            Some("edge.example.com"),
            "/api/v1/users",
            &Method::GET,
        ),
        Some("wildcard")
    );
    assert_eq!(
        selected_pool(
            &table,
            Some("unrelated.test"),
            "/api/v1/users",
            &Method::GET,
        ),
        Some("catch-all")
    );
}

#[test]
fn ascii_case_insensitive_exact_authority_matches_case_but_not_an_added_port() {
    let table = RouteTable::new(vec![
        route(None, "/", None, "fallback"),
        Route::new(
            Some(HttpHostSelector::AsciiCaseInsensitiveExactAuthority {
                value: "ollama.yellowmaverick.com".into(),
            }),
            HttpPathSelector::RawPrefix { value: "/".into() },
            None,
            "ollama",
        )
        .expect("case-insensitive exact authority"),
    ]);

    assert_eq!(
        selected_pool(&table, Some("OLLAMA.YellowMaverick.COM"), "/", &Method::GET,),
        Some("ollama")
    );
    assert_eq!(
        selected_pool(
            &table,
            Some("ollama.yellowmaverick.com:80"),
            "/",
            &Method::GET,
        ),
        Some("fallback")
    );
}

#[test]
fn longest_path_prefix_wins_within_a_host_class() {
    let table = RouteTable::new(vec![
        route(Some("api.example.com"), "/", None, "root"),
        route(Some("api.example.com"), "/api", None, "api"),
        route(Some("api.example.com"), "/api/v1", None, "v1"),
    ]);

    assert_eq!(
        selected_pool(
            &table,
            Some("api.example.com"),
            "/api/v1/users",
            &Method::GET,
        ),
        Some("v1")
    );
}

#[test]
fn path_precedence_is_exact_then_segment_prefix_then_raw_prefix() {
    let table = RouteTable::new(vec![
        route_with_path(
            None,
            HttpPathSelector::RawPrefix {
                value: "/api".into(),
            },
            None,
            "raw",
        ),
        route_with_path(
            None,
            HttpPathSelector::SegmentPrefix {
                value: "/api".into(),
            },
            None,
            "segment",
        ),
        route_with_path(
            None,
            HttpPathSelector::Exact {
                value: "/api".into(),
            },
            None,
            "exact",
        ),
    ]);

    assert_eq!(
        selected_pool(&table, None, "/api", &Method::GET),
        Some("exact")
    );
    assert_eq!(
        selected_pool(&table, None, "/api/users", &Method::GET),
        Some("segment")
    );
    assert_eq!(
        selected_pool(&table, None, "/apix", &Method::GET),
        Some("raw")
    );
}

#[test]
fn ascii_case_insensitive_exact_path_beats_the_proxy_fallback() {
    let table = RouteTable::new(vec![
        route_with_path(
            None,
            HttpPathSelector::AsciiCaseInsensitiveExact {
                value: "/_infra/health".into(),
            },
            None,
            "health",
        ),
        route_with_path(
            None,
            HttpPathSelector::RawPrefix { value: "/".into() },
            None,
            "proxy",
        ),
    ]);

    assert_eq!(
        selected_pool(&table, None, "/_INFRA/Health", &Method::GET),
        Some("health")
    );
    assert_eq!(
        selected_pool(&table, None, "/_infra/health/extra", &Method::GET),
        Some("proxy")
    );
}

#[test]
fn wildcard_matches_exactly_one_host_label() {
    let table = RouteTable::new(vec![
        route(None, "/", None, "catch-all"),
        route(Some("*.example.com"), "/", None, "wildcard"),
    ]);

    assert_eq!(
        selected_pool(&table, Some("edge.example.com"), "/", &Method::GET),
        Some("wildcard")
    );
    assert_eq!(
        selected_pool(&table, Some("example.com"), "/", &Method::GET),
        Some("catch-all")
    );
    assert_eq!(
        selected_pool(&table, Some("deep.edge.example.com"), "/", &Method::GET),
        Some("catch-all")
    );
}

#[test]
fn nginx_suffix_names_match_base_and_multiple_labels_with_longest_suffix_precedence() {
    let table = RouteTable::new(vec![
        route(None, "/", None, "default"),
        Route::new(
            Some(HttpHostSelector::NginxLeadingWildcard {
                value: "example.com".into(),
            }),
            HttpPathSelector::SegmentPrefix { value: "/".into() },
            None,
            "wildcard",
        )
        .expect("nginx wildcard"),
        Route::new(
            Some(HttpHostSelector::NginxLeadingDot {
                value: "deep.example.com".into(),
            }),
            HttpPathSelector::SegmentPrefix { value: "/".into() },
            None,
            "long-dot",
        )
        .expect("nginx leading dot"),
        Route::new(
            Some(HttpHostSelector::NginxLeadingDot {
                value: "base.test".into(),
            }),
            HttpPathSelector::SegmentPrefix { value: "/".into() },
            None,
            "base-dot",
        )
        .expect("base leading dot"),
    ]);

    assert_eq!(
        selected_pool(&table, Some("example.com"), "/", &Method::GET),
        Some("default")
    );
    assert_eq!(
        selected_pool(&table, Some("edge.example.com"), "/", &Method::GET),
        Some("wildcard")
    );
    assert_eq!(
        selected_pool(&table, Some("many.labels.example.com"), "/", &Method::GET,),
        Some("wildcard")
    );
    assert_eq!(
        selected_pool(&table, Some("edge.deep.example.com"), "/", &Method::GET,),
        Some("long-dot")
    );
    assert_eq!(
        selected_pool(&table, Some("base.test"), "/", &Method::GET),
        Some("base-dot")
    );
    assert_eq!(
        selected_pool(&table, Some("a.b.base.test"), "/", &Method::GET),
        Some("base-dot")
    );
}

#[test]
fn host_matching_ignores_ascii_case_and_legal_ports() {
    let table = RouteTable::new(vec![
        route(Some("api.example.com"), "/", None, "exact"),
        route(Some("*.service.test"), "/", None, "wildcard"),
        route(Some("::1"), "/", None, "ipv6"),
    ]);

    assert_eq!(
        selected_pool(
            &table,
            Some("API.Example.COM:8443"),
            "/status",
            &Method::GET,
        ),
        Some("exact")
    );

    let absolute_uri = "http://EDGE.Service.Test:443/status?full=true"
        .parse::<Uri>()
        .expect("valid absolute URI");
    assert_eq!(
        table
            .select(None, &absolute_uri, &Method::GET)
            .map(Route::route_id),
        Some("wildcard")
    );
    assert_eq!(
        selected_pool(&table, Some("[::1]:8080"), "/status", &Method::GET),
        Some("ipv6")
    );
}

#[test]
fn path_prefixes_are_normalized_and_respect_segment_boundaries() {
    let api = route(None, "/api", None, "api");
    assert_eq!(api.path_value(), "/api");
    let table = RouteTable::new(vec![api, route(None, "/", None, "root")]);

    assert_eq!(
        selected_pool(&table, None, "/api?version=1", &Method::GET),
        Some("api")
    );
    assert_eq!(
        selected_pool(&table, None, "/api/users?active=true", &Method::GET),
        Some("api")
    );
    assert_eq!(
        selected_pool(&table, None, "/apix?version=1", &Method::GET),
        Some("root")
    );
}

#[test]
fn slash_terminated_segment_prefixes_match_descendants_without_widening() {
    let table = RouteTable::new(vec![
        route(None, "/api/", None, "api-slash"),
        route(None, "/", None, "root"),
    ]);

    assert_eq!(
        selected_pool(&table, None, "/api/users", &Method::GET),
        Some("api-slash")
    );
    assert_eq!(
        selected_pool(&table, None, "/api", &Method::GET),
        Some("root")
    );
    assert_eq!(
        selected_pool(&table, None, "/apix", &Method::GET),
        Some("root")
    );
}

#[test]
fn percent_triplet_case_is_canonical_for_matching() {
    let encoded = route(None, "/private%3Azone", None, "private");
    assert_eq!(encoded.path_value(), "/private%3Azone");
    let table = RouteTable::new(vec![encoded]);

    assert_eq!(
        selected_pool(&table, None, "/private%3Azone", &Method::GET),
        Some("private")
    );
    assert_eq!(
        selected_pool(&table, None, "/private%3azone", &Method::GET),
        Some("private")
    );
}

#[test]
fn method_sets_filter_routes_before_precedence_is_applied() {
    let table = RouteTable::new(vec![
        route(None, "/items", None, "reader"),
        route(None, "/items", Some(vec![Method::POST]), "writer"),
    ]);

    assert_eq!(
        selected_pool(&table, None, "/items", &Method::POST),
        Some("writer")
    );
    assert_eq!(
        selected_pool(&table, None, "/items", &Method::GET),
        Some("reader")
    );
}

#[test]
fn source_order_resolves_complete_ties() {
    let table = RouteTable::new(vec![
        route(Some("api.example.com"), "/items", None, "first"),
        route(Some("api.example.com"), "/items", None, "second"),
    ]);

    assert_eq!(
        selected_pool(&table, Some("api.example.com"), "/items/1", &Method::GET),
        Some("first")
    );
}

#[test]
fn invalid_route_definitions_return_typed_errors() {
    assert!(matches!(
        Route::new(
            Some(HttpHostSelector::NormalizedHost {
                value: "*example.com".into(),
            }),
            HttpPathSelector::SegmentPrefix { value: "/".into() },
            None,
            "route",
        ),
        Err(RouteError::InvalidHost(_))
    ));
    assert!(matches!(
        Route::new(
            Some(HttpHostSelector::NormalizedHost {
                value: "example.com:443".into(),
            }),
            HttpPathSelector::SegmentPrefix { value: "/".into() },
            None,
            "route",
        ),
        Err(RouteError::InvalidHost(_))
    ));
    assert!(matches!(
        Route::new(
            Some(HttpHostSelector::NormalizedHost {
                value: "*.127.0.0.1".into(),
            }),
            HttpPathSelector::SegmentPrefix { value: "/".into() },
            None,
            "route",
        ),
        Err(RouteError::InvalidHost(_))
    ));
    assert!(matches!(
        Route::new(
            None,
            HttpPathSelector::SegmentPrefix {
                value: "api".into()
            },
            None,
            "route",
        ),
        Err(RouteError::InvalidPathPrefix(_))
    ));
    assert!(matches!(
        Route::new(
            None,
            HttpPathSelector::SegmentPrefix {
                value: "/api?version=1".into(),
            },
            None,
            "route",
        ),
        Err(RouteError::InvalidPathPrefix(_))
    ));
    assert!(matches!(
        Route::new(
            None,
            HttpPathSelector::SegmentPrefix {
                value: "/api/../internal".into(),
            },
            None,
            "route",
        ),
        Err(RouteError::InvalidPathPrefix(_))
    ));
    assert!(matches!(
        Route::new(
            None,
            HttpPathSelector::SegmentPrefix {
                value: "/%61pi".into(),
            },
            None,
            "route",
        ),
        Err(RouteError::InvalidPathPrefix(_))
    ));
    assert!(matches!(
        Route::new(
            None,
            HttpPathSelector::SegmentPrefix { value: "/".into() },
            Some(Vec::new()),
            "route",
        ),
        Err(RouteError::EmptyMethodSet)
    ));
    assert!(matches!(
        Route::new(
            None,
            HttpPathSelector::SegmentPrefix { value: "/".into() },
            None,
            "",
        ),
        Err(RouteError::EmptyRouteIdentity)
    ));
}

#[test]
fn empty_round_robin_pools_return_a_typed_error() {
    assert!(matches!(
        RoundRobinPool::new(Vec::<SocketAddr>::new()),
        Err(PoolError::Empty)
    ));
}

#[test]
fn round_robin_selection_wraps_in_definition_order() {
    let endpoints = [address(3001), address(3002), address(3003)];
    let pool = RoundRobinPool::new(endpoints).expect("nonempty pool");

    let selected = (0..7).map(|_| pool.select()).collect::<Vec<_>>();
    let selected = selected
        .into_iter()
        .map(|lease| lease.map(|lease| lease.endpoint().clone()))
        .collect::<Vec<_>>();
    let endpoints = endpoints.map(RuntimeEndpoint::from);

    assert_eq!(
        selected,
        vec![
            Some(endpoints[0].clone()),
            Some(endpoints[1].clone()),
            Some(endpoints[2].clone()),
            Some(endpoints[0].clone()),
            Some(endpoints[1].clone()),
            Some(endpoints[2].clone()),
            Some(endpoints[0].clone()),
        ]
    );
}

#[test]
fn round_robin_can_exclude_endpoints_already_attempted_by_a_request() {
    let endpoints = [address(3001), address(3002), address(3003)];
    let pool = RoundRobinPool::new(endpoints).expect("nonempty pool");

    let first = pool.select_excluding(&[]).expect("first endpoint");
    let first_endpoint = first.endpoint().clone();
    let second = pool
        .select_excluding(std::slice::from_ref(&first_endpoint))
        .expect("distinct retry endpoint");

    assert_ne!(first.endpoint(), second.endpoint());
    let endpoints = endpoints.map(RuntimeEndpoint::from);
    assert!(pool.select_excluding(&endpoints).is_none());
}

#[test]
fn concurrent_round_robin_selection_distributes_every_atomic_turn() {
    const THREADS: usize = 8;
    const SELECTIONS_PER_THREAD: usize = 1_000;

    let endpoints = [address(4001), address(4002), address(4003), address(4004)];
    let pool = Arc::new(RoundRobinPool::new(endpoints).expect("nonempty pool"));
    let barrier = Arc::new(Barrier::new(THREADS));
    let handles = (0..THREADS)
        .map(|_| {
            let pool = Arc::clone(&pool);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                (0..SELECTIONS_PER_THREAD)
                    .map(|_| pool.select().map(|lease| lease.endpoint().clone()))
                    .collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();

    let selected = handles
        .into_iter()
        .flat_map(|handle| handle.join().expect("selection thread"))
        .collect::<Vec<_>>();

    for endpoint in endpoints.map(RuntimeEndpoint::from) {
        assert_eq!(
            selected
                .iter()
                .filter(|selected| selected.as_ref() == Some(&endpoint))
                .count(),
            THREADS * SELECTIONS_PER_THREAD / endpoints.len()
        );
    }
    assert_eq!(
        pool.select().map(|lease| lease.endpoint().clone()),
        Some(RuntimeEndpoint::from(endpoints[0]))
    );
}

#[test]
fn concurrent_exclusion_never_falsely_exhausts_the_pool() {
    const THREADS: usize = 8;
    const SELECTIONS_PER_THREAD: usize = 1_000;

    let excluded = address(5001);
    let available = address(5002);
    let pool = Arc::new(RoundRobinPool::new([excluded, available]).expect("nonempty pool"));
    let excluded = RuntimeEndpoint::from(excluded);
    let available = RuntimeEndpoint::from(available);
    let barrier = Arc::new(Barrier::new(THREADS));
    let handles = (0..THREADS)
        .map(|_| {
            let pool = Arc::clone(&pool);
            let barrier = Arc::clone(&barrier);
            let excluded = excluded.clone();
            let available = available.clone();
            thread::spawn(move || {
                barrier.wait();
                for _ in 0..SELECTIONS_PER_THREAD {
                    assert_eq!(
                        pool.select_excluding(std::slice::from_ref(&excluded))
                            .map(|lease| lease.endpoint().clone()),
                        Some(available.clone())
                    );
                }
            })
        })
        .collect::<Vec<_>>();

    for handle in handles {
        handle.join().expect("selection thread");
    }
}

#[test]
fn least_connections_prefers_the_smallest_lease_count_and_rotates_ties() {
    let endpoints = [address(6001), address(6002), address(6003)].map(RuntimeEndpoint::from);
    let pool =
        RoundRobinPool::from_endpoints(endpoints.clone(), UpstreamAlgorithm::LeastConnections)
            .expect("least-connections pool");

    let first = pool.select().expect("first lease");
    assert_eq!(first.endpoint(), &endpoints[0]);
    let second = pool.select().expect("second lease");
    assert_eq!(second.endpoint(), &endpoints[1]);
    drop(first);
    let third = pool.select().expect("third lease");
    assert_eq!(third.endpoint(), &endpoints[2]);
    drop(second);
    drop(third);

    let snapshot = pool.health_snapshot();
    assert_eq!(snapshot.algorithm, "least_connections");
    assert!(
        snapshot
            .endpoints
            .iter()
            .all(|endpoint| endpoint.active_connections == 0)
    );
}

#[test]
fn least_connections_excludes_attempted_endpoints_before_leasing() {
    let endpoints = [address(7001), address(7002)].map(RuntimeEndpoint::from);
    let pool =
        RoundRobinPool::from_endpoints(endpoints.clone(), UpstreamAlgorithm::LeastConnections)
            .expect("least-connections pool");

    let held = pool.select().expect("held lease");
    let retry = pool
        .select_excluding(std::slice::from_ref(held.endpoint()))
        .expect("retry lease");
    assert_eq!(retry.endpoint(), &endpoints[1]);
    assert_eq!(pool.health_snapshot().endpoints[0].active_connections, 1);

    drop(held);
    drop(retry);
    assert!(
        pool.health_snapshot()
            .endpoints
            .iter()
            .all(|endpoint| endpoint.active_connections == 0)
    );
}

fn route(
    host: Option<&str>,
    path_prefix: &str,
    methods: Option<Vec<Method>>,
    pool_id: &str,
) -> Route {
    route_with_path(
        host,
        HttpPathSelector::SegmentPrefix {
            value: path_prefix.into(),
        },
        methods,
        pool_id,
    )
}

fn route_with_path(
    host: Option<&str>,
    path: HttpPathSelector,
    methods: Option<Vec<Method>>,
    route_id: &str,
) -> Route {
    Route::new(
        host.map(|value| HttpHostSelector::NormalizedHost {
            value: value.into(),
        }),
        path,
        methods,
        route_id,
    )
    .expect("valid route")
}

fn exact_authority_route(
    authority: &str,
    path: &str,
    methods: Option<Vec<Method>>,
    route_id: &str,
) -> Route {
    Route::new(
        Some(HttpHostSelector::ExactAuthority {
            value: authority.into(),
        }),
        HttpPathSelector::SegmentPrefix { value: path.into() },
        methods,
        route_id,
    )
    .expect("valid exact-authority route")
}

fn selected_pool<'a>(
    table: &'a RouteTable,
    authority: Option<&str>,
    uri: &str,
    method: &Method,
) -> Option<&'a str> {
    let authority = authority.map(|authority| {
        authority
            .parse::<Authority>()
            .expect("valid request authority")
    });
    let uri = uri.parse::<Uri>().expect("valid request URI");

    table
        .select(authority.as_ref(), &uri, method)
        .map(Route::route_id)
}

fn address(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}
