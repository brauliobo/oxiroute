#![allow(dead_code)]

#[path = "support/config.rs"]
mod config_support;
#[path = "support/fixtures.rs"]
mod fixture_support;
#[path = "support/http.rs"]
mod http_support;
#[path = "support/process.rs"]
mod process_support;

use std::{fs, net::SocketAddr, time::Duration};

use config_support::{empty_config, socket_bind};
use fixture_support::fixture;
use http_support::{HttpResponse, http_request};
use oxiroute_config::{
    ConfigDraft, DownstreamTimeoutPolicy, HttpRoute, HttpRouteAction, HttpService,
    HttpVersionPolicy, Listener, Management, Protocol, UpstreamAlgorithm, UpstreamConnectionReuse,
    UpstreamEndpoint, UpstreamPool, UpstreamServer,
};
use process_support::{ServerProcess, reserve_tcp_address};
use serde_json::Value;
use tokio::time::{Instant, sleep};

const MANAGEMENT_TOKEN: &str = "phase6-management-secret-canary-1";
const SECRET_CANARY: &str = "phase6-management-secret-canary";

#[tokio::test]
async fn read_only_management_responses_preserve_dto_wire_shapes() {
    let management_address = reserve_tcp_address();
    let listener_address = reserve_tcp_address();
    let config = characterization_config(management_address, listener_address)
        .validate()
        .expect("valid characterization config");
    let mut server = ServerProcess::start(&config, Some(MANAGEMENT_TOKEN));
    server.wait_for_tcp(management_address).await;

    let generation = wait_for_generation(management_address).await;
    let listeners = authorized_get(management_address, "/api/v1/listeners").await;
    let pools = authorized_get(management_address, "/api/v1/pools").await;
    let servers = authorized_get(management_address, "/api/v1/servers").await;

    for response in [&generation, &listeners, &pools, &servers] {
        assert_eq!(response.status, 200, "{}", response.text());
        assert_eq!(response.header("content-type"), Some("application/json"));
        let wire = String::from_utf8_lossy(response.body());
        assert!(!wire.contains(MANAGEMENT_TOKEN));
        assert!(!wire.contains(SECRET_CANARY));
        let private_key =
            fs::read_to_string(fixture("proxy-a-key.pem")).expect("private key fixture");
        assert!(!wire.contains(private_key.trim()));
    }

    let generation = generation.json();
    let generation = generation["generation"]
        .as_object()
        .expect("generation object");
    assert!(generation["buildVersion"].is_string());
    for key in [
        "diskRevision",
        "candidateRevision",
        "activeRevision",
        "previousRevision",
        "quarantinedRevision",
    ] {
        assert!(
            generation[key].is_string() || generation[key].is_null(),
            "{key}"
        );
    }
    assert!(generation["activeAccepting"].is_boolean());
    assert!(generation["degraded"].is_boolean());
    for key in ["prepares", "activations", "failures", "rollbacks"] {
        assert!(generation[key].is_u64(), "generation counter {key}");
    }

    let listeners = listeners.json();
    let listener = &listeners["listeners"][0];
    assert_eq!(listener["name"], "phase6-edge");
    assert_enum(
        &listener["administrativeState"],
        &["ready", "drain", "maintenance"],
    );
    assert_enum(
        &listener["state"],
        &["configured", "listening", "stopped", "failed"],
    );
    assert_eq!(listener["protocol"], "http");
    assert_eq!(listener["bind"], format!("socket:{listener_address}"));
    assert_eq!(listener["maxConnections"], 8);
    for key in [
        "acceptedConnections",
        "rejectedConnections",
        "bytesReceived",
        "bytesSent",
    ] {
        assert_decimal(&listener[key]);
    }
    assert!(listener["httpOperations"].is_null());
    assert!(listener["tcpRelays"].is_null());
    assert!(listener["proxyProtocol"].is_null());
    assert!(listener["cache"].is_null());

    let pools = pools.json();
    let pool = &pools["pools"][0];
    assert_eq!(pool["name"], "phase6-origin");
    assert_enum(
        &pool["algorithm"],
        &[
            "first",
            "round_robin",
            "least_connections",
            "weighted_round_robin",
        ],
    );
    assert!(pool["availableEndpoints"].is_u64());
    assert!(pool["totalEndpoints"].is_u64());
    for key in [
        "unavailableSelections",
        "queuedTotal",
        "queueTimeouts",
        "queueCancellations",
    ] {
        assert_decimal(&pool[key]);
    }
    assert_eq!(pool["endpoints"].as_array().map(Vec::len), Some(1));
    assert_endpoint(&pool["endpoints"][0]);

    let servers = servers.json();
    assert_eq!(servers["servers"].as_array().map(Vec::len), Some(1));
    assert_eq!(servers["servers"][0]["pool"], "phase6-origin");
    assert_endpoint(&servers["servers"][0]["server"]);

    server.shutdown();
}

