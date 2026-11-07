use std::{sync::Arc, time::Duration};

use oxiroute_config::{HealthStartup, PoolRegistration, UpstreamAlgorithm};
use serde_json::json;

use super::*;
use crate::{
    RoundRobinPool, RuntimeEndpoint,
    registry::{auth::Scope, lease::ManualClock, service::PoolBinding, store::Store},
    routing::RuntimeServer,
};

struct Fixture {
    _dir: tempfile::TempDir,
    clock: Arc<ManualClock>,
    service: Arc<RegistryService>,
    _pool: Arc<RoundRobinPool>,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("dir");
    let clock = Arc::new(ManualClock::new());
    let service =
        RegistryService::with_store(Store::open(dir.path()).expect("store"), clock.clone(), None);
    service.set_writer(true);
    let pool = Arc::new(
        RoundRobinPool::new_named_servers(
            "render".into(),
            [RuntimeServer {
                name: "local".into(),
                endpoint: RuntimeEndpoint::from(
                    "192.0.2.200:9000"
                        .parse::<std::net::SocketAddr>()
                        .expect("address"),
                ),
                max_connections: None,
                pinned_addresses: None,
                protected_addresses: Arc::from([]),
            }],
            UpstreamAlgorithm::RoundRobin,
            Some(HealthStartup::Healthy),
            None,
        )
        .expect("pool"),
    );
    let policy = PoolRegistration {
        allowed_cidrs: vec!["192.0.2.0/24".into()],
        require_peer_address: false,
        ..PoolRegistration::default()
    };
    service.attach(vec![PoolBinding::new(
        &pool,
        Some(policy),
        None,
        Arc::from([]),
    )]);
    Fixture {
        _dir: dir,
        clock,
        service,
        _pool: pool,
    }
}

fn call(f: &Fixture, who: &Principal, method: &str, path: &str, body: serde_json::Value) -> Reply {
    let bytes = match body {
        serde_json::Value::Null => Vec::new(),
        body => serde_json::to_vec(&body).expect("body"),
    };
    let (path, query) = path
        .split_once('?')
        .map_or((path, None), |(path, query)| (path, Some(query)));
    let route = match_route(path).unwrap_or_else(|| panic!("route {path}"));
    handle(
        &f.service,
        route,
        &Request {
            method,
            query,
            body: &bytes,
            principal: who,
            peer: None,
        },
    )
}

fn admin() -> Principal {
    Principal::admin("root")
}

fn error_code(reply: &Reply) -> &str {
    reply.body.as_ref().expect("error body")["error"]["code"]
        .as_str()
        .expect("code")
}

#[test]
fn only_registry_shapes_match_and_percent_encoding_is_refused() {
    assert_eq!(
        match_route("/api/v1/registrations"),
        Some(Route::Registrations)
    );
    assert_eq!(
        match_route("/api/v1/pools/render/servers"),
        Some(Route::Servers { pool: "render" })
    );
    assert_eq!(
        match_route("/api/v1/pools/render/servers/gpu-1/heartbeat"),
        Some(Route::Heartbeat {
            pool: "render",
            name: "gpu-1"
        })
    );
    for path in [
        "/api/v1/pools",
        "/api/v1/pools/administrative-state",
        "/api/v1/pools/render",
        "/api/v1/pools/render/servers/",
        "/api/v1/pools/render/servers/a%2fb",
        "/api/v1/pools/render/servers/a/b/c",
        "/api/v1/servers/administrative-state",
    ] {
        assert_eq!(match_route(path), None, "{path}");
    }
}

#[test]
fn put_creates_then_replaces_and_the_listing_shows_both_origins() {
    let f = fixture();
    let body = json!({"address": "192.0.2.10:9000", "weight": 1, "labels": {"gpu": "a100"}, "ttlMs": 10000});

    let created = call(
        &f,
        &admin(),
        "PUT",
        "/api/v1/pools/render/servers/gpu-1",
        body.clone(),
    );
    let replaced = call(
        &f,
        &admin(),
        "PUT",
        "/api/v1/pools/render/servers/gpu-1",
        body,
    );

    assert_eq!((created.status, replaced.status), (201, 200));
    let created = created.body.expect("body");
    assert_eq!(created["expiresInMs"], 10_000);
    assert_eq!(created["server"]["effectiveState"], "ready");
    assert_eq!(created["registryRevision"], "1");
    let listing = call(
        &f,
        &admin(),
        "GET",
        "/api/v1/pools/render/servers",
        serde_json::Value::Null,
    );
    let servers = &listing.body.expect("listing")["servers"];
    assert_eq!(servers[0]["origin"], "static");
    assert_eq!(servers[1]["origin"], "dynamic");
    assert_eq!(servers[1]["registration"]["labels"]["gpu"], "a100");
    let one = call(
        &f,
        &admin(),
        "GET",
        "/api/v1/pools/render/servers/gpu-1",
        serde_json::Value::Null,
    );
    assert_eq!(one.body.expect("server")["servers"][0]["name"], "gpu-1");
}

