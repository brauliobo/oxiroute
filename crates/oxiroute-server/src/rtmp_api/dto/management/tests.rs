use std::str::FromStr;

use schemars::{JsonSchema, generate::SchemaSettings};
use serde_json::{Value, json};

use super::*;
use crate::{
    AcmeManagedCertificateSnapshot, ComponentState, ComponentStatus, HostSnapshot, ProcessSnapshot,
    RuntimeEndpoint, TrafficSnapshot,
    config_coordinator::{AuthoredRevision, EffectiveRevision},
};

#[test]
fn management_response_schema_roots_match_the_version_one_contract() {
    let roots = [
        (
            "GenerationResponse",
            "generation",
            response_schema::<GenerationResponse>(),
        ),
        (
            "ListenerInventoryResponse",
            "listeners",
            response_schema::<ListenerInventoryResponse>(),
        ),
        (
            "PoolInventoryResponse",
            "pools",
            response_schema::<PoolInventoryResponse>(),
        ),
        (
            "ServerInventoryResponse",
            "servers",
            response_schema::<ServerInventoryResponse>(),
        ),
    ];

    for (title, property, schema) in roots {
        assert_eq!(schema["title"], title);
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!([property]));
        assert!(schema["properties"].get(property).is_some());
    }
}

#[test]
fn management_schemas_preserve_decimal_null_enum_and_secret_contracts() {
    let generation = response_schema::<GenerationResponse>();
    let listeners = response_schema::<ListenerInventoryResponse>();
    let pools = response_schema::<PoolInventoryResponse>();

    assert_eq!(
        listeners["$defs"]["DecimalCounter"],
        json!({ "type": "string", "pattern": "^[0-9]+$" })
    );
    assert_eq!(
        generation["$defs"]["GenerationStatusDto"]["properties"]["lastFailure"],
        json!({ "type": ["string", "null"] })
    );
    assert!(
        generation["$defs"]["GenerationStatusDto"]["required"]
            .as_array()
            .is_some_and(|required| required.contains(&json!("lastFailure")))
    );
    assert_eq!(
        pools["$defs"]["EndpointDto"]["properties"]["lastFailure"],
        json!({
            "anyOf": [
                { "$ref": "#/$defs/HealthFailureDto" },
                { "type": "null" },
            ],
        })
    );

    assert_eq!(
        listeners["$defs"]["AdministrativeStateDto"]["enum"],
        json!(["ready", "drain", "maintenance"])
    );
    assert_eq!(
        listeners["$defs"]["ListenerRuntimeStateDto"]["enum"],
        json!(["configured", "listening", "stopped", "failed"])
    );
    assert_eq!(
        listeners["$defs"]["HttpOperationResultDto"]["enum"],
        json!([
            "success",
            "client_error",
            "server_error",
            "upstream_error",
            "timeout",
            "cancelled",
            "internal_error",
        ])
    );
    assert_eq!(
        listeners["$defs"]["TcpRelayResultDto"]["enum"],
        json!([
            "success",
            "connect_error",
            "connect_timeout",
            "idle_timeout",
            "lifetime_timeout",
            "cancelled",
            "io_error",
            "accounting_error",
            "proxy_protocol_error",
        ])
    );
    assert_eq!(
        listeners["$defs"]["ProxyProtocolResultDto"]["enum"],
        json!([
            "accepted",
            "sent",
            "timeout",
            "cancelled",
            "malformed",
            "unsupported",
            "mismatch",
            "io_error",
        ])
    );
    assert_eq!(
        pools["$defs"]["EndpointHealthStateDto"]["enum"],
        json!(["unchecked", "unknown", "healthy", "unhealthy"])
    );
    assert_eq!(
        pools["$defs"]["HealthOverrideDto"]["enum"],
        json!(["auto", "up", "down"])
    );
    assert_eq!(
        pools["$defs"]["HealthFailureDto"]["enum"],
        json!([
            "timeout",
            "connect_failed",
            "unexpected_status",
            "protocol_error",
        ])
    );

    for schema in [&generation, &listeners, &pools] {
        let schema = serde_json::to_string(schema).expect("management response schema JSON");
        for secret in ["private-key", "session-secret", "token", "password"] {
            assert!(!schema.contains(secret), "schema exposed {secret}");
        }
    }
}

fn response_schema<T: JsonSchema>() -> Value {
    let generator = SchemaSettings::default().for_serialize().into_generator();
    serde_json::to_value(generator.into_root_schema_for::<T>()).expect("response schema")
}

