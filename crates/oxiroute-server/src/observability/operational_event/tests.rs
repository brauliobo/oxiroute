use std::{
    fs::{self, OpenOptions},
    os::unix::fs::PermissionsExt,
};

use tempfile::tempdir;

use super::*;

fn audit_limits() -> AuditLimits {
    AuditLimits {
        max_records: 2,
        max_record_bytes: 4 * 1024,
        max_file_bytes: 8 * 1024,
        max_total_bytes: 32 * 1024,
        max_rotated_files: 2,
    }
}

fn audit_directory() -> tempfile::TempDir {
    let directory = tempdir().expect("audit directory");
    fs::set_permissions(
        directory.path(),
        fs::Permissions::from_mode(AUDIT_ROOT_MODE),
    )
    .expect("secure audit directory");
    directory
}

#[test]
fn external_correlation_ids_are_bounded_and_safe() {
    assert!(AuditContext::from_external("request-42").is_some());
    assert!(AuditContext::from_external(&"x".repeat(65)).is_none());
    assert!(AuditContext::from_external("request id").is_none());
    assert!(AuditContext::from_external("Authorization: Bearer secret").is_none());
}

#[test]
fn durable_audit_records_round_trip_with_filters_and_retention() {
    let directory = audit_directory();
    let limits = audit_limits();
    let context = AuditContext::from_external("request-42").expect("safe context");
    let store = AuditStore::open(directory.path(), limits).expect("open audit store");
    store
        .append(
            &context,
            AuditCategory::Reload,
            "generation_reload",
            AuditResult::Succeeded,
            None,
        )
        .expect("reload record");
    store
        .append(
            &context,
            AuditCategory::Control,
            "listener_control",
            AuditResult::Rejected,
            None,
        )
        .expect("control record");
    store
        .append(
            &context,
            AuditCategory::Certificate,
            "certificate_renew",
            AuditResult::Requested,
            None,
        )
        .expect("certificate record");

    let page = store.page(0, 10, Some(AuditCategory::Certificate), None);
    assert_eq!(page.records.len(), 1);
    assert_eq!(page.records[0].operation, "certificate_renew");
    assert_eq!(store.status().record_count, 2);
    drop(store);

    let reopened = AuditStore::open(directory.path(), limits).expect("reopen audit store");
    let page = reopened.page(0, 10, None, None);
    assert_eq!(page.records.len(), 2);
    assert_eq!(page.records[0].id, 2);
    assert_eq!(page.records[1].id, 3);
    assert_eq!(page.records[0].actor, "management_bearer");
    assert_eq!(page.records[0].source, "management_api");
}

#[test]
fn audit_operation_totals_remain_monotonic_after_retention() {
    let store = AuditStore::memory(audit_limits());
    let context = AuditContext::generated();

    for _ in 0..2 {
        store
            .append(
                &context,
                AuditCategory::Reload,
                "generation_reload",
                AuditResult::Requested,
                None,
            )
            .expect("reload record");
    }
    store
        .append(
            &context,
            AuditCategory::Control,
            "process_shutdown",
            AuditResult::Rejected,
            None,
        )
        .expect("control record");

    let metrics = store.metric_snapshot();
    assert_eq!(metrics.status.record_count, 2);
    assert_eq!(
        metrics.operation_counts[AuditCategory::Reload.index()][AuditResult::Requested.index()],
        2
    );
    assert_eq!(
        metrics.operation_counts[AuditCategory::Control.index()][AuditResult::Rejected.index()],
        1
    );
}

#[test]
fn corrupt_audit_lines_are_skipped_and_reported_as_degraded() {
    let directory = audit_directory();
    let limits = audit_limits();
    let context = AuditContext::generated();
    let store = AuditStore::open(directory.path(), limits).expect("open audit store");
    store
        .append(
            &context,
            AuditCategory::Control,
            "process_drain",
            AuditResult::Requested,
            None,
        )
        .expect("audit record");
    drop(store);

    let mut file = OpenOptions::new()
        .append(true)
        .open(directory.path().join(AUDIT_ACTIVE_FILE))
        .expect("active audit file");
    file.write_all(b"not-json\n").expect("corrupt line");
    drop(file);

    let reopened = AuditStore::open(directory.path(), limits).expect("recover audit store");
    let status = reopened.status();
    assert!(status.degraded);
    assert_eq!(status.corrupt_records, 1);
    assert_eq!(reopened.page(0, 10, None, None).records.len(), 1);
}