#[tokio::test]
async fn management_rejects_malformed_control_payloads_without_reflecting_secrets() {
    let management_address = reserve_tcp_address();
    let listener_address = reserve_tcp_address();
    let config = characterization_config(management_address, listener_address)
        .validate()
        .expect("valid characterization config");
    let mut server = ServerProcess::start(&config, Some(MANAGEMENT_TOKEN));
    server.wait_for_tcp(management_address).await;
    let _ = wait_for_generation(management_address).await;

    let authorization = format!("Bearer {MANAGEMENT_TOKEN}");
    let headers = [
        ("Authorization", authorization.as_str()),
        ("Content-Type", "application/json"),
    ];
    let malformed = http_request(
        management_address,
        "POST",
        "/api/v1/listeners/administrative-state",
        &headers,
        br#"{"secret":"phase6-request-secret-canary"#,
    )
    .await;
    assert_eq!(malformed.status, 400);
    assert_eq!(malformed.json()["error"]["code"], "invalid_json");
    assert!(!String::from_utf8_lossy(malformed.body()).contains("phase6-request-secret-canary"));

    let unknown_field = http_request(
        management_address,
        "POST",
        "/api/v1/listeners/administrative-state",
        &headers,
        br#"{"listeners":["phase6-edge"],"state":"ready","expectedActiveRevision":"invalid","secret":"phase6-request-secret-canary"}"#,
    )
    .await;
    assert_eq!(unknown_field.status, 400);
    assert_eq!(unknown_field.json()["error"]["code"], "invalid_json");
    assert!(
        !String::from_utf8_lossy(unknown_field.body()).contains("phase6-request-secret-canary")
    );

    server.shutdown();
}

async fn wait_for_generation(address: SocketAddr) -> HttpResponse {
    let deadline = Instant::now() + process_support::PROCESS_TIMEOUT;
    loop {
        let response = authorized_get(address, "/api/v1/generations").await;
        if response.status == 200 {
            return response;
        }
        assert!(
            Instant::now() < deadline,
            "generation did not become available: {}",
            response.text()
        );
        sleep(Duration::from_millis(10)).await;
    }
}

async fn authorized_get(address: SocketAddr, path: &str) -> HttpResponse {
    let authorization = format!("Bearer {MANAGEMENT_TOKEN}");
    http_request(
        address,
        "GET",
        path,
        &[("Authorization", authorization.as_str())],
        &[],
    )
    .await
}

fn assert_decimal(value: &Value) {
    assert!(
        value.as_str().is_some_and(|value| {
            !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
        }),
        "expected decimal string, got {value}"
    );
}

fn assert_enum(value: &Value, allowed: &[&str]) {
    assert!(
        value.as_str().is_some_and(|value| allowed.contains(&value)),
        "unexpected enum value {value}; allowed values: {allowed:?}"
    );
}

fn assert_endpoint(endpoint: &Value) {
    assert_enum(
        &endpoint["administrativeState"],
        &["ready", "drain", "maintenance"],
    );
    assert_enum(
        &endpoint["state"],
        &["unchecked", "unknown", "healthy", "unhealthy"],
    );
    assert_enum(&endpoint["healthOverride"], &["auto", "up", "down"]);
    assert!(endpoint["address"].is_string());
    assert!(endpoint["checksEnabled"].is_boolean());
    assert!(endpoint["checksRunning"].is_boolean());
    assert!(endpoint["configuredMaxConnections"].is_null());
    assert!(endpoint["maxConnections"].is_null());
    assert!(endpoint["lastCheckedAtUnixMs"].is_null());
    assert!(endpoint["lastTransitionAtUnixMs"].is_null());
    assert!(endpoint["lastFailure"].is_null());
    assert!(endpoint["passiveEjectionReason"].is_null());
    for key in [
        "activeConnections",
        "successfulChecks",
        "failedChecks",
        "consecutiveSuccesses",
        "consecutiveFailures",
        "passiveFailureCount",
        "passiveConsecutiveFailures",
        "passiveEjectionCount",
        "passiveRecoveryCount",
    ] {
        assert_decimal(&endpoint[key]);
    }
}

fn characterization_config(
    management_address: SocketAddr,
    listener_address: SocketAddr,
) -> ConfigDraft {
    let mut config = empty_config();
    config.management = Some(Management {
        bind: management_address,
        ui_dir: None,
    });
    config.listeners = vec![Listener {
        name: "phase6-edge".into(),
        bind: socket_bind(listener_address),
        protocol: Protocol::Http,
        service: Some("phase6-web".into()),
        tls_profile: None,
        proxy_protocol: None,
        max_connections: Some(8),
        downstream_timeouts: DownstreamTimeoutPolicy::default(),
    }];
    config.upstream_pools = vec![UpstreamPool {
        name: "phase6-origin".into(),
        servers: vec![UpstreamServer {
            name: "origin-a".into(),
            endpoint: UpstreamEndpoint::Socket {
                address: "127.0.0.1:9".parse().expect("characterization endpoint"),
            },
            max_connections: None,
            dns_resolution: oxiroute_config::DnsResolutionPolicy::OnConnect,
        }],
        endpoints: Vec::new(),
        algorithm: UpstreamAlgorithm::RoundRobin,
        health_check: None,
        passive_health: None,
        tls: None,
        http_versions: HttpVersionPolicy::default(),
        queue_timeout_ms: None,
        connect_timeout_ms: None,
        server_timeout_ms: None,
        connection_reuse: UpstreamConnectionReuse::Safe,
    }];
    config.http_services = vec![HttpService {
        name: "phase6-web".into(),
        routes: vec![HttpRoute {
            host: None,
            path: oxiroute_config::HttpPathSelector::SegmentPrefix { value: "/".into() },
            methods: Vec::new(),
            access_policy: None,
            policy: oxiroute_config::HttpRoutePolicy::default(),
            action: HttpRouteAction::FixedResponse {
                status: 200,
                body: "phase6".into(),
                headers: Vec::new(),
            },
        }],
        automatic_response_headers: true,
        upstream_io_timeout_ms: 30_000,
        max_request_body_bytes: Some(10 * 1024 * 1024),
        gzip: None,
        access_log: None,
    }];
    config
}