#[test]
#[allow(clippy::too_many_lines)]
fn management_dtos_match_the_version_one_golden_json() {
    let generation = GenerationResponse::from(generation());
    let listeners = ListenerInventoryResponse::from(runtime_snapshot(vec![listener()]));
    let pools = vec![pool(), secondary_pool()];
    let value = json!({
        "error": serde_json::to_value(super::super::ErrorResponse::new(
            "route_not_found",
            "route does not exist".into(),
        )).expect("error JSON"),
        "generation": serde_json::to_value(generation).expect("generation JSON"),
        "listeners": serde_json::to_value(listeners).expect("listener JSON"),
        "pools": serde_json::to_value(PoolInventoryResponse::from(pools.clone()))
            .expect("pool JSON"),
        "servers": serde_json::to_value(ServerInventoryResponse::from(pools))
            .expect("server JSON"),
    });

    assert_eq!(
        value["error"],
        json!({
            "error": { "code": "route_not_found", "message": "route does not exist" }
        })
    );
    assert_eq!(
        value["generation"]["generation"],
        json!({
            "buildVersion": "0.5-test",
            "diskRevision": revision('a'),
            "candidateRevision": null,
            "activeRevision": revision('b'),
            "previousRevision": null,
            "quarantinedRevision": null,
            "activeAccepting": true,
            "degraded": false,
            "lastFailure": null,
            "prepares": 1,
            "activations": 2,
            "failures": 3,
            "rollbacks": 4,
        })
    );
    assert_eq!(
        value["listeners"]["listeners"][0],
        json!({
            "administrativeState": "ready",
            "name": "edge",
            "protocol": "http3",
            "bind": "socket:[::]:443",
            "maxConnections": null,
            "state": "listening",
            "acceptedConnections": "5",
            "rejectedConnections": "6",
            "activeConnections": 7,
            "bytesReceived": "8",
            "bytesSent": "9",
            "httpOperations": {
                "outcomes": [{ "result": "success", "count": "10" }],
                "latency": {
                    "buckets": [{ "upperBoundMs": null, "count": "11" }],
                    "count": "12",
                    "sumMs": "13",
                },
            },
            "tcpRelays": {
                "outcomes": [{ "result": "connect_timeout", "count": "40" }],
                "latency": {
                    "buckets": [{ "upperBoundMs": 250, "count": "41" }],
                    "count": "42",
                    "sumMs": "43",
                },
            },
            "proxyProtocol": {
                "outcomes": [{ "result": "accepted", "count": "14" }],
            },
            "cache": { "hits": "15", "misses": "16", "admissions": "17", "evictions": "18" },
        })
    );

    let primary_endpoint = json!({
        "activeConnections": "19",
        "administrativeState": "maintenance",
        "address": "127.0.0.1:8080",
        "checksEnabled": true,
        "checksRunning": false,
        "configuredMaxConnections": null,
        "healthOverride": "down",
        "maxConnections": 25,
        "name": "origin-a",
        "state": "unhealthy",
        "weight": 26,
        "lastCheckedAtUnixMs": null,
        "lastTransitionAtUnixMs": 27,
        "successfulChecks": "28",
        "failedChecks": "29",
        "consecutiveSuccesses": "30",
        "consecutiveFailures": "31",
        "lastFailure": "protocol_error",
        "passiveEjected": true,
        "passiveFailureCount": "32",
        "passiveConsecutiveFailures": "33",
        "passiveEjectionCount": "34",
        "passiveEjectionReason": "timeout",
        "passiveEjectedAtUnixMs": 36,
        "passiveEjectionUntilUnixMs": 37,
        "passiveRecoveryCount": "35",
        "passiveLastRecoveryAtUnixMs": null,
    });
    let primary_second_endpoint = json!({
        "activeConnections": "46",
        "administrativeState": "ready",
        "address": "127.0.0.3:8082",
        "checksEnabled": true,
        "checksRunning": true,
        "configuredMaxConnections": 75,
        "healthOverride": "up",
        "maxConnections": 60,
        "name": "origin-a-second",
        "state": "unknown",
        "weight": 2,
        "lastCheckedAtUnixMs": 47,
        "lastTransitionAtUnixMs": 48,
        "successfulChecks": "49",
        "failedChecks": "50",
        "consecutiveSuccesses": "51",
        "consecutiveFailures": "52",
        "lastFailure": "connect_failed",
        "passiveEjected": false,
        "passiveFailureCount": "53",
        "passiveConsecutiveFailures": "54",
        "passiveEjectionCount": "55",
        "passiveEjectionReason": "unexpected_status",
        "passiveEjectedAtUnixMs": null,
        "passiveEjectionUntilUnixMs": null,
        "passiveRecoveryCount": "56",
        "passiveLastRecoveryAtUnixMs": 57,
    });
    let secondary_endpoint = json!({
        "activeConnections": "0",
        "administrativeState": "ready",
        "address": "127.0.0.2:8081",
        "checksEnabled": false,
        "checksRunning": false,
        "configuredMaxConnections": 50,
        "healthOverride": "auto",
        "maxConnections": null,
        "name": "origin-b",
        "state": "healthy",
        "weight": 1,
        "lastCheckedAtUnixMs": 44,
        "lastTransitionAtUnixMs": null,
        "successfulChecks": "0",
        "failedChecks": "0",
        "consecutiveSuccesses": "0",
        "consecutiveFailures": "0",
        "lastFailure": null,
        "passiveEjected": false,
        "passiveFailureCount": "0",
        "passiveConsecutiveFailures": "0",
        "passiveEjectionCount": "0",
        "passiveEjectionReason": null,
        "passiveEjectedAtUnixMs": null,
        "passiveEjectionUntilUnixMs": null,
        "passiveRecoveryCount": "0",
        "passiveLastRecoveryAtUnixMs": 45,
    });
    assert_eq!(
        value["pools"],
        json!({
            "pools": [
                {
                    "name": "primary",
                    "algorithm": "round_robin",
                    "availableEndpoints": 1,
                    "totalEndpoints": 2,
                    "unavailableSelections": "20",
                    "failOpenSelections": "25",
                    "queued": 21,
                    "queuedTotal": "22",
                    "queueTimeouts": "23",
                    "queueCancellations": "24",
                    "endpoints": [
                        primary_endpoint.clone(),
                        primary_second_endpoint.clone(),
                    ],
                },
                {
                    "name": "secondary",
                    "algorithm": "first",
                    "availableEndpoints": 1,
                    "totalEndpoints": 1,
                    "unavailableSelections": "0",
                    "failOpenSelections": "0",
                    "queued": 0,
                    "queuedTotal": "0",
                    "queueTimeouts": "0",
                    "queueCancellations": "0",
                    "endpoints": [secondary_endpoint.clone()],
                },
            ],
        })
    );
    assert_eq!(
        value["servers"],
        json!({
            "servers": [
                { "pool": "primary", "server": primary_endpoint },
                { "pool": "primary", "server": primary_second_endpoint },
                { "pool": "secondary", "server": secondary_endpoint },
            ],
        })
    );
}

