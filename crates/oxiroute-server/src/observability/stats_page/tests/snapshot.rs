use serde_json::{Value, json};

use super::fixture::{API, WEB, fixture, fixture_from};
use crate::{
    AdministrativeState, EndpointHealthState, HealthOverride, RuntimeMetrics,
    observability::{
        stats_page::{
            builder::{
                build_snapshot,
                certificates::{list as certificates, parse_openssl_time},
                pools::server_status,
            },
            model::*,
        },
        upstream_stats::{AttemptOutcome, upstream_stats},
    },
};
use std::time::Duration;

fn snapshot() -> (super::fixture::Fixture, StatsSnapshot) {
    let fixture = fixture();
    let snapshot = build_snapshot(&fixture.generations, 5_000).expect("snapshot");
    (fixture, snapshot)
}

fn json_of(snapshot: &StatsSnapshot) -> Value {
    serde_json::to_value(StatsDocument {
        admin: false,
        snapshot,
    })
    .expect("json")
}

#[test]
fn the_snapshot_lists_every_listener_route_pool_and_server_with_their_configuration() {
    let (fixture, snapshot) = snapshot();
    assert_eq!(snapshot.schema_version, 1);
    assert_eq!(snapshot.interval_hint_ms, 5_000);
    assert_eq!(snapshot.process.version, crate::cli::BUILD_VERSION);
    assert_eq!(
        snapshot.generation.active_revision.as_deref(),
        Some(fixture.revision.as_str())
    );
    assert!(snapshot.generation.retired.is_empty());

    let front = snapshot
        .listeners
        .iter()
        .find(|l| l.name == "front")
        .expect("front");
    assert_eq!(
        (front.protocol.as_str(), front.service.as_deref()),
        ("http", Some("site"))
    );
    assert!(front.http.is_none() && front.upstream_pool.is_none());
    let db = snapshot
        .listeners
        .iter()
        .find(|l| l.name == "db")
        .expect("db");
    assert_eq!(db.upstream_pool.as_deref(), Some(API));
    assert!(db.http.is_none());

    let ids: Vec<_> = snapshot
        .routes
        .iter()
        .map(|route| route.id.as_str())
        .collect();
    assert_eq!(ids, ["site#0", "site#1", "site#2"]);
    let [first, second, third] = &snapshot.routes[..] else {
        panic!("three routes")
    };
    assert_eq!(first.listeners, ["front"]);
    assert_eq!(
        (first.pool.as_deref(), first.max_retries),
        (Some(WEB), Some(2))
    );
    assert_eq!(
        second.host.as_ref().map(|host| host.value.as_str()),
        Some("api.example.com")
    );
    assert_eq!(
        (second.path.kind, second.methods.as_slice()),
        (SelectorKind::Exact, ["GET".to_owned()].as_slice())
    );
    assert_eq!(
        (third.action, third.status, third.pool.as_deref()),
        (RouteAction::FixedResponse, Some(410), None)
    );

    let web = snapshot
        .pools
        .iter()
        .find(|pool| pool.name == WEB)
        .expect("web pool");
    assert_eq!(
        (web.total_servers, web.available_servers, web.status),
        (2, 2, PoolStatus::Up)
    );
    assert_eq!(web.queue.timeout_ms, Some(2_000));
    let b = &web.servers[1];
    assert_eq!(
        (b.name.as_str(), b.address.as_str()),
        ("origin-b", "192.0.2.11:3000")
    );
    assert_eq!(
        (b.max_connections, b.configured_max_connections),
        (Some(50), Some(50))
    );
    assert_eq!(
        (b.origin, b.status, b.health),
        (
            ServerOrigin::Static,
            ServerStatus::Unchecked,
            HealthState::Unchecked
        )
    );
    assert!(web.servers.iter().all(|server| server.available));
}