#[test]
fn heartbeat_deregister_and_operator_state_round_trip() {
    let f = fixture();
    call(
        &f,
        &admin(),
        "PUT",
        "/api/v1/pools/render/servers/gpu-1",
        json!({"address": "192.0.2.10:9000"}),
    );

    f.clock.advance(Duration::from_secs(5));
    let beat = call(
        &f,
        &admin(),
        "POST",
        "/api/v1/pools/render/servers/gpu-1/heartbeat",
        serde_json::Value::Null,
    );
    assert_eq!(
        (
            beat.status,
            beat.body.expect("body")["expiresInMs"].as_u64()
        ),
        (200, Some(15_000))
    );
    let drain = call(
        &f,
        &admin(),
        "POST",
        "/api/v1/pools/render/servers/gpu-1/state",
        json!({"state": "drain"}),
    );
    assert_eq!(drain.status, 204);
    let one = call(
        &f,
        &admin(),
        "GET",
        "/api/v1/pools/render/servers/gpu-1",
        serde_json::Value::Null,
    );
    assert_eq!(
        one.body.expect("server")["servers"][0]["administrativeState"],
        "drain"
    );
    let gone = call(
        &f,
        &admin(),
        "DELETE",
        "/api/v1/pools/render/servers/gpu-1",
        serde_json::Value::Null,
    );
    assert_eq!(gone.status, 204);
    let again = call(
        &f,
        &admin(),
        "DELETE",
        "/api/v1/pools/render/servers/gpu-1",
        serde_json::Value::Null,
    );
    assert_eq!(
        (again.status, error_code(&again)),
        (404, "registration_not_found")
    );
    let beat = call(
        &f,
        &admin(),
        "POST",
        "/api/v1/pools/render/servers/gpu-1/heartbeat",
        serde_json::Value::Null,
    );
    assert_eq!(
        (beat.status, error_code(&beat)),
        (404, "registration_not_found")
    );
}

#[test]
fn malformed_oversized_and_unknown_bodies_are_rejected() {
    let f = fixture();
    let put = |body: serde_json::Value| {
        call(
            &f,
            &admin(),
            "PUT",
            "/api/v1/pools/render/servers/gpu-1",
            body,
        )
    };

    assert_eq!(
        error_code(&put(json!({"address": "192.0.2.10:9000", "surprise": 1}))),
        "invalid_request"
    );
    assert_eq!(error_code(&put(json!({"weight": 1}))), "invalid_address");
    assert_eq!(
        error_code(&put(json!({"address": "192.0.2.10:9000", "ttlMs": 5}))),
        "invalid_ttl"
    );
    let huge = put(json!({"address": "192.0.2.10:9000", "metadata": "x".repeat(5000)}));
    assert_eq!((huge.status, error_code(&huge)), (413, "body_too_large"));
}

#[test]
fn credentials_are_checked_per_route() {
    let f = fixture();
    let reader = Principal {
        id: "ro".into(),
        scope: Scope::Read,
    };
    let worker = Principal {
        id: "w".into(),
        scope: Scope::Register {
            pools: vec!["render".into()],
            name_prefix: None,
        },
    };

    let denied = call(
        &f,
        &reader,
        "PUT",
        "/api/v1/pools/render/servers/gpu-1",
        json!({"address": "192.0.2.10:9000"}),
    );
    assert_eq!((denied.status, error_code(&denied)), (403, "scope_denied"));
    let denied = call(
        &f,
        &worker,
        "GET",
        "/api/v1/registrations",
        serde_json::Value::Null,
    );
    assert_eq!(denied.status, 403);
    let denied = call(
        &f,
        &worker,
        "POST",
        "/api/v1/pools/render/servers/gpu-1/state",
        json!({"state": "drain"}),
    );
    assert_eq!(denied.status, 403);
    let created = call(
        &f,
        &worker,
        "PUT",
        "/api/v1/pools/render/servers/gpu-1",
        json!({"address": "192.0.2.10:9000", "metadata": {"k": 1}}),
    );
    assert_eq!(created.status, 201);
    assert!(
        created.body.expect("body")["server"]["metadata"].is_null(),
        "register scope never reads metadata back"
    );
    let all = call(
        &f,
        &reader,
        "GET",
        "/api/v1/registrations",
        serde_json::Value::Null,
    );
    let body = all.body.expect("registrations");
    assert_eq!(body["registrations"][0]["registration"]["metadata"]["k"], 1);
    assert_eq!(body["writer"], true);
}

#[test]
fn wrong_methods_name_the_allowed_ones_and_a_follower_asks_clients_to_reconnect() {
    let f = fixture();
    let wrong = call(
        &f,
        &admin(),
        "PATCH",
        "/api/v1/pools/render/servers/gpu-1",
        json!({}),
    );
    assert_eq!((wrong.status, wrong.allow), (405, Some("GET, PUT, DELETE")));
    f.service.set_writer(false);

    let refused = call(
        &f,
        &admin(),
        "PUT",
        "/api/v1/pools/render/servers/gpu-1",
        json!({"address": "192.0.2.10:9000"}),
    );

    assert_eq!(
        (refused.status, error_code(&refused), refused.close),
        (409, "generation_inactive", true)
    );
}

#[test]
fn unknown_pools_are_404_and_rate_limited_callers_get_retry_after() {
    let f = fixture();
    let missing = call(
        &f,
        &admin(),
        "GET",
        "/api/v1/pools/ghost/servers",
        serde_json::Value::Null,
    );
    assert_eq!(
        (missing.status, error_code(&missing)),
        (404, "pool_not_found")
    );

    let mut limited = None;
    for _ in 0..100 {
        if let Err(error) = f.service.check_rate("peer") {
            limited = Some(Reply::error(&error));
            break;
        }
    }
    let limited = limited.expect("burst exhausted");
    assert_eq!(
        (limited.status, limited.retry_after_seconds.is_some()),
        (429, true)
    );
}