#[test]
fn bounded_pages_advance_to_the_last_returned_event() {
    let mut log = EventLog::default();
    for cursor in 1..=5 {
        log.events
            .push_back(OperationalEvent::new(cursor, None, "test", "ok", None));
        log.next_cursor = cursor;
    }

    let first = log.page(0, 2);
    assert_eq!(
        first
            .events
            .iter()
            .map(|event| event.cursor)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(first.cursor, 2);
    assert!(first.has_more);
    assert_eq!(first.oldest_cursor, Some(1));
    assert!(!first.cursor_lost);

    let second = log.page(first.cursor, 2);
    assert_eq!(
        second
            .events
            .iter()
            .map(|event| event.cursor)
            .collect::<Vec<_>>(),
        [3, 4]
    );
    assert_eq!(second.cursor, 4);
    assert!(second.has_more);
    assert_eq!(second.latest_cursor, 5);
}

#[test]
fn an_initial_page_uses_the_current_cursor_without_replay() {
    let mut log = EventLog::default();
    for cursor in 1..=5 {
        log.events
            .push_back(OperationalEvent::new(cursor, None, "test", "ok", None));
        log.next_cursor = cursor;
    }

    let page = log.page(log.next_cursor, 64);
    assert!(page.events.is_empty());
    assert_eq!(page.cursor, 5);
    assert_eq!(page.latest_cursor, 5);
    assert!(!page.cursor_lost);
}

#[test]
fn reports_cursor_loss_only_after_the_last_evicted_cursor() {
    let mut log = EventLog::default();
    for cursor in 1..=(EVENT_CAPACITY as u64 + 2) {
        log.events
            .push_back(OperationalEvent::new(cursor, None, "test", "ok", None));
        if log.events.len() > EVENT_CAPACITY {
            log.events.pop_front();
        }
        log.next_cursor = cursor;
    }

    assert_eq!(log.events.front().map(|event| event.cursor), Some(3));
    assert!(log.page(1, 10).cursor_lost);
    assert!(!log.page(2, 10).cursor_lost);
    assert_eq!(log.page(2, 10).events[0].cursor, 3);
}

#[test]
fn unknown_event_values_are_serialized_as_safe_typed_values() {
    let event = OperationalEvent::new(
        1,
        None,
        "Authorization: Bearer private-key-secret",
        "Cookie=session-secret",
        None,
    );
    let json = serde_json::to_string(&event).expect("event JSON");

    assert!(json.contains(r#""event":"unknown""#));
    assert!(json.contains(r#""outcome":"unknown""#));
    assert!(!json.contains("private-key-secret"));
    assert!(!json.contains("session-secret"));
}

#[test]
fn certificate_operation_event_names_round_trip_in_the_certificate_category() {
    for name in [
        "certificate_revocation",
        "certificate_deletion",
        "certificate_account_rollover",
        "certificate_job_control",
    ] {
        let event = OperationalEvent::new(1, None, name, "requested", None);

        assert_eq!(event.event.as_str(), name);
        assert_eq!(category_for_event(event.event), AuditCategory::Certificate);
    }
}

#[test]
fn default_serialization_preserves_persisted_version_one_event_names() {
    let mut event = OperationalEvent {
        cursor: 1,
        timestamp_unix_ms: None,
        event: EventName::CertificateActivation,
        outcome: EventOutcome::Activated,
        revision: None,
        certificate: Some("edge".into()),
        correlation_id: None,
        actor: None,
        source: None,
        operation: None,
    };

    assert_eq!(
        serde_json::to_string(&event).expect("event JSON"),
        r#"{"cursor":1,"timestampUnixMs":null,"event":"certificate_activation","outcome":"activated","revision":null,"certificate":"edge"}"#
    );

    event.event = EventName::CertificateRevocation;
    event.outcome = EventOutcome::Requested;
    assert_eq!(
        serde_json::to_string(&event).expect("event JSON"),
        r#"{"cursor":1,"timestampUnixMs":null,"event":"unknown","outcome":"requested","revision":null,"certificate":"edge"}"#
    );
}

#[test]
fn version_one_projection_preserves_shipped_event_names() {
    let mut event = OperationalEvent {
        cursor: 1,
        timestamp_unix_ms: None,
        event: EventName::CertificateActivation,
        outcome: EventOutcome::Activated,
        revision: None,
        certificate: Some("edge".into()),
        correlation_id: None,
        actor: None,
        source: None,
        operation: None,
    };

    assert_eq!(v1_event_value(&event)["event"], "certificate_activation");
    event.event = EventName::CertificateRevocation;
    event.outcome = EventOutcome::Requested;
    assert_eq!(v1_event_value(&event)["event"], "unknown");
    assert_eq!(v1_event_value(&event)["outcome"], "requested");
}

#[test]
fn version_two_projection_uses_the_authoritative_event_names() {
    let mut event = OperationalEvent {
        cursor: 1,
        timestamp_unix_ms: None,
        event: EventName::CertificateActivation,
        outcome: EventOutcome::Activated,
        revision: None,
        certificate: Some("edge".into()),
        correlation_id: None,
        actor: None,
        source: None,
        operation: None,
    };

    assert_eq!(v2_event_value(&event)["event"], "certificate_activated");
    event.event = EventName::CertificateRevocation;
    assert_eq!(v2_event_value(&event)["event"], "certificate_revocation");
}

#[test]
fn passive_endpoint_events_serialize_bounded_recovery_details() {
    let ejection = OperationalEvent {
        cursor: 1,
        timestamp_unix_ms: Some(10),
        event: EventName::UpstreamEndpointEjection,
        outcome: EventOutcome::Ejected {
            pool: "backend".into(),
            server: "primary".into(),
            reason: HealthFailure::ConnectFailed,
            failure_count: 3,
            ejection_count: 2,
            ejected_at_unix_ms: 10,
            ejection_until_unix_ms: 20,
        },
        revision: None,
        certificate: None,
        correlation_id: None,
        actor: None,
        source: None,
        operation: None,
    };
    let recovery = OperationalEvent {
        cursor: 2,
        timestamp_unix_ms: Some(30),
        event: EventName::UpstreamEndpointRecovery,
        outcome: EventOutcome::Recovered {
            pool: "backend".into(),
            server: "primary".into(),
            reason: Some(HealthFailure::ConnectFailed),
            recovery_count: 1,
            recovered_at_unix_ms: 30,
        },
        revision: None,
        certificate: None,
        correlation_id: None,
        actor: None,
        source: None,
        operation: None,
    };

    let ejection = serde_json::to_value(ejection).expect("ejection event JSON");
    let recovery = serde_json::to_value(recovery).expect("recovery event JSON");
    assert_eq!(ejection["outcome"]["type"], "ejected");
    assert_eq!(ejection["outcome"]["reason"], "connect_failed");
    assert_eq!(ejection["outcome"]["ejectionCount"], 2);
    assert_eq!(recovery["outcome"]["type"], "recovered");
    assert_eq!(recovery["outcome"]["recoveryCount"], 1);
}

#[test]
fn health_changes_serialize_both_states_and_the_failure() {
    let event = OperationalEvent {
        cursor: 1,
        timestamp_unix_ms: Some(10),
        event: EventName::UpstreamEndpointHealth,
        outcome: EventOutcome::HealthChanged {
            pool: "backend".into(),
            server: "primary".into(),
            previous: EndpointHealthState::Healthy,
            next: EndpointHealthState::Unhealthy,
            reason: Some(HealthFailure::UnexpectedStatus),
            changed_at_unix_ms: 10,
        },
        revision: None,
        certificate: None,
        correlation_id: None,
        actor: None,
        source: None,
        operation: None,
    };

    let value = v2_event_value(&event);
    assert_eq!(value["event"], "upstream_endpoint_health");
    assert_eq!(
        value["outcome"],
        serde_json::json!({
            "type": "health_changed",
            "pool": "backend",
            "server": "primary",
            "previous": "healthy",
            "next": "unhealthy",
            "reason": "unexpected_status",
            "changedAtUnixMs": 10,
        })
    );
    assert_eq!(v1_event_value(&event)["event"], "unknown");
    assert_eq!(event.event.as_str(), "upstream_endpoint_health");
}

fn evacuation_event(server: Option<&str>) -> OperationalEvent {
    let mut report = crate::EvacuationReport {
        redispatched: 3,
        forced: 1,
        ..crate::EvacuationReport::default()
    };
    report
        .left_running
        .insert(crate::LeftRunning::ResponseStarted, 2);
    OperationalEvent {
        cursor: 1,
        timestamp_unix_ms: Some(10),
        event: EventName::ServerEvacuation,
        outcome: EventOutcome::Evacuated {
            pool: "gpu-render".into(),
            server: server.map(Into::into),
            force: true,
            report,
        },
        revision: None,
        certificate: None,
        correlation_id: None,
        actor: None,
        source: None,
        operation: None,
    }
}

#[test]
fn evacuations_serialize_the_counts_and_name_the_server_only_when_there_is_one() {
    let server = v2_event_value(&evacuation_event(Some("node-a")));
    let pool_event = evacuation_event(None);
    let pool = v2_event_value(&pool_event);

    assert_eq!(server["event"], "server_evacuation");
    assert_eq!(
        server["outcome"],
        serde_json::json!({
            "type": "evacuated",
            "pool": "gpu-render",
            "server": "node-a",
            "force": true,
            "cancelledAndRedispatched": 3,
            "forced": 1,
            "leftRunning": { "response_started": 2 },
        })
    );
    assert!(pool["outcome"].get("server").is_none());
    assert_eq!(v1_event_value(&pool_event)["event"], "unknown");
    assert_eq!(
        EventName::parse("server_evacuation").as_str(),
        "server_evacuation"
    );
}

#[test]
fn an_emitted_evacuation_is_one_event_in_the_log() {
    let report = crate::EvacuationReport::default();

    emit_server_evacuation(
        &AuditContext::generated(),
        "gpu-render",
        Some("node-a"),
        false,
        &report,
    );

    assert_eq!(thread_events::count("server_evacuation"), 1);
}