#[test]
fn a_value_the_proxy_does_not_know_is_absent_instead_of_zero() {
    let (_fixture, snapshot) = snapshot();
    let document = json_of(&snapshot);
    let pool = document["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == API)
        .unwrap();
    assert!(pool["queue"].get("timeoutMs").is_none());
    assert!(pool["queue"].get("byClass").is_none());
    assert!(pool.get("canary").is_none());
    let server = &pool["servers"][0];
    for key in ["lastCheckedAtUnixMs", "lastFailure"] {
        assert!(server["check"].get(key).is_none(), "{key}");
    }
    for key in [
        "downtimeMs",
        "lastTransitionAtUnixMs",
        "maxConnections",
        "checkDurationMs",
        "backup",
    ] {
        assert!(server.get(key).is_none(), "{key}");
    }
    let relay = document["listeners"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["name"] == "db")
        .unwrap();
    assert!(relay.get("http").is_none() && relay.get("tls_profile").is_none());
    assert!(document["routes"][0].get("rescues").is_none());
}

#[test]
fn counters_are_decimal_strings_and_gauges_are_numbers() {
    let (_fixture, snapshot) = snapshot();
    let document = json_of(&snapshot);
    let server = &document["pools"][0]["servers"][0];
    assert!(server["traffic"]["bytesSent"].is_string());
    assert!(server["traffic"]["responses"]["2xx"].is_string());
    assert!(server["traffic"]["sessions"]["current"].is_u64());
    assert!(document["listeners"][0]["acceptedConnections"].is_string());
    assert!(document["listeners"][0]["activeConnections"].is_u64());
    assert!(document["sampledAtUnixMs"].is_u64());
    assert_eq!(
        document["bucketBoundsMs"]["operation"]
            .as_array()
            .unwrap()
            .len(),
        14
    );
}

#[test]
fn per_server_responses_attempts_bytes_and_latency_come_from_the_upstream_counters() {
    let (fixture, _) = snapshot();
    let stats = upstream_stats().server(WEB, "origin-b");
    stats.record_response(Some(204));
    stats.record_response(Some(503));
    stats.record_response(Some(503));
    stats.record_response(None);
    stats.record_attempt(AttemptOutcome::Ok);
    stats.record_attempt(AttemptOutcome::ConnectFailed);
    stats.record_attempt(AttemptOutcome::StatusRetry);
    stats.record_bytes(100, 2_000);
    stats.record_request_duration(Duration::from_millis(30));
    stats.record_header_duration(Duration::from_millis(7));
    upstream_stats()
        .pool(WEB)
        .record_queue_wait(Duration::from_millis(3));
    let snapshot = build_snapshot(&fixture.generations, 1_000).expect("snapshot");
    let web = snapshot.pools.iter().find(|pool| pool.name == WEB).unwrap();
    let document = json_of(&snapshot);
    let web_json = document["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == WEB)
        .unwrap();
    let server = &web_json["servers"][1];
    assert_eq!(
        server["traffic"]["responses"],
        json!({"1xx":"0","2xx":"1","3xx":"0","4xx":"0","5xx":"2","none":"1"})
    );
    assert_eq!(server["traffic"]["attempts"]["connectFailed"], "1");
    assert_eq!(server["traffic"]["sessions"]["total"], "3");
    assert_eq!(
        (
            &server["traffic"]["bytesSent"],
            &server["traffic"]["bytesReceived"]
        ),
        (&json!("100"), &json!("2000"))
    );
    assert_eq!(server["latency"]["request"]["count"], "1");
    assert_eq!(server["latency"]["request"]["sumMs"], "30");
    let buckets = server["latency"]["request"]["buckets"].as_array().unwrap();
    assert_eq!(buckets.len(), 14);
    assert_eq!(
        (&buckets[3], &buckets[4]),
        (&json!("0"), &json!("1")),
        "30 ms is above 25 and at most 50"
    );
    assert_eq!(server["latency"]["header"]["sumMs"], "7");
    assert_eq!(
        web_json["queue"]["wait"]["buckets"]
            .as_array()
            .unwrap()
            .len(),
        17
    );
    assert_eq!(web_json["queue"]["wait"]["count"], "1");
    // The pool is the sum of its servers.
    assert_eq!(web.traffic.sessions.total.get(), 3);
    assert_eq!(web.traffic.responses.c5xx.get(), 2);
    assert_eq!(web_json["latency"]["request"]["count"], "1");
}

#[test]
fn the_status_of_a_server_follows_administration_ejection_override_then_health() {
    let (fixture, _) = snapshot();
    let pool = fixture.pool(WEB);
    let mut endpoint = pool.health_snapshot().endpoints[0].clone();
    let status = server_status;
    assert_eq!(status(&endpoint), ServerStatus::Unchecked);
    endpoint.state = EndpointHealthState::Healthy;
    assert_eq!(status(&endpoint), ServerStatus::Up);
    endpoint.state = EndpointHealthState::Unhealthy;
    assert_eq!(status(&endpoint), ServerStatus::Down);
    endpoint.state = EndpointHealthState::Unknown;
    assert_eq!(status(&endpoint), ServerStatus::Unknown);
    endpoint.health_override = HealthOverride::Up;
    assert_eq!(status(&endpoint), ServerStatus::Up);
    endpoint.health_override = HealthOverride::Down;
    assert_eq!(status(&endpoint), ServerStatus::Down);
    endpoint.passive_ejected = true;
    assert_eq!(status(&endpoint), ServerStatus::Ejected);
    endpoint.administrative_state = AdministrativeState::Drain;
    assert_eq!(status(&endpoint), ServerStatus::Drain);
    endpoint.administrative_state = AdministrativeState::Maintenance;
    assert_eq!(status(&endpoint), ServerStatus::Maint);
}

#[test]
fn administrative_changes_show_in_server_and_pool_status() {
    let (fixture, _) = snapshot();
    let pool = fixture.pool(WEB);
    pool.set_server_administrative_state("origin-a", AdministrativeState::Drain)
        .unwrap();
    pool.set_server_administrative_state("origin-b", AdministrativeState::Maintenance)
        .unwrap();
    let snapshot = build_snapshot(&fixture.generations, 1_000).unwrap();
    let web = snapshot.pools.iter().find(|pool| pool.name == WEB).unwrap();
    let statuses: Vec<_> = web.servers.iter().map(|server| server.status).collect();
    assert_eq!(statuses, [ServerStatus::Drain, ServerStatus::Maint]);
    assert_eq!((web.available_servers, web.status), (0, PoolStatus::Down));
    assert_eq!(
        web.servers.iter().filter(|server| server.available).count() as u64,
        web.available_servers
    );
}

#[test]
fn configuration_warnings_and_generation_state_are_reported() {
    let (_fixture, snapshot) = snapshot();
    let codes: Vec<_> = snapshot
        .warnings
        .iter()
        .map(|warning| warning.code.as_str())
        .collect();
    assert_eq!(codes, [oxiroute_config::W_PASSIVE_HEALTH_MISSING]);
    assert!(snapshot.warnings[0].message.contains(WEB));
    assert_eq!(snapshot.generation.number, 1);
}

#[test]
fn certificate_expiry_is_read_from_the_printed_validity() {
    assert_eq!(parse_openssl_time("Jan  1 00:00:00 1970 GMT"), Some(0));
    assert_eq!(
        parse_openssl_time("Jan  5 12:00:00 2030 GMT"),
        Some(1_893_844_800)
    );
    assert_eq!(
        parse_openssl_time("Feb 29 23:59:59 2028 GMT"),
        Some(1_835_481_599)
    );
    for invalid in [
        "",
        "not a time",
        "Foo  5 12:00:00 2030 GMT",
        "Jan  5 12:00 2030 GMT",
        "Jan  5 12:00:00 2030 UTC",
        "Jan 40 12:00:00 2030 GMT",
    ] {
        assert_eq!(parse_openssl_time(invalid), None, "{invalid}");
    }
    let mut runtime = RuntimeMetrics::new().snapshot().expect("snapshot");
    runtime
        .direct_file_certificates
        .push(crate::DirectFileCertificateSnapshot {
            name: "edge".into(),
            active_content_revision: "r1".into(),
            expires_at: "Jan  5 12:00:00 2030 GMT".into(),
            last_outcome: Some("activated".into()),
            last_error_code: None,
        });
    let certificates = certificates(&runtime);
    assert_eq!(certificates.len(), 1);
    assert_eq!(certificates[0].source, CertificateSource::DirectFile);
    assert_eq!(certificates[0].not_after_unix_seconds, Some(1_893_844_800));
    assert_eq!(certificates[0].last_outcome.as_deref(), Some("activated"));
}

#[test]
fn listener_and_route_counters_come_from_the_runtime_metrics() {
    use crate::{HttpOperationResult, TcpRelayResult};
    let fixture = fixture();
    let front = fixture
        .generation
        .traffic_listener_metrics("front")
        .unwrap();
    front
        .record_http_operation(HttpOperationResult::ClientError, Duration::from_millis(30))
        .unwrap();
    front
        .record_http_operation(HttpOperationResult::Success, Duration::from_millis(2))
        .unwrap();
    front.record_bytes_received(11).unwrap();
    front.record_rescue("0", 502);
    front.record_rescue("0", 502);
    front.record_pool_selection("1", API);
    fixture
        .generation
        .traffic_listener_metrics("db")
        .unwrap()
        .record_tcp_relay(TcpRelayResult::IdleTimeout, Duration::from_millis(900))
        .unwrap();
    let snapshot = build_snapshot(&fixture.generations, 1_000).unwrap();

    let document = json_of(&snapshot);
    let listeners = document["listeners"].as_array().unwrap();
    let front = listeners.iter().find(|l| l["name"] == "front").unwrap();
    assert_eq!(front["bytesReceived"], "11");
    assert_eq!(front["http"]["total"], "2");
    let outcome = |name: &str| {
        front["http"]["outcomes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["result"] == name)
            .unwrap()["count"]
            .clone()
    };
    assert_eq!(
        (
            outcome("success"),
            outcome("client_error"),
            outcome("timeout")
        ),
        ("1".into(), "1".into(), "0".into())
    );
    assert_eq!(front["http"]["latency"]["sumMs"], "32");
    assert_eq!(
        front["http"]["latency"]["buckets"]
            .as_array()
            .unwrap()
            .len(),
        14
    );
    let db = listeners.iter().find(|l| l["name"] == "db").unwrap();
    assert_eq!(db["tcpRelays"]["total"], "1");
    assert_eq!(
        db["tcpRelays"]["outcomes"][3],
        json!({"result": "idle_timeout", "count": "1"})
    );

    let routes = document["routes"].as_array().unwrap();
    assert_eq!(routes[0]["rescues"], json!([{"status": 502, "count": "2"}]));
    assert_eq!(
        routes[1]["poolSelections"],
        json!([{"pool": API, "count": "1"}])
    );
    assert!(routes[2].get("rescues").is_none() && routes[2].get("poolSelections").is_none());
}

#[test]
fn a_configuration_at_the_server_limit_gives_a_bounded_snapshot_quickly() {
    use std::fmt::Write as _;
    let mut pools = String::new();
    for pool in 0..4 {
        write!(pools, "{{ name = \"sp-big-{pool}\", servers = {{ ").unwrap();
        for index in 0..256 {
            let (host, port) = (index % 250 + 1, 3_000 + index / 250);
            let endpoint = format!("{{ type = \"socket\", address = \"192.0.2.{host}:{port}\" }}");
            write!(
                pools,
                "{{ name = \"node-{index}\", endpoint = {endpoint} }},"
            )
            .unwrap();
        }
        pools.push_str(" } },");
    }
    let source =
        format!("return {{ version = 1, listeners = {{}}, upstream_pools = {{ {pools} }} }}");
    let fixture = fixture_from(&source);
    let started = std::time::Instant::now();
    let snapshot = build_snapshot(&fixture.generations, 1_000).unwrap();
    let json = serde_json::to_vec(&StatsDocument {
        admin: false,
        snapshot: &snapshot,
    })
    .unwrap();
    let csv = crate::observability::stats_page::csv::render(&snapshot);
    assert_eq!(
        snapshot
            .pools
            .iter()
            .map(|pool| pool.servers.len())
            .sum::<usize>(),
        1_024
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert!(json.len() < 2_000_000, "{} bytes", json.len());
    assert!(csv.len() < 250_000, "{} bytes", csv.len());
    eprintln!(
        "1024 servers: {} B JSON, {} B CSV, {:?}",
        json.len(),
        csv.len(),
        started.elapsed()
    );
}