#[test]
fn listener_inventory_preserves_public_snapshot_byte_order() {
    let mut alpha = listener();
    alpha.name = "alpha".into();
    let mut zulu = listener();
    zulu.name = "zulu".into();
    let response = ListenerInventoryResponse::from(runtime_snapshot(vec![alpha, zulu]));
    let bytes = serde_json::to_vec(&response).expect("listener inventory bytes");

    assert!(
        bytes
            .windows(b"alpha".len())
            .position(|value| value == b"alpha")
            < bytes
                .windows(b"zulu".len())
                .position(|value| value == b"zulu")
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn api_enum_projections_are_exhaustive_and_stable() {
    assert_eq!(
        serde_json::to_value([
            AdministrativeStateDto::from(AdministrativeState::Ready),
            AdministrativeStateDto::from(AdministrativeState::Drain),
            AdministrativeStateDto::from(AdministrativeState::Maintenance),
        ])
        .expect("administrative state JSON"),
        json!(["ready", "drain", "maintenance"])
    );
    assert_eq!(
        serde_json::to_value([
            ListenerRuntimeStateDto::from(ListenerRuntimeState::Configured),
            ListenerRuntimeStateDto::from(ListenerRuntimeState::Listening),
            ListenerRuntimeStateDto::from(ListenerRuntimeState::Stopped),
            ListenerRuntimeStateDto::from(ListenerRuntimeState::Failed),
        ])
        .expect("listener state JSON"),
        json!(["configured", "listening", "stopped", "failed"])
    );
    assert_eq!(
        serde_json::to_value([
            EndpointHealthStateDto::from(EndpointHealthState::Unchecked),
            EndpointHealthStateDto::from(EndpointHealthState::Unknown),
            EndpointHealthStateDto::from(EndpointHealthState::Healthy),
            EndpointHealthStateDto::from(EndpointHealthState::Unhealthy),
        ])
        .expect("endpoint state JSON"),
        json!(["unchecked", "unknown", "healthy", "unhealthy"])
    );
    assert_eq!(
        serde_json::to_value([
            HealthFailureDto::from(HealthFailure::Timeout),
            HealthFailureDto::from(HealthFailure::ConnectFailed),
            HealthFailureDto::from(HealthFailure::UnexpectedStatus),
            HealthFailureDto::from(HealthFailure::ProtocolError),
        ])
        .expect("health failure JSON"),
        json!([
            "timeout",
            "connect_failed",
            "unexpected_status",
            "protocol_error"
        ])
    );
    assert_eq!(
        serde_json::to_value([
            HealthOverrideDto::from(HealthOverride::Auto),
            HealthOverrideDto::from(HealthOverride::Up),
            HealthOverrideDto::from(HealthOverride::Down),
        ])
        .expect("health override JSON"),
        json!(["auto", "up", "down"])
    );
    assert_eq!(
        serde_json::to_value([
            HttpOperationResultDto::from(HttpOperationResult::Success),
            HttpOperationResultDto::from(HttpOperationResult::ClientError),
            HttpOperationResultDto::from(HttpOperationResult::ServerError),
            HttpOperationResultDto::from(HttpOperationResult::UpstreamError),
            HttpOperationResultDto::from(HttpOperationResult::Timeout),
            HttpOperationResultDto::from(HttpOperationResult::Cancelled),
            HttpOperationResultDto::from(HttpOperationResult::InternalError),
        ])
        .expect("HTTP result JSON"),
        json!([
            "success",
            "client_error",
            "server_error",
            "upstream_error",
            "timeout",
            "cancelled",
            "internal_error"
        ])
    );
    assert_eq!(
        serde_json::to_value([
            TcpRelayResultDto::from(TcpRelayResult::Success),
            TcpRelayResultDto::from(TcpRelayResult::ConnectError),
            TcpRelayResultDto::from(TcpRelayResult::ConnectTimeout),
            TcpRelayResultDto::from(TcpRelayResult::IdleTimeout),
            TcpRelayResultDto::from(TcpRelayResult::LifetimeTimeout),
            TcpRelayResultDto::from(TcpRelayResult::Cancelled),
            TcpRelayResultDto::from(TcpRelayResult::IoError),
            TcpRelayResultDto::from(TcpRelayResult::AccountingError),
            TcpRelayResultDto::from(TcpRelayResult::ProxyProtocolError),
        ])
        .expect("TCP result JSON"),
        json!([
            "success",
            "connect_error",
            "connect_timeout",
            "idle_timeout",
            "lifetime_timeout",
            "cancelled",
            "io_error",
            "accounting_error",
            "proxy_protocol_error"
        ])
    );
    assert_eq!(
        serde_json::to_value([
            ProxyProtocolResultDto::from(ProxyProtocolResult::Accepted),
            ProxyProtocolResultDto::from(ProxyProtocolResult::Sent),
            ProxyProtocolResultDto::from(ProxyProtocolResult::Timeout),
            ProxyProtocolResultDto::from(ProxyProtocolResult::Cancelled),
            ProxyProtocolResultDto::from(ProxyProtocolResult::Malformed),
            ProxyProtocolResultDto::from(ProxyProtocolResult::Unsupported),
            ProxyProtocolResultDto::from(ProxyProtocolResult::Mismatch),
            ProxyProtocolResultDto::from(ProxyProtocolResult::IoError),
        ])
        .expect("PROXY result JSON"),
        json!([
            "accepted",
            "sent",
            "timeout",
            "cancelled",
            "malformed",
            "unsupported",
            "mismatch",
            "io_error"
        ])
    );
}

#[test]
fn listener_inventory_is_an_explicit_secret_safe_allowlist() {
    let snapshot = runtime_snapshot(vec![listener()]);
    let response = ListenerInventoryResponse::from(snapshot);
    let json = serde_json::to_string(&response).expect("listener inventory JSON");

    assert!(json.contains(r#""maxConnections":null"#));
    assert!(json.contains(r#""upperBoundMs":null"#));
    assert!(!json.contains("private-key-secret"));
    assert!(!json.contains("session-secret"));
}

fn generation() -> GenerationStatus {
    GenerationStatus {
        build_version: "0.5-test",
        disk_revision: Some(authored_revision_value('a')),
        candidate_revision: None,
        active_revision: Some(effective_revision_value('b')),
        previous_revision: None,
        quarantined_revision: None,
        active_accepting: true,
        degraded: false,
        last_failure: None,
        prepares: 1,
        activations: 2,
        failures: 3,
        rollbacks: 4,
    }
}

fn listener() -> ListenerSnapshot {
    ListenerSnapshot {
        administrative_state: AdministrativeState::Ready,
        name: "edge".into(),
        protocol: "http3".into(),
        bind: "socket:[::]:443".into(),
        max_connections: None,
        state: ListenerRuntimeState::Listening,
        accepted_connections: 5,
        rejected_connections: 6,
        active_connections: 7,
        bytes_received: 8,
        bytes_sent: 9,
        http_operations: Some(HttpOperationSnapshot {
            outcomes: Box::new([HttpOperationCountSnapshot {
                result: HttpOperationResult::Success,
                count: 10,
            }]),
            latency: LatencySnapshot {
                buckets: Box::new([LatencyBucketSnapshot {
                    upper_bound_ms: None,
                    count: 11,
                }]),
                count: 12,
                sum_ms: 13,
            },
        }),
        tcp_relays: Some(TcpRelaySnapshot {
            outcomes: Box::new([TcpRelayCountSnapshot {
                result: TcpRelayResult::ConnectTimeout,
                count: 40,
            }]),
            latency: LatencySnapshot {
                buckets: Box::new([LatencyBucketSnapshot {
                    upper_bound_ms: Some(250),
                    count: 41,
                }]),
                count: 42,
                sum_ms: 43,
            },
        }),
        proxy_protocol: Some(ProxyProtocolSnapshot {
            outcomes: Box::new([ProxyProtocolCountSnapshot {
                result: ProxyProtocolResult::Accepted,
                count: 14,
            }]),
        }),
        cache: Some(CacheSnapshot {
            hits: 15,
            misses: 16,
            admissions: 17,
            evictions: 18,
        }),
    }
}

fn pool() -> PoolHealthSnapshot {
    PoolHealthSnapshot {
        name: "primary".into(),
        algorithm: "round_robin",
        available_endpoints: 1,
        total_endpoints: 2,
        unavailable_selections: 20,
        fail_open_selections: 25,
        queued: 21,
        queued_total: 22,
        queue_timeouts: 23,
        queue_cancellations: 24,
        endpoints: vec![
            EndpointHealthSnapshot {
                active_connections: 19,
                administrative_state: AdministrativeState::Maintenance,
                address: RuntimeEndpoint::from(
                    "127.0.0.1:8080"
                        .parse::<std::net::SocketAddr>()
                        .expect("endpoint"),
                ),
                checks_enabled: true,
                checks_running: false,
                configured_max_connections: None,
                health_override: HealthOverride::Down,
                max_connections: Some(25),
                name: "origin-a".into(),
                state: EndpointHealthState::Unhealthy,
                weight: 26,
                last_checked_at_unix_ms: None,
                last_transition_at_unix_ms: Some(27),
                successful_checks: 28,
                failed_checks: 29,
                consecutive_successes: 30,
                consecutive_failures: 31,
                last_failure: Some(HealthFailure::ProtocolError),
                passive_ejected: true,
                passive_failure_count: 32,
                passive_consecutive_failures: 33,
                passive_ejection_count: 34,
                passive_ejection_reason: Some(HealthFailure::Timeout),
                passive_ejected_at_unix_ms: Some(36),
                passive_ejection_until_unix_ms: Some(37),
                passive_recovery_count: 35,
                passive_last_recovery_at_unix_ms: None,
            },
            EndpointHealthSnapshot {
                active_connections: 46,
                administrative_state: AdministrativeState::Ready,
                address: RuntimeEndpoint::from(
                    "127.0.0.3:8082"
                        .parse::<std::net::SocketAddr>()
                        .expect("endpoint"),
                ),
                checks_enabled: true,
                checks_running: true,
                configured_max_connections: Some(75),
                health_override: HealthOverride::Up,
                max_connections: Some(60),
                name: "origin-a-second".into(),
                state: EndpointHealthState::Unknown,
                weight: 2,
                last_checked_at_unix_ms: Some(47),
                last_transition_at_unix_ms: Some(48),
                successful_checks: 49,
                failed_checks: 50,
                consecutive_successes: 51,
                consecutive_failures: 52,
                last_failure: Some(HealthFailure::ConnectFailed),
                passive_ejected: false,
                passive_failure_count: 53,
                passive_consecutive_failures: 54,
                passive_ejection_count: 55,
                passive_ejection_reason: Some(HealthFailure::UnexpectedStatus),
                passive_ejected_at_unix_ms: None,
                passive_ejection_until_unix_ms: None,
                passive_recovery_count: 56,
                passive_last_recovery_at_unix_ms: Some(57),
            },
        ],
    }
}

fn secondary_pool() -> PoolHealthSnapshot {
    PoolHealthSnapshot {
        name: "secondary".into(),
        algorithm: "first",
        available_endpoints: 1,
        total_endpoints: 1,
        unavailable_selections: 0,
        fail_open_selections: 0,
        queued: 0,
        queued_total: 0,
        queue_timeouts: 0,
        queue_cancellations: 0,
        endpoints: vec![EndpointHealthSnapshot {
            active_connections: 0,
            administrative_state: AdministrativeState::Ready,
            address: RuntimeEndpoint::from(
                "127.0.0.2:8081"
                    .parse::<std::net::SocketAddr>()
                    .expect("endpoint"),
            ),
            checks_enabled: false,
            checks_running: false,
            configured_max_connections: Some(50),
            health_override: HealthOverride::Auto,
            max_connections: None,
            name: "origin-b".into(),
            state: EndpointHealthState::Healthy,
            weight: 1,
            last_checked_at_unix_ms: Some(44),
            last_transition_at_unix_ms: None,
            successful_checks: 0,
            failed_checks: 0,
            consecutive_successes: 0,
            consecutive_failures: 0,
            last_failure: None,
            passive_ejected: false,
            passive_failure_count: 0,
            passive_consecutive_failures: 0,
            passive_ejection_count: 0,
            passive_ejection_reason: None,
            passive_ejected_at_unix_ms: None,
            passive_ejection_until_unix_ms: None,
            passive_recovery_count: 0,
            passive_last_recovery_at_unix_ms: Some(45),
        }],
    }
}

fn runtime_snapshot(listeners: Vec<ListenerSnapshot>) -> RuntimeSnapshot {
    RuntimeSnapshot {
        sampled_at_unix_ms: 1,
        uptime_ms: 2,
        generation_age_ms: 3,
        process: ProcessSnapshot {
            active_connections: 0,
            administrative_state: AdministrativeState::Ready,
            status: ComponentStatus {
                state: ComponentState::Healthy,
                reason: None,
            },
            cpu_percent: None,
            max_connections: None,
            rejected_connections: 0,
            retry_attempts: 0,
            rescues: Vec::new(),
            resident_memory_bytes: None,
            virtual_memory_bytes: None,
            thread_count: None,
            open_file_descriptors: None,
        },
        host: HostSnapshot {
            status: ComponentStatus {
                state: ComponentState::Healthy,
                reason: None,
            },
            load_average_1m: None,
            load_average_5m: None,
            load_average_15m: None,
            total_memory_bytes: None,
            available_memory_bytes: None,
        },
        traffic: TrafficSnapshot::default(),
        listeners,
        upstream_pools: Vec::new(),
        transport_operations: Vec::new(),
        access_records: Vec::new(),
        certbot_certificates: Vec::new(),
        certbot_watcher: None,
        acme_managed_certificates: vec![AcmeManagedCertificateSnapshot {
            name: "private-key-secret".into(),
            directory_url: "session-secret".into(),
            disk_revision: "private-key-secret".into(),
            active_revision: "session-secret".into(),
            expires_at: "private-key-secret".into(),
            not_before_unix_seconds: None,
            not_after_unix_seconds: None,
            next_action_unix_seconds: None,
            last_outcome: Some("session-secret".into()),
            last_error_code: Some("private-key-secret".into()),
            renewal_information_status: "session-secret".into(),
            dns_provider: Some("private-key-secret".into()),
            dns_provider_deployment: Some("session-secret".into()),
            dns_provider_health: Some("private-key-secret".into()),
            dns_cleanup_status: "session-secret".into(),
        }],
        direct_file_certificates: Vec::new(),
        direct_file_watcher: None,
    }
}

fn authored_revision_value(value: char) -> AuthoredRevision {
    AuthoredRevision::from_str(&revision(value)).expect("revision")
}

fn effective_revision_value(value: char) -> EffectiveRevision {
    EffectiveRevision::from_str(&revision(value)).expect("revision")
}

fn revision(value: char) -> String {
    std::iter::repeat_n(value, 64).collect()
}
