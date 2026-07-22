use std::{
    net::SocketAddr,
    sync::{Arc, Barrier},
    thread,
};

use http::{Method, Uri, uri::Authority};
use oxiroute_server::{PoolError, RoundRobinPool, Route, RouteError, RouteTable};

#[test]
fn host_precedence_is_exact_then_wildcard_then_catch_all() {
    let table = RouteTable::new(vec![
        route(None, "/api/v1", None, "catch-all"),
        route(Some("*.example.com"), "/api", None, "wildcard"),
        route(Some("api.example.com"), "/", None, "exact"),
    ]);

    assert_eq!(
        selected_pool(
            &table,
            Some("api.example.com"),
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
            .map(Route::pool_id),
        Some("wildcard")
    );
    assert_eq!(
        selected_pool(&table, Some("[::1]:8080"), "/status", &Method::GET),
        Some("ipv6")
    );
}

#[test]
fn path_prefixes_are_normalized_and_respect_segment_boundaries() {
    let api = route(None, "/api///", None, "api");
    assert_eq!(api.path_prefix(), "/api");
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
fn percent_triplet_case_is_canonical_for_matching() {
    let encoded = route(None, "/private%3azone", None, "private");
    assert_eq!(encoded.path_prefix(), "/private%3Azone");
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
        route(None, "/items", Some(vec![Method::POST]), "writer"),
        route(None, "/items", None, "reader"),
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
        Route::new(Some("*example.com"), "/", None, "pool"),
        Err(RouteError::InvalidHost(_))
    ));
    assert!(matches!(
        Route::new(Some("example.com:443"), "/", None, "pool"),
        Err(RouteError::InvalidHost(_))
    ));
    assert!(matches!(
        Route::new(Some("*.127.0.0.1"), "/", None, "pool"),
        Err(RouteError::InvalidHost(_))
    ));
    assert!(matches!(
        Route::new(None, "api", None, "pool"),
        Err(RouteError::InvalidPathPrefix(_))
    ));
    assert!(matches!(
        Route::new(None, "/api?version=1", None, "pool"),
        Err(RouteError::InvalidPathPrefix(_))
    ));
    assert!(matches!(
        Route::new(None, "/api/../internal", None, "pool"),
        Err(RouteError::InvalidPathPrefix(_))
    ));
    assert!(matches!(
        Route::new(None, "/%61pi", None, "pool"),
        Err(RouteError::InvalidPathPrefix(_))
    ));
    assert!(matches!(
        Route::new(None, "/", Some(Vec::new()), "pool"),
        Err(RouteError::EmptyMethodSet)
    ));
    assert!(matches!(
        Route::new(None, "/", None, ""),
        Err(RouteError::EmptyPoolIdentity)
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

    assert_eq!(
        selected,
        vec![
            Some(endpoints[0]),
            Some(endpoints[1]),
            Some(endpoints[2]),
            Some(endpoints[0]),
            Some(endpoints[1]),
            Some(endpoints[2]),
            Some(endpoints[0]),
        ]
    );
}

#[test]
fn round_robin_can_exclude_endpoints_already_attempted_by_a_request() {
    let endpoints = [address(3001), address(3002), address(3003)];
    let pool = RoundRobinPool::new(endpoints).expect("nonempty pool");

    let first = pool.select_excluding(&[]).expect("first endpoint");
    let second = pool
        .select_excluding(&[first])
        .expect("distinct retry endpoint");

    assert_ne!(first, second);
    assert_eq!(pool.select_excluding(&endpoints), None);
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
                    .map(|_| pool.select())
                    .collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();

    let selected = handles
        .into_iter()
        .flat_map(|handle| handle.join().expect("selection thread"))
        .collect::<Vec<_>>();

    for endpoint in endpoints {
        assert_eq!(
            selected
                .iter()
                .filter(|selected| **selected == Some(endpoint))
                .count(),
            THREADS * SELECTIONS_PER_THREAD / endpoints.len()
        );
    }
    assert_eq!(pool.select(), Some(endpoints[0]));
}

#[test]
fn concurrent_exclusion_never_falsely_exhausts_the_pool() {
    const THREADS: usize = 8;
    const SELECTIONS_PER_THREAD: usize = 1_000;

    let excluded = address(5001);
    let available = address(5002);
    let pool = Arc::new(RoundRobinPool::new([excluded, available]).expect("nonempty pool"));
    let barrier = Arc::new(Barrier::new(THREADS));
    let handles = (0..THREADS)
        .map(|_| {
            let pool = Arc::clone(&pool);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                for _ in 0..SELECTIONS_PER_THREAD {
                    assert_eq!(pool.select_excluding(&[excluded]), Some(available));
                }
            })
        })
        .collect::<Vec<_>>();

    for handle in handles {
        handle.join().expect("selection thread");
    }
}

fn route(
    host: Option<&str>,
    path_prefix: &str,
    methods: Option<Vec<Method>>,
    pool_id: &str,
) -> Route {
    Route::new(host, path_prefix, methods, pool_id).expect("valid route")
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
        .map(Route::pool_id)
}

fn address(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}
